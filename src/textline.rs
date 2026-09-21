//! Pure text predicates and measurements, shared by every stage.
//!
//! The bulk of them answer "would packing this line into a prose paragraph
//! destroy its meaning?": fences, tables, deliberately indented code, and
//! ASCII/Unicode art. The rest are the small shared rules that more than one
//! stage has to agree on exactly, so they cannot live in either stage: the
//! decorative-bookend shape, column arithmetic, and the sentence boundary
//! ahead of a Return keyword.
//!
//! This is the crate's leaf module. It takes no "crate::" dependency, which is
//! what lets "parse", "linekind", "normalize", and "reflow" share these rules
//! without importing each other. Anything two stages must agree on byte for
//! byte belongs here.
//!
//! # The discriminator model
//!
//! Comment body lines fall into four classes, decided in this priority order:
//!
//! 1. Solid rule: a single run of one of "-=*", length >= 3, nothing else
//!    ("is_horizontal_rule"). A decorative banner/box frame. The bookend
//!    carve-out strips it, so it is the ONE art-shaped thing that is allowed to
//!    be rewritten. Everything below this line is preserved verbatim.
//! 2. Box wall: a line fenced on both ends by the same frame char: "|..|", or
//!    "* x *" with whitespace hugging the stars, which is
//!    "looks_like_ascii_art_frame_line". Whitespace is what separates a drawn
//!    wall from Markdown emphasis ("*emphasis*", "**bold**"), which has none.
//! 3. Art / table / fence / indented code: preserved verbatim. When uncertain,
//!    default here: corrupting a diagram is worse than failing to reflow a
//!    paragraph.
//! 4. Prose: everything else, and the only class that reflows.
//!
//! ## Why each magic constant is what it is (change one => an invariant test
//! fails far away; this is the map of what each defends)
//!
//! - "has_alpha_word_min4" (4): an alphabetic run >= 4 means a real word, which
//!   vetoes the art classification. Below 4 ("foo", single letters) can be art
//!   labels ("* a *"), so they do not veto.
//! - "ascii_art_density" 40% ("art*100 > total*40"): a line that is >40%
//!   "+-|/\><^v*=" (ignoring spaces) is a drawing. Below 40% it is prose with
//!   incidental punctuation.
//! - "unicode_art_density" "art_count >= 3" OR "max_run >= 2": box-drawing/
//!   block/arrow/braille glyphs cluster in diagrams; a couple in a row, or three
//!   anywhere, is enough to pin the line as art.
//! - fence run ">= 3": CommonMark fence length. Callers gate on it explicitly.
//! - "is_horizontal_rule" len ">= 3": a rule needs at least three marks to read
//!   as a divider rather than an ellipsis or operator ("--", "==").
//! - star box wall len ">= 4": "* x *" is the shortest wall that is
//!   unambiguously a frame and not "**" / "* *" noise.

/// Leading run of identical backtick or tilde fence characters, if any. A
/// backtick fence with another backtick later in the line is rejected (that is
/// inline code, not a fence). Callers gate on a run length of 3 or more.
pub(crate) fn fence_marker_run(body: &str) -> Option<(char, usize)> {
    let trimmed = body.trim_start_matches([' ', '\t']);
    let first = trimmed.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let run = trimmed.chars().take_while(|c| *c == first).count();
    if first == '`' {
        let after = &trimmed[run..];
        if after.contains('`') {
            return None;
        }
    }
    Some((first, run))
}

pub(crate) fn is_table_row(body: &str) -> bool {
    // Legend rows like "+ means available" or "s = slow": a single +/-/s
    // marker, a two-space gutter, then a definition keyword. These head up tab-
    // or pipe-aligned tables and must pass through, not wrap as prose.
    let legend = body.trim_start();
    if let Some(first) = legend.chars().next()
        && matches!(first, '+' | '-' | 's')
        && legend[first.len_utf8()..].starts_with("  ")
        && [" means ", " = ", " : ", " - "]
            .iter()
            .any(|kw| legend.contains(kw))
    {
        return true;
    }

    let t = body.trim();
    if t.contains("+--") {
        return true;
    }
    // Pipe-delimited columns, or whitespace columns aligned with a tab.
    t.matches('|').count() >= 2 || (t.contains('\t') && t.split_whitespace().count() >= 3)
}

