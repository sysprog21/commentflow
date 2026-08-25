use crate::classify::{DocFlavor, Kind, Style};
pub use crate::linekind::LineKind;
use crate::linekind::{StrippedLine, classify_lines, doxy_tag};
use crate::parse::{Comment, Language};
use crate::textline::{
    BookendKind, FAST_PATH_TAB_WIDTH, advance_col, block_is_doc, bookend_match, line_is_art_only,
    one_sided_banner, split_at_return_boundary,
};

#[derive(Debug, Clone)]
pub struct Line {
    pub kind: LineKind,
    pub text: String,
    pub had_crlf: bool,
    /// The raw source line BEFORE any prefix stripping. Used by Preformatted
    /// emission to replay the original bytes verbatim, so fences, art,
    /// tables, and indented code never drift through normalize→reflow.
    pub raw: String,
}

#[derive(Debug, Clone)]
pub struct NormalizedDoc {
    pub lines: Vec<Line>,
    pub indent: String,
    pub paragraphs: Vec<Paragraph>,
    pub style: Style,
    pub flavor: DocFlavor,
    pub is_file_header: bool,
    pub default_continuation_prefix: String,
    pub line_marker: Option<String>,
    pub block_opener_marker: Option<String>,
    /// Ending used by "join_lines" when the comment has no interior break to
    /// vote on (a single source line that reflow wraps). Carried verbatim
    /// from the source "Comment".
    pub fallback_ending: &'static str,
    /// True when this comment was a well-formed X11 manual-page block
    /// ("DESCRIPTION" + "RETURN(S)" sections) sitting between a function's
    /// signature and body, and the section headers were stripped. "plan"
    /// reads it to hoist the reflowed comment ahead of the function instead
    /// of rewriting it in place.
    pub manpage: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParagraphKind {
    Prose,
    AtxHeader,
    Preformatted,
}

#[derive(Debug, Clone)]
pub struct Paragraph {
    pub kind: ParagraphKind,
    pub line_indices: Vec<usize>,
    pub preceded_by_blank: bool,
}

pub fn normalize(
    c: &Comment,
    kind: &Kind,
    lang: Language,
    column_limit: usize,
) -> Option<NormalizedDoc> {
    if c.force_passthrough {
        return None;
    }

    if is_single_line_short(c, kind, column_limit) {
        return None;
    }

    let mut indent = c.line_indent_bytes.clone();
    let raw_lines = split_lines(&c.text);
    let mut stripped = strip_prefixes(&raw_lines, kind.style);
    strip_decorative_bookends(&mut stripped, kind);

    // A comment that is nothing but a one-sided banner passes through. Inside a
    // larger comment "LineKind::Banner" freezes such a line and that is enough,
    // but a single-line BLOCK also has an opener and a closer to place, and
    // reshaping those around a body the emitter must not touch yields "/*"
    // alone above a body still carrying its own "*/". There is nothing to
    // rewrite here in the first place, so leave the bytes exactly as found.
    if stripped.len() == 1 && one_sided_banner(&stripped[0].body) {
        return None;
    }

    let line_marker = source_line_marker(&c.text);
    let block_opener_marker = source_block_opener_marker(&c.text).map(str::to_string);
    let default_continuation_prefix = pick_default_continuation_prefix(&stripped);

    // Width left for a comment body line, matching reflow's effective width.
    // Only the label-run rule uses it, to refuse freezing a line that does not
    // already fit.
    let label_budget = column_limit.saturating_sub(
        indent
            .chars()
            .chain(default_continuation_prefix.chars())
            .fold(0usize, |col, c| advance_col(col, c, FAST_PATH_TAB_WIDTH)),
    );
    let kinds = classify_lines(&stripped, kind.flavor, label_budget);
    let lines: Vec<Line> = stripped
        .into_iter()
        .zip(raw_lines)
        .zip(&kinds)
        .map(|((stripped, raw), k)| Line {
            kind: *k,
            text: stripped.body,
            had_crlf: stripped.had_crlf,
            raw: raw.body,
        })
        .collect();

    // A comment wedged between a function signature and its body is a
    // relocation candidate. Try the normal line-based manual-page conversion
    // before the trailing-comment bail below, because these blocks are
    // routinely glued after the ")" and relocation moves the comment to its own
    // line.
    let manpage_lines = if c.relocate_before.is_some()
        && matches!(lang, Language::C | Language::Cpp)
        && matches!(kind.style, Style::Block | Style::DocBlock)
    {
        convert_manpage_sections(&lines)
            .or_else(|| convert_mangled_manpage_sections(&c.text, c.text.contains("\r\n")))
    } else {
        None
    };

    // Trailing-comment policy: a trailing comment either fits on its line
    // (caught by the fast path above) or passes through unchanged. Splitting
    // one across lines would take continuation-indent decisions about the next
    // source line, which is out of scope. The exception is a manual-page block,
    // which is relocated rather than split in place.
    if c.is_trailing && manpage_lines.is_none() {
        return None;
    }

    // A well-formed manual-page block relocates; otherwise fall back to the
    // normal kernel-doc pass (and it stays put: plan only moves comments whose
    // "manpage" fired).
    let (lines, manpage) = match manpage_lines {
        Some(converted) => (converted, true),
        None => (
            convert_kernel_doc(split_doxygen_return_boundaries(lines, lang), lang),
            false,
        ),
    };

    // A relocated manual-page comment is spliced at column 0 (see
    // "Comment::relocate_before"), so it must reflow with zero indent.
    // Otherwise the opener sits at column 0 while continuation lines keep the
    // old between-body indent, which misaligns the block and breaks
    // idempotency. No-op for top-level functions, which are already at column
    // 0.
    if manpage {
        indent.clear();
    }
    let paragraphs = group_paragraphs(&lines);

    // File-header (opener-alone) detection. Plain "/*" blocks are
    // position-based: a block at file offset 0 (after an optional BOM) uses the
    // opener-alone form, every other plain block uses the inline form. Doc
    // blocks ("/**", "/*!") keep the author's source opener shape: their opener
    // marker is semantic (Doxygen/Rustdoc), not a file banner, so file position
    // must not reshape it.
    let is_file_header = match kind.style {
        // A relocated manual-page comment adopts the file-header (opener-alone)
        // form iff it will land at file start (offset 0), matching what the
        // next pass sees once it sits there. Without this, a function at byte 0
        // relocates its comment to byte 0 and the block flips from inline to
        // opener-alone on the re-run, breaking idempotency.
        Style::Block if manpage => c.relocate_before == Some(0),
        Style::Block => c.at_file_start,
        Style::DocBlock => matches!(c.style_opener_alone(), Some(true)),
        _ => false,
    };

    Some(NormalizedDoc {
        lines,
        indent,
        paragraphs,
        style: kind.style,
        flavor: kind.flavor,
        is_file_header,
        default_continuation_prefix,
        line_marker,
        block_opener_marker,
        fallback_ending: c.fallback_ending,
        manpage,
    })
}

/// Trailing block comments pass through reflow untouched (see the policy in
/// "normalize"), but a multi-line one whose closing "*/" is glued to the last
/// content line still reads badly, and clang-format won't move it. This is the
/// one layout fix applied to trailing blocks: split a glued "*/" onto its own
/// line, reusing the last line's indentation (which lands "*/" under the
/// continuation "*" marker when one is present, else under the content).
/// Returns the comment text, or "None" when there is nothing to split
/// (single-line, "*/" already alone, or no content before the closer).
pub fn split_trailing_block_closer(text: &str) -> Option<String> {
    let nl = text.rfind('\n')?;
    let (prefix, last_line) = (&text[..=nl], &text[nl + 1..]);

    let head = last_line.trim_end();
    let before = head.strip_suffix("*/")?.trim_end();

    // Nothing but leading whitespace and the continuation "*" before the closer
    // means "*/" is effectively already on its own line: leave it.
    if before
        .trim_start()
        .trim_start_matches('*')
        .trim()
        .is_empty()
    {
        return None;
    }

    let indent: String = last_line
        .chars()
        .take_while(|&c| c == ' ' || c == '\t')
        .collect();
    let eol = if prefix.ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    Some(format!("{prefix}{before}{eol}{indent}*/"))
}

fn source_line_marker(text: &str) -> Option<String> {
    if text.starts_with("///") {
        Some("///".to_string())
    } else if text.starts_with("//!") {
        Some("//!".to_string())
    } else if text.starts_with("//") {
        Some("//".to_string())
    } else if text.starts_with("#!") {
        // File-leading shebangs are filtered at extraction, so anything
        // reaching here is a mid-file "#!", a regular comment per kernel
        // exec(2). Keep the "#!" as the marker so reflow round-trips it
        // verbatim.
        Some("#!".to_string())
    } else if text.starts_with('#') {
        // Preserve the full leading "#"-run as the marker so "## foo" stays "##
        // foo" rather than collapsing to "# # foo".
        let run: String = text.chars().take_while(|&c| c == '#').collect();
        Some(run)
    } else {
        None
    }
}

/// True for the BSD license opener "/*-", false for a "/*---" banner rule.
/// Claiming the longer run as a marker splits it: the emitted opener takes one
/// dash and the body keeps "--", which is no longer a bookend, so the strip
/// never fires and "/*--- end x ---*/" comes back as "/*- -- end x --- */".
fn is_bsd_dash_opener(text: &str) -> bool {
    text.starts_with("/*-") && !text.starts_with("/*--")
}

fn source_block_opener_marker(text: &str) -> Option<&'static str> {
    if block_is_doc(text) {
        // "/***..." (a star banner) is not a "/**" doc opener. block_is_doc
        // already rejected it, so reaching here means a real doc marker.
        if text.starts_with("/*!") {
            Some("/*!")
        } else {
            Some("/**")
        }
    } else if is_bsd_dash_opener(text) {
        Some("/*-")
    } else if text.starts_with("/*") {
        Some("/*")
    } else {
        None
    }
}

fn is_single_line_short(c: &Comment, kind: &Kind, column_limit: usize) -> bool {
    let body = c.text.trim_end_matches(['\r', '\n']);
    if body.contains('\n') {
        return false;
    }

    // Single-line decorative bookends must be normalized even when they fit the
    // column budget, otherwise the strip pass never gets a chance to collapse
    // them. Rustdoc bare bookends keep the fast path because a standalone "///
    // =====" is indistinguishable from a setext underline at this layer.
    if single_line_has_strippable_bookend(body, kind) {
        return false;
    }

    // Use display width with tab-stop advancement: tab moves the column to the
    // next multiple of TAB_WIDTH (default 8), not always +8. We don't have
    // IndentConfig here, so use the default tab width. The fast path is
    // conservative and slight over-counting just means we run reflow when we
    // could have skipped it, which is safe.
    let prefix_col = c
        .line_indent_bytes
        .chars()
        .fold(0usize, |col, ch| advance_col(col, ch, FAST_PATH_TAB_WIDTH));
    let body_width = body
        .chars()
        .fold(0usize, |col, ch| advance_col(col, ch, FAST_PATH_TAB_WIDTH));
    prefix_col + body_width <= column_limit
}

fn split_lines(text: &str) -> Vec<RawLine> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        let (body, had_crlf) = if let Some(stripped) = line.strip_suffix('\r') {
            (stripped.to_string(), true)
        } else {
            (line.to_string(), false)
        };
        out.push(RawLine { body, had_crlf });
    }
    if text.ends_with('\n') {
        out.pop();
    }
    out
}

struct RawLine {
    body: String,
    had_crlf: bool,
}

fn strip_prefixes(raw: &[RawLine], style: Style) -> Vec<StrippedLine> {
    let mut out = Vec::with_capacity(raw.len());
    for (i, r) in raw.iter().enumerate() {
        let (prefix, body) = strip_one_line(&r.body, style, i == 0, i == raw.len() - 1);
        out.push(StrippedLine {
            prefix,
            body,
            had_crlf: r.had_crlf,
        });
    }
    out
}

/// Split a comment source line into (emitted prefix, body content). The four
/// cases below are independent: which marker family applies is decided once by
/// style, then opener, closer, and continuation are each matched in isolation.
fn strip_one_line(line: &str, style: Style, first: bool, last: bool) -> (String, String) {
    let trimmed_left = line.trim_start_matches([' ', '\t']);
    let leading_ws = &line[..line.len() - trimmed_left.len()];
    match style {
        Style::Line | Style::DocLine => strip_line_marker(line, leading_ws, trimmed_left),
        Style::Block | Style::DocBlock => {
            // A merged run of one-line block comments ("parse::can_merge")
            // keeps every member's own "/* ... */" on its own source line, and
            // only the run's first opener and last closer survive into the
            // emitted block. Strip each member's delimiters here: left in the
            // body they hide the content from bookend detection, so "/*--- end
            // x ---*/" survives pass 1 and only gets stripped on pass 2, once
            // the emitter has spaced the delimiters apart. That is the
            // identical-bytes invariant broken.
            if !(first && last)
                && let Some(hit) = strip_merged_member(leading_ws, trimmed_left, first)
            {
                return hit;
            }
            if first && let Some(hit) = strip_block_opener(leading_ws, trimmed_left, style, last) {
                return hit;
            }
            if last && let Some(hit) = strip_block_closer(leading_ws, trimmed_left) {
                return hit;
            }
            strip_block_continuation(leading_ws, trimmed_left)
        }
    }
}

/// Body after the marker, with one separator space consumed.
fn marker_body(rest: &str) -> String {
    rest.strip_prefix(' ').unwrap_or(rest).to_string()
}

fn strip_line_marker(line: &str, leading_ws: &str, trimmed_left: &str) -> (String, String) {
    // Longer prefixes first so "///" wins over "//". Slash markers are a closed
    // set; hash markers come in arbitrary-length runs ("#", "##", "###", ...)
    // and shell convention uses the run length as a visual heading level, so
    // preserve it verbatim instead of collapsing "## foo" to "# # foo".
    for marker in ["///", "//!", "//"] {
        if let Some(rest) = trimmed_left.strip_prefix(marker) {
            return (format!("{leading_ws}{marker} "), marker_body(rest));
        }
    }

    // Mid-file "#!" is a regular shell comment per kernel exec(2) semantics:
    // only the file-leading shebang is special, and extraction already filtered
    // that one out. Treat "#!" as a two-character marker so reflow round-trips
    // it as "#!" and not as "#" (which would emit "# ! body") or "//" (the
    // pre-fix bug, where falling out of every branch left line_marker = None
    // and reflow used the slash default).
    if let Some(rest) = trimmed_left.strip_prefix("#!") {
        return (format!("{leading_ws}#! "), marker_body(rest));
    }
    let hash_run: String = trimmed_left.chars().take_while(|&c| c == '#').collect();
    if !hash_run.is_empty() {
        let rest = &trimmed_left[hash_run.len()..];
        return (format!("{leading_ws}{hash_run} "), marker_body(rest));
    }
    (String::new(), line.to_string())
}