/// Narrows a row mask to the rows that sit in a maximal run of two or more
/// adjacent ones: the shape that freezes instead of packing into a paragraph.
///
/// Requiring two is what keeps a sentence out. A lone "Note: ..." or
/// "count = zero when the queue drained" is prose that happens to open like a
/// row, and wrapping it is correct. Two or more adjacent rows are not one
/// wrapped sentence, so the run freezes whole and whatever follows is simply
/// the next paragraph -- releasing its last row to a prose tail would split
/// one logical table, which is the damage this rule exists to prevent.
pub(crate) fn keep_complete_row_runs(is_row: &mut [bool]) {
    let mut prev = false;
    for i in 0..is_row.len() {
        let cur = is_row[i];
        let next = is_row.get(i + 1).copied().unwrap_or(false);
        is_row[i] = cur && (prev || next);
        prev = cur;
    }
}

/// One "X = Y" mapping row: a compact assignment or formula that documents a
/// mapping rather than stating a sentence, so its one-row layout is the
/// content. "ir->imm = immediate", "flags |= MASK", "imm   = value".
///
/// Shared by the classify stage and the merge stage, which must agree byte
/// for byte on what a mapping row is.
///
/// What keeps prose out is the left-hand side: it must be exactly one
/// code-like token. A sentence that happens to contain an equals sign
/// ("the default width = 80 columns") has spaces on the left and is rejected
/// here; a sentence that happens to START with one ("count = zero when the
/// queue drained") is rejected by the run rule at the call site, not here.
pub(crate) fn is_assignment_line(body: &str) -> bool {
    let t = body.trim();

    // The operator is the first '='. If it turns out to compare rather than
    // assign, the '=' it leaves behind on the left fails the token test below,
    // so no later '=' could rescue the line anyway.
    let Some((lhs, rhs)) = t.split_once('=') else {
        return false;
    };

    // "==" / "===" compare, "=>" is an arrow and "=<" is nothing at all; a
    // right side of nothing is not a mapping either. Tested unspaced on
    // purpose, so a genuine value like "x = <unknown>" still reads as one.
    if rhs.starts_with(['=', '>', '<']) || rhs.trim().is_empty() {
        return false;
    }

    let b = lhs.as_bytes();
    let last = b.last().copied();
    let prev = b.len().checked_sub(2).map(|j| b[j]);
    let left = match last {
        // "!=", and bare "<=" / ">=", compare; "<<=" / ">>=" assign.
        Some(b'!') => return false,
        Some(c @ (b'<' | b'>')) if prev != Some(c) => return false,
        Some(b'<' | b'>') => &lhs[..lhs.len() - 2],
        // Compound assignment: "+=", "-=", "*=", "/=", "%=", "|=", "&=", "^=".
        Some(b'+' | b'-' | b'*' | b'/' | b'%' | b'|' | b'&' | b'^') => &lhs[..lhs.len() - 1],
        _ => lhs,
    }
    .trim_end();

    // One token, code-shaped, and short enough to be an identifier rather than
    // a clause. Every accepted byte is ASCII, so the byte length is the char
    // count; the cap is a sanity bound, not a real limit, and has to clear
    // names like "ctx->sub->field[MAX_INDEX]".
    !left.is_empty()
        && left.len() <= 48
        && left.bytes().any(|c| c.is_ascii_alphabetic() || c == b'_')
        && left
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.->:[]()*%$".contains(&c))
}

pub(crate) fn is_indented_code(body: &str) -> bool {
    // Intentional content alignment: ≥2 leading spaces OR ≥1 leading tab. A
    // single leading ASCII space is too common as a wrap artifact (it can
    // appear after reflow on round-trip 2), so single-space lines are NOT
    // pinned as preformatted. Two-or-more spaces, or any tab, indicate the
    // author deliberately indented for visual alignment.
    let mut spaces = 0usize;
    let mut tabs = 0usize;
    for c in body.chars() {
        match c {
            ' ' => spaces += 1,
            '\t' => tabs += 1,
            _ => break,
        }
    }
    tabs >= 1 || spaces >= 2
}

/// True when a block-comment opener begins a documentation comment ("/**" or
/// "/*!") rather than a banner. Matches Doxygen: "/***" (three or more stars)
/// is NOT a doc block, it's a decorative frame, so a star run stays a plain
/// block and can be stripped by the bookend carve-out.
pub(crate) fn block_is_doc(text: &str) -> bool {
    match text.strip_prefix("/**") {
        Some(rest) => !rest.starts_with('*') && !rest.starts_with('/'),
        None => text.starts_with("/*!"),
    }
}

/// A solid decorative rule: a single run of "-", "=", or "*" (length >= 3) and
/// nothing else. These are banner/box frame lines, which the bookend carve-out
/// strips, so they must NOT be pinned as art-to-preserve. A genuine art line
/// mixes characters or spacing ("*  *", "+--+") and fails this test.
pub(crate) fn is_horizontal_rule(body: &str) -> bool {
    let t = body.trim();
    let mut chars = t.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    matches!(first, '-' | '=' | '*') && t.chars().count() >= 3 && t.chars().all(|c| c == first)
}

pub(crate) fn is_art(body: &str) -> bool {
    if has_alpha_word_min4(body) {
        return false;
    }

    // An assignment row's operator is content, not drawing. "=" is a member of
    // the art alphabet, so a short mapping row like "a += b" clears the density
    // threshold on the very characters that make it a row, and art would claim
    // it after the run rule had deliberately declined to freeze it -- stranding
    // the tail of the paragraph it belongs to, the damage the run rule exists
    // to avoid. Whether a mapping row freezes is the run rule's alone. A
    // rule-decorated line is excepted: released to prose, the packer can join
    // "x += y ---" to a "--- label" above it and build a line ruled on both
    // ends, which the next pass strips. Different bytes each pass, and
    // "--check" would never settle. Decoration outranks the mapping reading.
    if is_assignment_line(body) && bookend_match(body).is_none() && !one_sided_banner(body) {
        return false;
    }
    if ascii_art_density(body) {
        return true;
    }
    unicode_art_density(body)
}