/// Strip one member of a merged block-comment run: a source line that is a
/// complete "/* ... */" by itself. Inside a single block comment an interior
/// "*/" cannot occur (tree-sitter would have ended the comment there), so a
/// line carrying both delimiters is always a separate comment the merge pulled
/// in. Returns None for anything else.
fn strip_merged_member(
    leading_ws: &str,
    trimmed_left: &str,
    first: bool,
) -> Option<(String, String)> {
    let rest = trimmed_left.strip_suffix("*/")?;

    // Not "/*-": a "/*---" banner rule must keep its full dash run so
    // "bookend_match" can see it (see "is_bsd_dash_opener").
    let marker = ["/**", "/*!", "/*"]
        .into_iter()
        .find(|m| trimmed_left.starts_with(m))?;
    let inner = rest.get(marker.len()..)?;
    if inner.contains("*/") {
        return None;
    }
    let prefix = if first {
        format!("{leading_ws}{marker} ")
    } else {
        format!("{leading_ws} * ")
    };
    Some((prefix, marker_body(inner).trim_end().to_string()))
}

fn strip_block_opener(
    leading_ws: &str,
    trimmed_left: &str,
    style: Style,
    last: bool,
) -> Option<(String, String)> {
    // A plain block that opens "/**" is a banner, not a doc comment (classify
    // already decided that), so the extra stars are decoration to drop.
    if matches!(style, Style::Block)
        && !last
        && let Some(rest) = trimmed_left.strip_prefix("/**")
    {
        return Some((
            format!("{leading_ws}/* "),
            marker_body(rest.trim_start_matches('*')),
        ));
    }
    let markers: &[&str] = match style {
        Style::DocBlock => &["/**", "/*!"],
        _ if is_bsd_dash_opener(trimmed_left) => &["/*-", "/*"],
        _ => &["/*"],
    };
    for marker in markers {
        if let Some(rest) = trimmed_left.strip_prefix(marker) {
            let body = marker_body(rest);
            // A one-line comment carries its own closer on this same line.
            let body = if last {
                body.strip_suffix("*/")
                    .map(|s| s.trim_end().to_string())
                    .unwrap_or(body)
            } else {
                body
            };
            return Some((format!("{leading_ws}{marker} "), body));
        }
    }
    None
}

fn strip_block_closer(leading_ws: &str, trimmed_left: &str) -> Option<(String, String)> {
    for marker in ["**/", "*/"] {
        if let Some(rest_before) = trimmed_left.strip_suffix(marker) {
            let body = rest_before.trim_start_matches([' ', '*']).to_string();

            // A bare closer keeps its own line; content before the closer is
            // re-emitted as an ordinary continuation line.
            if body.is_empty() && marker == "*/" {
                return Some((format!("{leading_ws} */"), String::new()));
            }
            return Some((format!("{leading_ws} * "), body));
        }
    }
    None
}

fn strip_block_continuation(leading_ws: &str, trimmed_left: &str) -> (String, String) {
    for marker in ["* ", "*", "** ", "**"] {
        let Some(rest) = trimmed_left.strip_prefix(marker) else {
            continue;
        };

        // For markers WITHOUT a trailing space ("*", "**"), accept this as a
        // prefix strip only when the byte after the marker is whitespace or
        // end-of-line. Otherwise "*foo" inside a body would read as marker "*"
        // plus content "foo", and round-tripping would emit " * foo": content
        // corruption. Markers that already include the separator space don't
        // need the check.
        if !marker.ends_with(' ') && !matches!(rest.chars().next(), None | Some(' ' | '\t')) {
            continue;
        }
        let stars = marker.trim_end();
        return (format!("{leading_ws} {stars} "), rest.to_string());
    }
    (format!("{leading_ws} * "), trimmed_left.to_string())
}

/// Collapse decorative dash/equals bookends to what they were framing. A body
/// of the shape "----- LABEL -----" becomes "LABEL"; a body that is nothing but
/// a rule becomes an empty comment line. Cancelled when the line directly above
/// or below in the same comment is itself art-like, so multi-line ASCII
/// drawings keep the rules that border them. "bookend_match" decides the shape;
/// this decides whether the strip is safe to apply.
fn strip_decorative_bookends(lines: &mut [StrippedLine], kind: &Kind) {
    /// What each line looked like BEFORE this pass mutated anything, so the
    /// adjacency decisions below all see the original input.
    enum Snapshot {
        BookendLabeled(String),
        BookendBare,
        ArtOnly,
        Other,
    }

    let n = lines.len();
    if n == 0 {
        return;
    }
    let snapshots: Vec<Snapshot> = lines
        .iter()
        .map(|l| match bookend_match(&l.body) {
            Some(BookendKind::Labeled(label)) => Snapshot::BookendLabeled(label),
            Some(BookendKind::Bare) => Snapshot::BookendBare,
            None if line_is_art_only(&l.body) => Snapshot::ArtOnly,
            None => Snapshot::Other,
        })
        .collect();

    // Which lines are still there once this pass finishes. A bare rule that is
    // about to be deleted protects nothing: the next run sees the label alone
    // and strips it then, so the file never settles. That is bzlib.h's footer,
    // "/*---*/" over "/*--- end bzlib.h ---*/" over "/*---*/", where pass 1
    // deleted both rules and kept the dashes on the label, and pass 2 removed
    // them. Two stacked rules still protect each other: neither is deleted,
    // they are one thick border.
    //
    // CONTRACT: "classify_lines" asks the same "is the neighbor protective?"
    // question through "is_art_adjacent_bookend", but it runs AFTER this pass,
    // so a deleted rule is already an empty body there and "line_is_art_only"
    // rejects it. Narrowing the set here is what makes the two agree.
    // "stacked_solid_star_rules_preserved_as_art" exercises both paths.
    let raw_protective = |s: &Snapshot| matches!(s, Snapshot::ArtOnly | Snapshot::BookendBare);
    let protective: Vec<bool> = (0..n)
        .map(|j| match &snapshots[j] {
            Snapshot::ArtOnly => true,
            Snapshot::BookendBare => {
                (j > 0 && raw_protective(&snapshots[j - 1]))
                    || (j + 1 < n && raw_protective(&snapshots[j + 1]))
            }
            _ => false,
        })
        .collect();

    for i in 0..n {
        let new_body: Option<String> = match &snapshots[i] {
            Snapshot::BookendLabeled(label) => Some(label.clone()),
            Snapshot::BookendBare => {
                // Rustdoc setext guard: a bare bookend whose immediate
                // predecessor is prose is exactly the setext-underline shape. A
                // standalone Rustdoc bare line is also ambiguous, so keep it
                // intact and let classify_lines pick it up.
                if matches!(kind.flavor, DocFlavor::Rustdoc)
                    && (n == 1 || (i > 0 && matches!(snapshots[i - 1], Snapshot::Other)))
                {
                    None
                } else {
                    Some(String::new())
                }
            }
            _ => None,
        };
        let Some(replacement) = new_body else {
            continue;
        };

        let above_art = i > 0 && protective[i - 1];
        let below_art = i + 1 < n && protective[i + 1];
        if above_art || below_art {
            continue;
        }
        lines[i].body = replacement;
    }
}

/// True when a single comment-source-line (full text, marker included)
/// has a bookend body that should be stripped. Used by the single-line fast
/// path to force normalization for comments that would otherwise pass through
/// unchanged.
fn single_line_has_strippable_bookend(text: &str, kind: &Kind) -> bool {
    let trimmed = text.trim_start_matches([' ', '\t']);
    let body: &str = if let Some(rest) = trimmed.strip_prefix("///") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("//!") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("//") {
        rest
    } else if trimmed.starts_with("#!") {
        return false;
    } else if trimmed.starts_with('#') {
        trimmed.trim_start_matches('#')
    } else if block_is_doc(trimmed) {
        // Doc blocks strip their full "/**" / "/*!" marker, so a dash bookend
        // living inside a doc comment ("/** ==== x ==== */") is still detected.
        let rest = trimmed
            .strip_prefix("/**")
            .or_else(|| trimmed.strip_prefix("/*!"))
            .unwrap_or(trimmed);

        // "trim_end_matches("*/")" would treat "*/" as a char SET, so any
        // trailing run of "*" or "/" would be eaten, so "/* body** */" would
        // lose the in-body "**". Use a precise suffix strip.
        rest.strip_suffix("*/").unwrap_or(rest).trim_end()
    } else if let Some(rest) = trimmed.strip_prefix("/*") {
        // Plain blocks strip only the 2-char "/*" delimiter (NOT a greedy
        // "/**"), so a star banner's own asterisks stay in the body ("/****
        // label ****/" -> "*** label ***") and bookend_match collapses it like
        // the dash/equals forms. Mirrors strip_one_line's plain-block strip so
        // detection and application agree.
        rest.strip_suffix("*/").unwrap_or(rest).trim_end()
    } else {
        return false;
    };
    match bookend_match(body) {
        Some(BookendKind::Labeled(_)) => true,
        Some(BookendKind::Bare) => !matches!(kind.flavor, DocFlavor::Rustdoc),
        None => false,
    }
}

fn pick_default_continuation_prefix(lines: &[StrippedLine]) -> String {
    // The opener and closer lines carry their own markers; the continuation
    // prefix comes from the first interior line, defaulting to "*".
    let chosen = lines
        .iter()
        .enumerate()
        .find(|&(i, _)| i != 0 && i + 1 != lines.len())
        .map_or("*", |(_, l)| l.prefix.trim());

    // Block continuations are strictly single-star: a star-run marker ("**",
    // "***") collapses to the canonical " * ". Preserving the run instead would
    // mean every downstream prefix strip had to handle both widths, and the
    // extra stars carry no meaning.
    if chosen.is_empty() || chosen.chars().all(|c| c == '*') {
        return " * ".to_string();
    }
    format!(" {chosen} ")
}

enum KTag {
    Param,
    Return,
}

/// Classify an already-prefix-stripped tag keyword. Single source of truth for
/// which tags we convert: "kdoc_tag_of" and "is_foreign_tag" both route
/// through here so the accepted set can't drift between them. The three Doxygen
/// direction annotations ("param[in]", "param[out]", "param[in,out]") are
/// accepted; the direction is dropped, kernel-doc has no equivalent. A
/// malformed direction ("param[", "param[bogus") is not a param tag.
fn kdoc_tag_of_keyword(kw: &str) -> Option<KTag> {
    match kw {
        "param" | "param[in]" | "param[out]" | "param[in,out]" => Some(KTag::Param),
        "return" | "returns" => Some(KTag::Return),
        _ => None,
    }
}

/// Classify one whitespace-delimited word ("\param"/"@param", "@param[in]",
/// "\return"/"\returns", …) as a convertible tag. Both "@" and "\" spellings
/// are accepted.
fn kdoc_tag_of(word: &str) -> Option<KTag> {
    kdoc_tag_of_keyword(word.strip_prefix(['@', '\\'])?)
}

/// A kernel-doc param name: non-empty "[A-Za-z0-9_]+". Names outside this
/// grammar (e.g. "x,y") would not round-trip through "is_kernel_doc_tag", so a
/// param carrying one makes the whole comment pass through.
fn is_kdoc_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A whitespace-delimited word that looks like a Doxygen/kernel-doc tag token
/// ("@x" / "\x") but isn't one we convert. Its presence anywhere in the comment
/// makes the whole thing pass through. Strips the prefix once and reuses
/// "kdoc_tag_of_keyword" so the convertible set stays in one place.
fn is_foreign_tag(word: &str) -> bool {
    word.strip_prefix(['@', '\\']).is_some_and(|kw| {
        kw.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && kdoc_tag_of_keyword(kw).is_none()
    })
}

/// Whether a whitespace-delimited word is a cross-reference to one of this
/// comment's own parameters rather than a Doxygen block tag. Doxygen prose
/// writes a parameter reference as "@name" ("aligned to @align, or NULL"), the
/// same spelling a block tag uses, so two things separate them.
///
/// The name has to be one the comment declares, and it must not be a Doxygen
/// tag keyword in its own right: a function with a parameter named "file" or
/// "note", both ordinary in C and both in "DOXY_TAGS", would otherwise swallow
/// a real "@file"/"@note" section into the preceding parameter's description
/// instead of aborting the conversion.
///
/// Deliberately position-blind. Line position looks like the sharper
/// discriminator (a block tag opens a line, a reference in running prose
/// essentially never does) and it is not, because reflow OWNS line position.
/// In a comment this aborts on, the next run re-wraps the untouched text and
/// merges the line-leading "@file" into the line above; a position test then
/// reads it mid-line as a reference and converts what the run before refused.
/// That is a two-pass fixed point, which "--check" converges on instead of
/// reporting. A name-only test cannot move under the packer.
///
/// Ceiling: the test is "DOXY_TAGS" membership, and that table is the subset
/// of Doxygen commands this tool knows, not all of them. A parameter named
/// after a command outside it ("@param arg" beside an "@arg" list) still reads
/// the command as a reference and folds it into the description. Widen
/// "DOXY_TAGS" when a real file needs it.
fn is_param_xref(word: &str, param_names: &[&str]) -> bool {
    kdoc_xref_name(word).is_some_and(|name| param_names.contains(&name) && doxy_tag(name).is_none())
}