fn has_alpha_word_min4(body: &str) -> bool {
    // is_alphabetic (Unicode-aware) covers non-English prose: French
    // "éclatant", German "über", Chinese hanzi, etc. ASCII-only would falsely
    // classify accented prose as ASCII art.
    let mut run = 0usize;
    for c in body.chars() {
        if c.is_alphabetic() {
            run += 1;
            if run >= 4 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

fn ascii_art_density(body: &str) -> bool {
    const ART: &str = "+-|/\\><^v*=";
    let mut total = 0usize;
    let mut art = 0usize;
    for c in body.chars() {
        if c == ' ' || c == '\t' {
            continue;
        }
        total += 1;
        if ART.contains(c) {
            art += 1;
        }
    }
    if total == 0 {
        return false;
    }
    art * 100 > total * 40
}

fn unicode_art_density(body: &str) -> bool {
    let mut art_count = 0usize;
    let mut max_run = 0usize;
    let mut run = 0usize;
    for c in body.chars() {
        if is_unicode_art_char(c) {
            art_count += 1;
            run += 1;
            if run > max_run {
                max_run = run;
            }
        } else {
            run = 0;
        }
    }
    art_count >= 3 || max_run >= 2
}

fn is_unicode_art_char(c: char) -> bool {
    let v = c as u32;
    matches!(v,
        0x2500..=0x257F | // box-drawing
        0x2580..=0x259F | // block elements
        0x25A0..=0x25FF | // geometric shapes
        0x2190..=0x21FF | // arrows
        0x2800..=0x28FF   // braille
    )
}

/// Tab width for the width estimates taken before "IndentConfig" is in hand
/// (the single-line fast path, the label-run budget). Over-counting only
/// costs a reflow that could have been skipped, which is safe.
pub(crate) const FAST_PATH_TAB_WIDTH: usize = 8;

/// Advance a column counter by one character. Tabs jump to the next multiple
/// of "tab_width" (clang-format / terminal tab-stop semantics), not "col +
/// tab_width". Unicode wide characters use their display width.
pub fn advance_col(col: usize, c: char, tab_width: usize) -> usize {
    if c == '\t' {
        ((col / tab_width) + 1) * tab_width
    } else {
        col + unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
    }
}

pub(crate) enum BookendKind {
    Labeled(String),
    Bare,
}

pub(crate) fn bookend_match(body: &str) -> Option<BookendKind> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    let is_bookend_char = |c: char| c == '-' || c == '=' || c == '*';

    // A run counts as decoration only when it is *uniform*. A mixed run carries
    // meaning: "-*-" is the Emacs file-variables marker, and "/* -*- Mode: C;
    // tab-width: 4 -*- */" is a directive, not a banner. Collapsing it silently
    // drops the file's editor settings.
    let uniform = |run: &[char]| run.windows(2).all(|w| w[0] == w[1]);

    let chars: Vec<char> = trimmed.chars().collect();
    let lead_end = chars
        .iter()
        .position(|&c| !is_bookend_char(c))
        .unwrap_or(chars.len());
    if lead_end == chars.len() {
        // The whole line is rule characters. It is decoration when it is a
        // uniform run, optionally behind leftover "*" continuation markers:
        // "strip_block_continuation" only removes a "*" that is followed by
        // whitespace, so the very common " *------------" rule keeps its star
        // in the body and would fail a naive uniformity test.
        //
        // The test is needed here and not just on the labeled form below,
        // because reflow routinely wraps a mode line and leaves its closing
        // "-*-" alone on a line. Reading that as a bare rule deletes it, and
        // the next pass then sees different bytes, so the file never settles.
        let stars = chars.iter().take_while(|&&c| c == '*').count();
        let rule = if stars == chars.len() {
            &chars[..]
        } else {
            &chars[stars..]
        };
        return if chars.len() >= 3 && uniform(rule) {
            Some(BookendKind::Bare)
        } else {
            None
        };
    }

    // A banner is bracketed on BOTH sides. A rule on one side only is prose:
    // "--- a/kernel/sched.c" is a diff header quoted in a comment, and a line
    // ending in "---" is overwhelmingly a sentence wrapped right after an
    // em-dash written as three hyphens ("a fundamentally unsound strategy ---"
    // in apr_pools.h). Collapsing either one deletes bytes the author wrote.
    if lead_end < 3 || !chars[lead_end].is_whitespace() {
        return None;
    }
    let mut trail_start = chars.len();
    while trail_start > 0 && is_bookend_char(chars[trail_start - 1]) {
        trail_start -= 1;
    }
    let trail_len = chars.len() - trail_start;
    if trail_len < 3 || trail_start == 0 || !chars[trail_start - 1].is_whitespace() {
        return None;
    }

    if !uniform(&chars[..lead_end]) || !uniform(&chars[trail_start..]) {
        return None;
    }
    let label: String = chars[lead_end..trail_start].iter().collect();
    let label = label.trim();
    if label.is_empty() {
        return None;
    }

    // A "label" that is itself rule characters means the whole line is rules: a
    // table's separator row ("------- ------ -------"), not a banner around a
    // word. Calling it Labeled collapses the row to its middle run, which then
    // reads as a setext underline and splits the table's header off into its
    // own paragraph, a split the next run cannot see, so the file never settles
    // (AvailabilityMacros.h). It is decoration end to end, which is what Bare
    // means.
    if label
        .chars()
        .all(|c| is_bookend_char(c) || c.is_whitespace())
    {
        return Some(BookendKind::Bare);
    }
    Some(BookendKind::Labeled(label.to_string()))
}

/// A banner whose rule run sits on ONE side only: "label -------" or
/// "------- label".
///
/// "bookend_match" deliberately refuses these, because a run on one side is
/// often content ("--- a/kernel/sched.c"), so collapsing it would delete bytes.
/// That leaves the line as ordinary prose, which reflow then wraps, and the
/// wrap is where the damage happens: the rule run lands alone on the next line,
/// the following pass reads that line as a bare rule and deletes it, and the
/// file settles only on its second run. ICU's utf8.h hits this at the default
/// column limit. Freezing the line fixes the convergence hole and is better
/// output anyway, since a decorative rule split across two lines is not a shape
/// anyone asked for.
///
/// Caller's job: only freeze a line that stands ALONE in its paragraph. Inside
/// a paragraph a trailing "---" is usually a sentence wrapped right after an
/// em-dash written as three hyphens, and freezing that strands the rest of it.
pub(crate) fn one_sided_banner(body: &str) -> bool {
    // An all-rule line, and a run bracketed on both sides, are bookend_match's
    // business. This is only the shapes it hands back as prose.
    if bookend_match(body).is_some() {
        return false;
    }
    let is_rule = |c: char| c == '-' || c == '=' || c == '*';
    let uniform = |run: &[char]| run.windows(2).all(|w| w[0] == w[1]);
    let chars: Vec<char> = body.trim().chars().collect();

    let lead = chars.iter().take_while(|&&c| is_rule(c)).count();
    let trail = chars.iter().rev().take_while(|&&c| is_rule(c)).count();

    let leads =
        lead >= 3 && uniform(&chars[..lead]) && chars.get(lead).is_some_and(|c| c.is_whitespace());
    let trails = trail >= 3
        && uniform(&chars[chars.len() - trail..])
        && chars
            .len()
            .checked_sub(trail + 1)
            .and_then(|before| chars.get(before))
            .is_some_and(|c| c.is_whitespace());
    leads || trails
}

/// True when a stripped-line body is art-like and non-empty after trim. Used
/// by the bookend-strip adjacency check to protect multi-line ASCII drawings
/// ("+----+", "|  |", "| label |") from getting their bordering dash runs
/// collapsed.
pub(crate) fn line_is_art_only(body: &str) -> bool {
    let t = body.trim();
    if t.is_empty() {
        return false;
    }
    !t.chars().any(char::is_alphabetic) || looks_like_ascii_art_frame_line(t)
}

fn looks_like_ascii_art_frame_line(t: &str) -> bool {
    // Box side-walls: a line fenced on both ends by the same frame char. "|..|"
    // is the classic table/diagram wall; "* .. *" is a star-box label row ("*
    // Decorative *"). Recognizing these as art cancels the bookend strip on the
    // rule lines above/below, so a framed label box survives while a star rule
    // bracketing plain prose (no side frame) still collapses.
    //
    // The star wall requires whitespace inside the stars ("* x *"), which is
    // what separates a drawn box from Markdown emphasis ("*emphasis*",
    // "**bold**"): emphasis has no space hugging the marker, so it never
    // cancels a neighboring bookend.
    let pipe_wall = t.len() >= 2 && t.starts_with('|') && t.ends_with('|');
    let star_wall = t.len() >= 4 && t.starts_with("* ") && t.ends_with(" *");
    pipe_wall || star_wall
}

/// A line body of the form "@name:" or "@name :", the kernel-doc parameter
/// shape convert_kernel_doc produces. "name" is "[A-Za-z0-9_]+".
pub(crate) fn is_kernel_doc_tag(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('@') else {
        return false;
    };
    let name_len = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .count();
    if name_len == 0 {
        return false;
    }
    // "name" is all ASCII, so its char count equals its byte length.
    rest[name_len..].trim_start().starts_with(':')
}

/// Split a Prose paragraph body at every sentence-ending punctuation that is
/// immediately followed by a Return/Returns/@return/@returns/\return/\returns
/// token. ATX header forms "# Returns" are intentionally excluded, since "#"
/// mid-prose is not a header marker.
pub(crate) fn split_at_return_boundary(body: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut last = 0usize;
    let bytes = body.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if !matches!(b, b'.' | b'!' | b'?') {
            continue;
        }

        // "j > i + 1" also covers the end of the body: there the run of spaces
        // is empty, so no boundary can follow.
        let mut j = i + 1;
        while j < bytes.len() && matches!(bytes[j], b' ' | b'\t') {
            j += 1;
        }
        if j > i + 1
            && is_return_token_start(&body[j..])
            && !is_list_ordinal_dot(body, i, b as char)
            && !is_inside_bracket_or_quote(body, i)
        {
            segments.push(body[last..j].trim().to_string());
            last = j;
        }
    }
    let tail = body[last..].trim();
    if !tail.is_empty() {
        segments.push(tail.to_string());
    }
    if segments.is_empty() {
        segments.push(body.to_string());
    }
    segments
}

/// True when "pos" sits inside a parenthetical or a quotation. A sentence end
/// there is not a paragraph boundary: splitting orphans the closing delimiter
/// onto a new paragraph, so "(the allocator can fail. Return codes are listed
/// above)" would break into two paragraphs with a stray ")" opening the second.
/// Found by running this tool on its own source, where a doc comment quoting an
/// example ("2. Return the pointer") was split apart mid-quote.
///
/// Only the double-quote character counts as a quote. An apostrophe in
/// ordinary prose ("don't") would otherwise toggle the state and suppress every
/// later split in the paragraph.
fn is_inside_bracket_or_quote(body: &str, pos: usize) -> bool {
    let mut depth = 0i32;
    let mut in_quote = false;
    let mut escaped = false;
    for ch in body[..pos].chars() {
        match ch {
            '\\' => {
                escaped = !escaped;
                continue;
            }
            '"' if !escaped => in_quote = !in_quote,
            '(' | '[' | '{' if !in_quote => depth += 1,
            ')' | ']' | '}' if !in_quote => depth = (depth - 1).max(0),
            _ => {}
        }
        escaped = false;
    }

    // An opener with no closer after "pos" was never a region. Prose routinely
    // writes a lone inch mark or a lone opening paren that never pairs up;
    // requiring the closer keeps one stray delimiter from disabling the Return
    // rule for every later sentence in the paragraph.
    let rest = &body[pos..];
    if in_quote {
        return rest.contains('"');
    }
    depth > 0 && rest.contains([')', ']', '}'])
}

/// True when the "." at "dot" ends a numbered-list marker ("1.", "2.") rather
/// than a sentence. Without this, an enumerated step whose text happens to
/// start with the return keyword splits at the marker, stranding a bare "2." on
/// its own line, and the next run repacks it, so the same input keeps
/// flip-flopping.
///
/// The marker must open the body: normalize hands each list item to reflow as
/// its own paragraph, so a genuine ordinal is always at offset 0. A digit run
/// anywhere else is a number ending a sentence ("the frame sits at 42. Return
/// the pointer"), which the documented Return rule still splits. Only "." forms
/// an ordinal; "!"/"?" never do.
fn is_list_ordinal_dot(body: &str, dot: usize, c: char) -> bool {
    if c != '.' || dot == 0 {
        return false;
    }
    body.as_bytes()[..dot].iter().all(u8::is_ascii_digit)
}

fn is_return_token_start(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix("Return") {
        let stripped = rest.strip_prefix('s').unwrap_or(rest);
        return stripped
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
    }
    if let Some(rest) = s.strip_prefix(['@', '\\']) {
        // Try the longer prefix first: "returns" before "return". Otherwise
        // "@returns x" strips to "s x", fails the boundary check, and the split
        // never fires for the plural Doxygen form.
        let stripped = rest
            .strip_prefix("returns")
            .or_else(|| rest.strip_prefix("return"));
        if let Some(stripped) = stripped {
            return stripped
                .chars()
                .next()
                .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_runs_and_inline_code() {
        assert_eq!(fence_marker_run("```rust"), Some(('`', 3)));
        assert_eq!(fence_marker_run("~~~~"), Some(('~', 4)));
        assert_eq!(fence_marker_run("   ```"), Some(('`', 3)));
        // A backtick later in the line means inline code, not a fence.
        assert_eq!(fence_marker_run("`x` and `y`"), None);
        assert_eq!(fence_marker_run("prose"), None);
        // Length is reported, not judged: callers gate on 3 or more.
        assert_eq!(fence_marker_run("``"), Some(('`', 2)));
    }

    #[test]
    fn table_rows_versus_prose() {
        assert!(is_table_row("| a | b |"));
        assert!(is_table_row("+--+--+"));
        assert!(is_table_row("s  = slow"));
        assert!(is_table_row("col\tcol\tcol"));
        assert!(!is_table_row("a | b"));
        assert!(!is_table_row("ordinary prose"));
    }

    #[test]
    fn indented_code_needs_two_spaces_or_a_tab() {
        assert!(is_indented_code("  sample()"));
        assert!(is_indented_code("\tsample()"));
        // One space is a wrap artifact, not deliberate alignment.
        assert!(!is_indented_code(" continued prose"));
        assert!(!is_indented_code("flush prose"));
    }

    #[test]
    fn doc_openers_versus_star_banners() {
        assert!(block_is_doc("/** doc */"));
        assert!(block_is_doc("/*! doc */"));
        assert!(!block_is_doc("/*** banner */"));
        assert!(!block_is_doc("/* plain */"));
        assert!(!block_is_doc("/**/"));
    }

    #[test]
    fn horizontal_rules_are_uniform_runs() {
        assert!(is_horizontal_rule("---"));
        assert!(is_horizontal_rule("======"));
        assert!(is_horizontal_rule("***"));
        assert!(!is_horizontal_rule("--"), "two marks read as a dash");
        assert!(!is_horizontal_rule("-=-"), "mixed run is not a rule");
        assert!(!is_horizontal_rule("+--+"), "art, not a rule");
    }

    #[test]
    fn art_density_thresholds() {
        // A word of four letters vetoes the art reading.
        assert!(!is_art("when a > b && c < d, return early"));
        assert!(is_art("+--+--+"));
        assert!(is_art("|  ^  |"));
        assert!(is_art("┌─┐"), "unicode box drawing");
        assert!(!is_art("the symbol arrow means yields"));
    }

    #[test]
    fn bookend_shapes() {
        assert!(matches!(
            bookend_match("---- Section ----"),
            Some(BookendKind::Labeled(l)) if l == "Section"
        ));
        assert!(matches!(bookend_match("======"), Some(BookendKind::Bare)));

        // One-sided runs are content, whichever side they sit on: a quoted diff
        // header, or a sentence wrapped after a three-hyphen em-dash.
        assert!(bookend_match("--- a/kernel/sched.c").is_none());
        assert!(bookend_match("a fundamentally unsound strategy ---").is_none());

        // A mixed run carries meaning (the Emacs mode marker), whether it
        // brackets a label or stands alone. Reflow routinely wraps a mode line
        // so its closing "-*-" ends up on a line of its own; reading that as a
        // bare rule deletes it and the file never settles.
        assert!(bookend_match("-*- Mode: C -*-").is_none());
        assert!(bookend_match("-*-").is_none());
        assert!(bookend_match("-=-=-=").is_none());

        // A star glued to the front of a rule is a continuation marker that
        // "strip_block_continuation" could not remove (it only strips a "*"
        // followed by whitespace), not a mixed run. The rule still collapses.
        assert!(matches!(
            bookend_match("*--------"),
            Some(BookendKind::Bare)
        ));
        assert!(matches!(bookend_match("**======"), Some(BookendKind::Bare)));
        // All-rule "label" means the whole line is decoration.
        assert!(matches!(
            bookend_match("------- ------ -------"),
            Some(BookendKind::Bare)
        ));
    }

    #[test]
    fn columns_advance_to_tab_stops() {
        assert_eq!(advance_col(0, '\t', 8), 8);
        assert_eq!(advance_col(3, '\t', 8), 8, "tab stops, not +8");
        assert_eq!(advance_col(8, '\t', 8), 16);
        assert_eq!(advance_col(0, 'a', 8), 1);
        assert_eq!(advance_col(0, '你', 8), 2, "wide characters cost two");
    }

    #[test]
    fn kernel_doc_tag_form() {
        assert!(is_kernel_doc_tag("@name : desc"));
        assert!(is_kernel_doc_tag("@name: desc"));
        assert!(is_kernel_doc_tag("@buf_2:"));
        assert!(!is_kernel_doc_tag("@ : desc"));
        assert!(!is_kernel_doc_tag("@name desc"));
        assert!(!is_kernel_doc_tag("plain prose"));
    }

    #[test]
    fn return_boundary_splits() {
        assert_eq!(
            split_at_return_boundary("Does the thing. Return the count."),
            ["Does the thing.", "Return the count."]
        );
        assert_eq!(
            split_at_return_boundary("Does the thing. @returns the count."),
            ["Does the thing.", "@returns the count."]
        );
        // A list ordinal is not a sentence end.
        assert_eq!(
            split_at_return_boundary("2. Return the pointer"),
            ["2. Return the pointer"]
        );
        // Inside a parenthetical or a quotation, the closer would be orphaned.
        assert_eq!(
            split_at_return_boundary("(it can fail. Return codes are listed above)"),
            ["(it can fail. Return codes are listed above)"]
        );
        // An opener that never closes is not a region, so the split still runs.
        assert_eq!(
            split_at_return_boundary("the buffer is 4\" wide. Return the width"),
            ["the buffer is 4\" wide.", "Return the width"]
        );
        // "Returning" is not the keyword.
        assert_eq!(
            split_at_return_boundary("Does the thing. Returning early is fine."),
            ["Does the thing. Returning early is fine."]
        );
    }

    #[test]
    fn art_only_lines_protect_their_neighbors() {
        assert!(line_is_art_only("+----+"));
        assert!(line_is_art_only("|      |"));
        assert!(line_is_art_only("* label *"), "star box wall");
        assert!(!line_is_art_only("ordinary prose"));
        assert!(!line_is_art_only(""));
    }
}