/// The parameter name an "@name"/"\\name" cross-reference points at, or "None" if the
/// word cannot be one. Trailing punctuation is trimmed, since a reference at a
/// clause break ("@align,") is still a reference.
///
/// Both Doxygen spellings are supported, except one-character C escapes such
/// as "\\n"; the name must otherwise open like a C identifier, so "\\0"
/// cannot be a reference.
fn kdoc_xref_name(word: &str) -> Option<&str> {
    let kw = word.strip_prefix(['@', '\\'])?;
    let name = kw.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');

    // After the trim, not before: prose ends a clause, and "\\n." is the same
    // escape as "\\n". Checking the untrimmed word let a trailing period decide
    // whether a comment converted at all.
    if word.starts_with('\\') && matches!(name, "a" | "b" | "e" | "f" | "n" | "r" | "t" | "v") {
        return None;
    }
    let starts_like_ident = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    (starts_like_ident && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .then_some(name)
}

/// Split a prose line at a mid-line Doxygen return tag, using the very splitter
/// reflow uses so the two cannot disagree.
///
/// Without this, "/* Does the thing. @return the value. */" settles only after
/// two runs: "convert_kernel_doc" gates on the FIRST WORD of a line, so a
/// mid-paragraph tag is invisible to it; reflow then hoists the tag onto a line
/// of its own; and the next run, seeing it at a line start, converts it.
/// Splitting here puts the tag at a line start before the converter looks, so
/// one run is enough. C/C++ only, matching the conversion it feeds.
fn split_doxygen_return_boundaries(lines: Vec<Line>, lang: Language) -> Vec<Line> {
    if !matches!(lang, Language::C | Language::Cpp) {
        return lines;
    }
    let is_return_tag = |s: &str| {
        s.split_whitespace()
            .next()
            .and_then(kdoc_tag_of)
            .is_some_and(|tag| matches!(tag, KTag::Return))
    };

    let mut out = Vec::with_capacity(lines.len());
    for l in lines {
        if !matches!(l.kind, LineKind::Prose) {
            out.push(l);
            continue;
        }
        let segments = split_at_return_boundary(&l.text);

        // Only a split that hoists a return TAG matters here. A prose "Return"
        // split needs no help: nothing converts it, so reflow doing it later is
        // already stable.
        if segments.len() < 2 || !segments[1..].iter().any(|s| is_return_tag(s)) {
            out.push(l);
            continue;
        }
        for text in segments {
            out.push(Line {
                kind: LineKind::Prose,
                raw: text.clone(),
                text,
                had_crlf: l.had_crlf,
            });
        }
    }
    out
}

/// Convert a C/C++ function comment's Doxygen param/return block into Linux
/// kernel-doc style:
///
///   \param NAME desc   ->  @NAME : desc          (each its own tag paragraph)
///   \return desc       ->  (blank line) Return desc
///
/// Params are hoisted ahead of the return, matching kernel-doc order, and the
/// leading description (everything before the tag region) is preserved
/// verbatim.
/// A line-start prose "Return"/"Returns" inside the region converts too, but
/// only there: a comment that is prose-only, with no param or return tag, is
/// left alone, since the reflow layer already owns the standalone prose-Return
/// rule.
///
/// Deliberately narrow. The region begins at the first line whose FIRST word is
/// a param/return tag. A tag buried mid-sentence ("write @param name to ...")
/// never starts a conversion, so ordinary prose that merely mentions a tag is
/// untouched. Any other whitespace-delimited Doxygen tag token anywhere
/// ("@brief", "@note", "@retval", …), a nameless param, or a param whose name
/// isn't a bare identifier makes the whole comment pass through unchanged:
/// half-converting a doc comment is worse than leaving it. A tag glued inside
/// description punctuation ("(@ref x)") is not a whitespace-delimited token, so
/// it rides along as description text rather than aborting the conversion.
///
/// Multi-line comments only: a single-line tag comment takes normalize's
/// short-circuit and never reaches here. Widen the tag set, or add single-line
/// handling, when a real file needs it.
fn convert_kernel_doc(lines: Vec<Line>, lang: Language) -> Vec<Line> {
    if !matches!(lang, Language::C | Language::Cpp) {
        return lines;
    }

    // The region opens at the first line that BEGINS with a tag. A tag that
    // only appears mid-line marks prose describing the tag, not a doc block.
    let Some(first) = lines.iter().position(|l| {
        l.text
            .split_whitespace()
            .next()
            .is_some_and(|w| kdoc_tag_of(w).is_some())
    }) else {
        return lines;
    };

    // A whitespace-delimited tag token we don't convert, ANYWHERE in the
    // comment (head included), aborts the conversion. Scanning the whole
    // comment, not just the tag region, keeps a "@brief"/"@note" in the leading
    // description from being silently preserved beside converted params. ...
    // except a reference to one of this comment's OWN params. Doxygen prose
    // says "a multiple of @align", and kernel-doc keeps that spelling, so
    // treating it as a foreign tag aborted every function comment that
    // cross-references its arguments (the common case, not a corner). Collect
    // the declared names first, then let those words ride along as description
    // text. Names come from the tag region ONLY, the same first-word rule that
    // opens it: head prose saying "pass @param note to the logger" declares
    // nothing.
    //
    // The scan reads the region's words as ONE stream rather than line by line,
    // because that is how the entry scan below reads a name: a "@param" ending
    // a line takes its name off the line after. Restarting per line missed that
    // name, so the comment aborted, and the next run (reflow having rejoined
    // the two) converted it: a two-pass fixed point.
    let has_foreign_tag = {
        let mut param_names: Vec<&str> = Vec::new();
        let mut words = lines[first..]
            .iter()
            .flat_map(|l| l.text.split_whitespace());
        while let Some(w) = words.next() {
            if matches!(kdoc_tag_of(w), Some(KTag::Param))
                && let Some(n) = words.next().filter(|n| is_kdoc_name(n))
            {
                param_names.push(n);
            }
        }
        lines.iter().any(|l| {
            l.text
                .split_whitespace()
                .any(|w| is_foreign_tag(w) && !is_param_xref(w, &param_names))
        })
    };
    if has_foreign_tag {
        return lines;
    }

    // Reduce the tag region (first tag line to end) to (tag, description-words)
    // entries. "bail" keeps "lines" intact so we can return it untouched.
    let mut entries: Vec<(KTag, Vec<String>)> = Vec::new();
    let mut bail = false;
    let mut blank_after_entry = false;
    'scan: for l in &lines[first..] {
        if matches!(l.kind, LineKind::Blank) {
            blank_after_entry |= !entries.is_empty();
            continue;
        }
        let mut at_line_start = true;
        for w in l.text.split_whitespace() {
            if let Some(tag) = kdoc_tag_of(w) {
                entries.push((tag, Vec::new()));
                blank_after_entry = false;
            } else if at_line_start && (w == "Return" || w == "Returns") {
                // Prose "Returns X" at a line start is the return description
                // (case-sensitive, leading capital, the same rule reflow uses),
                // not a continuation of the preceding param.
                entries.push((KTag::Return, Vec::new()));
                blank_after_entry = false;
            } else {
                if at_line_start && blank_after_entry {
                    bail = true;
                    break 'scan;
                }

                // Foreign tags are already ruled out; a word with no entry to
                // attach to means text sits before the first tag on the region
                // line: pass through rather than guess where it belongs.
                let Some((_, desc)) = entries.last_mut() else {
                    bail = true;
                    break 'scan;
                };
                desc.push(w.to_string());
            }
            at_line_start = false;
        }
    }

    // A param name that is itself a convertible tag keyword cannot round-trip:
    // "@param return desc" emits "@return : desc", which the NEXT run reads as
    // a return tag and rewrites again to "Return : desc". Pass through instead.
    let bad_param = entries.iter().any(|(tag, desc)| {
        matches!(tag, KTag::Param)
            && !desc
                .first()
                .is_some_and(|n| is_kdoc_name(n) && kdoc_tag_of_keyword(n).is_none())
    });
    if bail || bad_param {
        return lines;
    }

    let had_crlf = lines[first].had_crlf;
    let mk = |kind, text: String| Line {
        kind,
        raw: text.clone(),
        text,
        had_crlf,
    };
    let mut out: Vec<Line> = lines[..first].to_vec();
    let mut returns: Vec<String> = Vec::new();
    for (tag, desc) in &entries {
        match tag {
            KTag::Param => {
                let name = &desc[0];
                let rest = desc[1..].join(" ");
                let body = if rest.is_empty() {
                    format!("@{name} :")
                } else {
                    format!("@{name} : {rest}")
                };
                out.push(mk(LineKind::DoxygenTag, body));
            }
            KTag::Return => returns.push(desc.join(" ")),
        }
    }
    if !returns.is_empty() {
        // Blank line before Return, but never as the comment's first line (a
        // return-only comment whose region starts at line 0 has empty "out").
        if !out.is_empty() && !matches!(out.last().map(|l| l.kind), Some(LineKind::Blank)) {
            out.push(mk(LineKind::Blank, String::new()));
        }
        for rt in returns {
            // Empty return description emits a bare "Return", with no trailing
            // space.
            let body = if rt.is_empty() {
                "Return".to_string()
            } else {
                format!("Return {rt}")
            };
            out.push(mk(LineKind::Prose, body));
        }
    }
    out
}

/// Convert an X11 manual-page comment body (uppercase "DESCRIPTION" /
/// "RETURN(S)" section headers with indented bodies) into kernel-doc-adjacent
/// prose. Returns "None" unless BOTH a "DESCRIPTION" and a "RETURN(S)" header
/// are present and the whole body parses as recognized sections.
fn convert_manpage_sections(lines: &[Line]) -> Option<Vec<Line>> {
    convert_manpage_segments(
        lines.iter().map(|l| l.text.trim().to_string()),
        lines.first().is_some_and(|l| l.had_crlf),
    )
}

/// A fallback for already-mangled one-line-ish manual pages where an earlier
/// reflow collapsed leading "*" line markers into the prose. Splits the body on
/// "*", treating EVERY standalone "*" as a former line marker. This both
/// un-mangles the structure and rejoins words the collapse split
/// ("XcmsColor * structures" → "XcmsColor structures"). Ceiling: a mangled
/// comment whose prose genuinely needed a "*" loses it. Safe because the clean
/// path runs first, so a "*" that is real content ("the *.c files") never
/// reaches here.
fn convert_mangled_manpage_sections(text: &str, had_crlf: bool) -> Option<Vec<Line>> {
    let body = text.trim();
    let body = body
        .strip_prefix("/**")
        .or_else(|| body.strip_prefix("/*"))?;
    let body = body.strip_suffix("*/").unwrap_or(body);
    convert_manpage_segments(
        body.split('*')
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")),
        had_crlf,
    )
}

fn convert_manpage_segments(
    segments: impl IntoIterator<Item = String>,
    had_crlf: bool,
) -> Option<Vec<Line>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Section {
        Description,
        Return,
    }
    // Case-sensitive: only real manual-page headers, all uppercase.
    fn recognize(seg: &str) -> Option<Section> {
        let clean = seg.strip_suffix(':').unwrap_or(seg);
        match clean {
            "DESCRIPTION" => Some(Section::Description),
            "RETURN" | "RETURNS" | "RETURN VALUE" | "RETURN VALUES" => Some(Section::Return),
            _ => None,
        }
    }

    // A standalone ALL-CAPS segment (with an optional trailing colon) is a
    // section-header shape even when we don't recognize the keyword (NAME,
    // SYNOPSIS, ERRORS, …). Those force a bail so we never fold an unhandled
    // section's body into the description.
    fn looks_like_header(seg: &str) -> bool {
        let clean = seg.strip_suffix(':').unwrap_or(seg);
        !clean.is_empty()
            && clean.len() <= 24
            && clean.chars().all(|c| c.is_ascii_uppercase() || c == ' ')
            && clean.chars().any(|c| c.is_ascii_uppercase())
    }

    let mut cur: Option<Section> = None;
    let mut desc: Vec<String> = Vec::new(); // prose lines; empty string == blank
    let mut ret: Vec<String> = Vec::new(); // return description words
    let mut seen_desc = false;
    let mut seen_ret = false;

    for seg in segments {
        if seg.is_empty() {
            // Preserve paragraph breaks inside the description; blanks
            // elsewhere are structural noise between sections.
            if matches!(cur, Some(Section::Description)) && !desc.is_empty() {
                desc.push(String::new());
            }
            continue;
        }
        if looks_like_header(&seg) {
            let sec = recognize(&seg)?;
            match sec {
                Section::Description => seen_desc = true,
                Section::Return => seen_ret = true,
            }
            cur = Some(sec);
            continue;
        }
        match cur {
            Some(Section::Description) => desc.push(seg),
            Some(Section::Return) => ret.extend(seg.split_whitespace().map(str::to_string)),
            None => return None, // body text before any header: not a clean block
        }
    }

    if !(seen_desc && seen_ret) {
        return None;
    }
    while desc.last().is_some_and(String::is_empty) {
        desc.pop();
    }
    if desc.is_empty() {
        return None;
    }

    let mk = |kind, text: String| Line {
        kind,
        raw: text.clone(),
        text,
        had_crlf,
    };
    let mut out: Vec<Line> = desc
        .into_iter()
        .map(|s| {
            if s.is_empty() {
                mk(LineKind::Blank, String::new())
            } else {
                mk(LineKind::Prose, s)
            }
        })
        .collect();

    // The section body often re-states "Returns …"; fold it into the single
    // kernel-doc "Return" lead so we don't emit "Return Returns …".
    let mut desc_text = ret.join(" ");
    for lead in ["Returns ", "Return ", "returns ", "return "] {
        if let Some(rest) = desc_text.strip_prefix(lead) {
            desc_text = rest.to_string();
            break;
        }
    }
    if desc_text.eq_ignore_ascii_case("returns") || desc_text.eq_ignore_ascii_case("return") {
        desc_text.clear();
    }
    out.push(mk(LineKind::Blank, String::new()));
    let body = if desc_text.is_empty() {
        "Return".to_string()
    } else {
        format!("Return {desc_text}")
    };
    out.push(mk(LineKind::Prose, body));
    Some(out)
}

fn group_paragraphs(lines: &[Line]) -> Vec<Paragraph> {
    let mut paragraphs = Vec::new();
    let mut i = 0;
    let mut blank_pending = false;
    while i < lines.len() {
        let l = &lines[i];
        match l.kind {
            LineKind::Blank => {
                blank_pending = true;
                i += 1;
            }
            LineKind::AtxHeader => {
                paragraphs.push(Paragraph {
                    kind: ParagraphKind::AtxHeader,
                    line_indices: vec![i],
                    preceded_by_blank: blank_pending,
                });
                blank_pending = false;
                i += 1;
            }
            kind if is_preformatted_kind(kind) => {
                let start = i;
                while i < lines.len() && is_preformatted_kind(lines[i].kind) {
                    i += 1;
                }
                paragraphs.push(Paragraph {
                    kind: ParagraphKind::Preformatted,
                    line_indices: (start..i).collect(),
                    preceded_by_blank: blank_pending,
                });
                blank_pending = false;
            }
            LineKind::Prose
            | LineKind::DoxygenTag
            | LineKind::ListItem
            | LineKind::SetextUnderline => {
                let start = i;
                i += 1;
                while i < lines.len()
                    && matches!(lines[i].kind, LineKind::Prose)
                    && !starts_new_prose_paragraph(&lines[i])
                {
                    i += 1;
                }
                paragraphs.push(Paragraph {
                    kind: ParagraphKind::Prose,
                    line_indices: (start..i).collect(),
                    preceded_by_blank: blank_pending,
                });
                blank_pending = false;
            }
            _ => {
                paragraphs.push(Paragraph {
                    kind: ParagraphKind::Preformatted,
                    line_indices: vec![i],
                    preceded_by_blank: blank_pending,
                });
                blank_pending = false;
                i += 1;
            }
        }
    }
    paragraphs
}

fn is_preformatted_kind(k: LineKind) -> bool {
    matches!(
        k,
        LineKind::FenceOpen
            | LineKind::FenceContent
            | LineKind::FenceClose
            | LineKind::DoxyVerbatimOpen
            | LineKind::DoxyVerbatimContent
            | LineKind::DoxyVerbatimClose
            | LineKind::IndentedCode
            | LineKind::TableRow
            | LineKind::Blockquote
            | LineKind::ReferenceLink
            | LineKind::Metadata
            | LineKind::LabelRow
            | LineKind::Banner
            | LineKind::Art
    )
}

fn starts_new_prose_paragraph(l: &Line) -> bool {
    let t = l.text.trim_start();
    starts_with_word(t, "Return") || starts_with_word(t, "Returns")
}

fn starts_with_word(s: &str, word: &str) -> bool {
    if !s.starts_with(word) {
        return false;
    }
    s[word.len()..]
        .chars()
        .next()
        .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_kind() -> Kind {
        Kind {
            style: Style::Line,
            flavor: DocFlavor::None,
        }
    }

    #[test]
    fn trailing_block_closer_split() {
        // Glued "*/" on the last line splits onto its own line, aligned under
        // the continuation stars.
        let text = "/* foo\n     * bar. */";
        assert_eq!(
            split_trailing_block_closer(text).as_deref(),
            Some("/* foo\n     * bar.\n     */")
        );
        // Idempotent: an already-split closer is left alone.
        assert_eq!(
            split_trailing_block_closer("/* foo\n     * bar.\n     */"),
            None
        );
        // Single-line trailing block has no interior line to split.
        assert_eq!(split_trailing_block_closer("/* foo */"), None);
        // Empty last line (bare closer with only a marker) stays put.
        assert_eq!(split_trailing_block_closer("/* foo\n     * */"), None);
        // CRLF endings are preserved.
        assert_eq!(
            split_trailing_block_closer("/* foo\r\n     * bar. */").as_deref(),
            Some("/* foo\r\n     * bar.\r\n     */")
        );
    }

    fn mk_lines(bodies: &[(&str, LineKind)]) -> Vec<Line> {
        bodies
            .iter()
            .map(|(b, k)| Line {
                kind: *k,
                text: b.to_string(),
                had_crlf: false,
                raw: b.to_string(),
            })
            .collect()
    }

    fn convert_bodies(lines: Vec<Line>) -> Vec<String> {
        convert_kernel_doc(lines, Language::C)
            .into_iter()
            .map(|l| l.text)
            .collect()
    }

    #[test]
    fn kernel_doc_glued_params_and_return_split() {
        // Two params glued on one line plus a wrapped return: the shape from
        // AI-generated code this feature targets.
        let lines = mk_lines(&[
            ("Brief.", LineKind::Prose),
            ("", LineKind::Blank),
            ("\\param ctp the ctp \\param msg the", LineKind::Prose),
            ("msg \\return EOK on failure", LineKind::Prose),
        ]);
        assert_eq!(
            convert_bodies(lines),
            vec![
                "Brief.",
                "",
                "@ctp : the ctp",
                "@msg : the msg",
                "",
                "Return EOK on failure",
            ]
        );
    }

    #[test]
    fn kernel_doc_consecutive_params_no_blank_between() {
        // Three params on their own lines convert to three adjacent tag lines,
        // with no extraneous blank line slipping in between them.
        let lines = mk_lines(&[
            ("@param a first", LineKind::Prose),
            ("@param b second", LineKind::Prose),
            ("@param c third", LineKind::Prose),
        ]);
        assert_eq!(
            convert_bodies(lines),
            vec!["@a : first", "@b : second", "@c : third"]
        );
    }

    #[test]
    fn kernel_doc_hoists_return_after_params() {
        // @return appears before @param in source; output puts params first.
        let lines = mk_lines(&[
            ("@return zero", LineKind::Prose),
            ("@param x the x", LineKind::Prose),
        ]);
        assert_eq!(convert_bodies(lines), vec!["@x : the x", "", "Return zero"]);
    }

    #[test]
    fn kdoc_xref_name_forms() {
        assert_eq!(kdoc_xref_name("@align"), Some("align"));
        assert_eq!(kdoc_xref_name("\\align"), Some("align"));
        // Trailing punctuation at a clause break does not hide the reference.
        assert_eq!(kdoc_xref_name("@align,"), Some("align"));
        assert_eq!(kdoc_xref_name("@size."), Some("size"));
        assert_eq!(kdoc_xref_name("@_x)"), Some("_x"));
        // Escape sequences and names beginning with a digit are not references.
        assert_eq!(kdoc_xref_name("\\n"), None);
        assert_eq!(kdoc_xref_name("\\0"), None);
        // Trailing punctuation must not smuggle an escape past the check: a
        // clause ends with "\\n." as readily as it ends with "\\n".
        assert_eq!(kdoc_xref_name("\\n."), None);
        assert_eq!(kdoc_xref_name("\\t,"), None);
        // The "@" spelling is unambiguous, so a param really named "n" works.
        assert_eq!(kdoc_xref_name("@n"), Some("n"));
        // A name cannot open with a digit, and "@" alone names nothing.
        assert_eq!(kdoc_xref_name("@0"), None);
        assert_eq!(kdoc_xref_name("@"), None);
        assert_eq!(kdoc_xref_name("plain"), None);
        // A path shape is not a reference even though it opens with a tag
        // keyword: the dot leaves "file.txt" outside the identifier grammar.
        assert_eq!(kdoc_xref_name("@file.txt"), None);
    }

    #[test]
    fn kernel_doc_param_cross_reference_is_description_text() {
        // "@align" in prose names a param this comment declares, so it is a
        // cross-reference, not a foreign tag: it rides along as description
        // text instead of aborting the conversion. Trailing punctuation
        // ("@align,") does not hide the reference. Shape from tlsf.h.
        let lines = mk_lines(&[
            ("@param align Alignment in bytes", LineKind::Prose),
            (
                "@param size Need not be a multiple of @align",
                LineKind::Prose,
            ),
            ("@return Aligned to @align, or NULL", LineKind::Prose),
        ]);
        assert_eq!(
            convert_bodies(lines),
            vec![
                "@align : Alignment in bytes",
                "@size : Need not be a multiple of @align",
                "",
                "Return Aligned to @align, or NULL",
            ]
        );

        // A tag naming something the comment does NOT declare is still foreign,
        // and still aborts.
        let undeclared = mk_lines(&[
            ("@param x the x", LineKind::Prose),
            ("@return bounded by @limit", LineKind::Prose),
        ]);
        assert_eq!(
            convert_bodies(undeclared),
            vec!["@param x the x", "@return bounded by @limit"]
        );

        // Head prose that merely mentions "@param note" declares nothing: the
        // region opens at the first line whose FIRST word is a tag, and names
        // are collected from there on. A real "@note" below is still foreign
        // and still aborts, instead of being folded into "@x"'s description.
        let prose_mention = mk_lines(&[
            ("Pass @param note to the logger.", LineKind::Prose),
            ("@param x the x", LineKind::Prose),
            ("@note beware", LineKind::Prose),
        ]);
        assert_eq!(
            convert_bodies(prose_mention),
            vec![
                "Pass @param note to the logger.",
                "@param x the x",
                "@note beware",
            ]
        );

        // A param named after a real Doxygen tag does not turn that tag into a
        // reference: the name is in DOXY_TAGS, so the whole comment aborts
        // rather than folding "writer.c" into "@file"'s description. The name
        // is the discriminator, not the position, because the position moves:
        // reflow merges the aborted comment's lines and the next run would read
        // the very same "@file" mid-line.
        let collision = mk_lines(&[
            ("@param file the output stream", LineKind::Prose),
            ("@file writer.c", LineKind::Prose),
        ]);
        assert_eq!(
            convert_bodies(collision),
            vec!["@param file the output stream", "@file writer.c"]
        );

        // Same name, mid-line: the name still decides, so this aborts too.
        let midline = mk_lines(&[
            ("@param file the output stream", LineKind::Prose),
            ("Opened before @file is read.", LineKind::Prose),
        ]);
        assert_eq!(
            convert_bodies(midline),
            vec![
                "@param file the output stream",
                "Opened before @file is read."
            ]
        );

        // The ceiling, pinned so it is a decision and not a surprise: a param
        // named after a Doxygen command DOXY_TAGS does not list reads as a
        // reference wherever it sits, so a real "@foo" section folds into the
        // description. Position cannot rescue this (it is not stable under
        // reflow); widening DOXY_TAGS can.
        let custom_collision = mk_lines(&[
            ("@param foo the output stream", LineKind::Prose),
            ("@foo custom section", LineKind::Prose),
        ]);
        assert_eq!(
            convert_bodies(custom_collision),
            vec!["@foo : the output stream @foo custom section"]
        );

        // The name scan reads the region as one word stream, so a "@param"
        // ending a line still declares the name that opens the next one. Read
        // line by line, "size" went undeclared, the comment aborted, and the
        // run after (reflow having rejoined the two) converted it.
        let wrapped_decl = mk_lines(&[
            ("@param", LineKind::Prose),
            ("size the size in bytes", LineKind::Prose),
            ("@return a multiple of @size", LineKind::Prose),
        ]);
        assert_eq!(
            convert_bodies(wrapped_decl),
            vec![
                "@size : the size in bytes",
                "",
                "Return a multiple of @size",
            ]
        );

        // Trailing punctuation does not smuggle an undeclared name through.
        let undeclared_punct = mk_lines(&[
            ("@param x the x", LineKind::Prose),
            ("bounded by @limit, roughly", LineKind::Prose),
        ]);
        assert_eq!(
            convert_bodies(undeclared_punct),
            vec!["@param x the x", "bounded by @limit, roughly"]
        );
    }

    #[test]
    fn kernel_doc_bails_on_foreign_tag() {
        // An unconvertible tag (@note) leaves the whole comment untouched,
        // whether it sits after the params or ahead of them in the head.
        let after = mk_lines(&[
            ("@param x the x", LineKind::Prose),
            ("@note beware", LineKind::Prose),
        ]);
        assert_eq!(
            convert_bodies(after),
            vec!["@param x the x", "@note beware"]
        );
        let head = mk_lines(&[
            ("@brief does things", LineKind::Prose),
            ("@param x the x", LineKind::Prose),
        ]);
        assert_eq!(
            convert_bodies(head),
            vec!["@brief does things", "@param x the x"]
        );
    }

    #[test]
    fn kernel_doc_param_direction_dropped() {
        // @param[in]/@param[out] convert; the direction annotation is dropped.
        let lines = mk_lines(&[
            ("@param[in] src the source", LineKind::Prose),
            ("@param[out] dst the dest", LineKind::Prose),
        ]);
        assert_eq!(
            convert_bodies(lines),
            vec!["@src : the source", "@dst : the dest"]
        );
    }

    #[test]
    fn kernel_doc_bails_on_post_tag_prose() {
        let lines = mk_lines(&[
            ("@param x the x", LineKind::Prose),
            ("", LineKind::Blank),
            ("More details.", LineKind::Prose),
        ]);
        assert_eq!(
            convert_bodies(lines),
            vec!["@param x the x", "", "More details."]
        );
    }

    #[test]
    fn kernel_doc_bails_on_non_identifier_name() {
        // A param name outside [A-Za-z0-9_] would not round-trip, so it passes
        // through.
        let lines = mk_lines(&[("@param x,y the pair", LineKind::Prose)]);
        assert_eq!(convert_bodies(lines), vec!["@param x,y the pair"]);
    }

    #[test]
    fn kernel_doc_bails_on_malformed_direction() {
        // Only [in]/[out]/[in,out] are directions; a malformed one is foreign.
        let lines = mk_lines(&[("@param[bogus x the x", LineKind::Prose)]);
        assert_eq!(convert_bodies(lines), vec!["@param[bogus x the x"]);
    }

    #[test]
    fn kernel_doc_inline_ref_in_description_passes_through() {
        // A punctuation-glued inline tag ("(@ref foo)") is description text,
        // not a structural block tag, so it rides along inside the param desc.
        let lines = mk_lines(&[("@param x see (@ref foo) for details", LineKind::Prose)]);
        assert_eq!(
            convert_bodies(lines),
            vec!["@x : see (@ref foo) for details"]
        );
    }

    #[test]
    fn kernel_doc_empty_return_has_no_trailing_space() {
        let lines = mk_lines(&[
            ("@param x the x", LineKind::Prose),
            ("@return", LineKind::Prose),
        ]);
        assert_eq!(convert_bodies(lines), vec!["@x : the x", "", "Return"]);
    }

    #[test]
    fn kernel_doc_return_only_has_no_leading_blank() {
        // Region starts at line 0 (return-only), so no spurious leading blank.
        let lines = mk_lines(&[("@return zero on success", LineKind::Prose)]);
        assert_eq!(convert_bodies(lines), vec!["Return zero on success"]);
    }

    #[test]
    fn kernel_doc_ignores_mid_sentence_tag() {
        // A tag mentioned mid-prose (not the line's first word) is not a doc
        // block, so the comment passes through untouched.
        let lines = mk_lines(&[
            (
                "To document a parameter write @param name desc",
                LineKind::Prose,
            ),
            ("above the function body.", LineKind::Prose),
        ]);
        assert_eq!(
            convert_bodies(lines),
            vec![
                "To document a parameter write @param name desc",
                "above the function body.",
            ]
        );
    }

    #[test]
    fn kernel_doc_skips_non_c_languages() {
        let lines = mk_lines(&[("\\param x the x", LineKind::Prose)]);
        let out: Vec<String> = convert_kernel_doc(lines, Language::Rust)
            .into_iter()
            .map(|l| l.text)
            .collect();
        assert_eq!(out, vec!["\\param x the x"]);
    }

    #[test]
    fn single_line_bare_bookend_forces_normalize_for_plain_comments() {
        assert!(single_line_has_strippable_bookend(
            "// --------",
            &plain_kind()
        ));
        assert!(single_line_has_strippable_bookend("# =====", &plain_kind()));
    }
}
