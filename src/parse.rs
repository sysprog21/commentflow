use anyhow::{Result, anyhow, bail};
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::{Node, Parser, Tree};

use crate::signature::{collect_param_shifts, manpage_relocate_target};
use crate::textline::{
    block_is_doc, fence_marker_run, is_art, is_horizontal_rule, is_indented_code, is_table_row,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    C,
    Cpp,
    Rust,
    Shell,
    /// Assembly (".s" / ".S"). Scanned by "crate::asm", not tree-sitter:
    /// only "/* */" is a comment on every assembler target.
    Asm,
}

/// Every extension the tool accepts, lowercase, in the order the help text and
/// the README list them. The match in "detect_language" is still the authority
/// on which language each one maps to; this is the authority on the SET, so the
/// error message below and the two prose copies (the "--help" text and the
/// README) have something to be checked against. See
/// "extension_list_is_consistent" in tests/cli.rs.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "c", "h", "m", "cc", "cpp", "cxx", "c++", "hh", "hpp", "hxx", "h++", "mm", "rs", "sh", "bash",
    "s",
];

pub fn detect_language(path: &Path) -> Result<Language> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        // Objective-C (.m) and Objective-C++ (.mm) ride the C and C++ grammars:
        // they share the comment syntax and the Doxygen doc flavor, and
        // tree-sitter lexes comments as "extras", independently of grammar
        // state, so they still extract cleanly amid the ObjC-specific syntax
        // the C/C++ grammars cannot parse ("@interface", "[obj msg]").
        Some("c" | "h" | "m") => Ok(Language::C),
        Some("cc" | "cpp" | "cxx" | "c++" | "hh" | "hpp" | "hxx" | "h++" | "mm") => {
            Ok(Language::Cpp)
        }
        Some("rs") => Ok(Language::Rust),
        Some("sh" | "bash") => Ok(Language::Shell),

        // Assembly. The extension is case-folded above, so ".S"
        // (cpp-preprocessed, hand-written) and ".s" (usually compiler output)
        // are indistinguishable here and both ride the same scanner. Reflowing
        // a generated ".s" is pointless but harmless, and the idempotency
        // invariant still holds.
        Some("s") => Ok(Language::Asm),
        // Has an extension, just not one we support: never inspect content.
        Some(_) => bail!(
            "unsupported extension on path: {} (commentflow supports {}, matched case-insensitively)",
            path.display(),
            SUPPORTED_EXTENSIONS
                .iter()
                .map(|e| format!(".{e}"))
                .collect::<Vec<_>>()
                .join(" ")
        ),

        // No extension: the one content sniff allowed. A POSIX-shell shebang on
        // line 1 makes it a Shell file. Never promotes a non-shell shebang.
        None => {
            if first_line_is_posix_shell_shebang(path) {
                Ok(Language::Shell)
            } else {
                bail!(
                    "no extension and no recognized POSIX-shell shebang: {}",
                    path.display()
                )
            }
        }
    }
}

/// Read at most the first line of "path" and report whether it is a POSIX-shell
/// shebang ("#!/bin/sh", "#!/bin/bash", "#!/usr/bin/env bash", …). Used ONLY
/// for extensionless files. Bounded read so a giant no-newline file can't be
/// slurped whole; non-UTF-8 line-1 bytes simply mean "not a shebang".
fn first_line_is_posix_shell_shebang(path: &Path) -> bool {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };

    // "take(256)" caps the read regardless of how "read" chunks, so a giant
    // no-newline file can never be slurped whole.
    let mut buf = Vec::with_capacity(256);
    if f.take(256).read_to_end(&mut buf).is_err() {
        return false;
    }
    let line_end = buf.iter().position(|&b| b == b'\n').unwrap_or(buf.len());
    std::str::from_utf8(&buf[..line_end]).is_ok_and(is_posix_shell_shebang_line)
}

/// A shebang names a POSIX shell when the interpreter basename (or the "env"
/// argument) is "sh" or "bash". Other interpreters ("zsh", "python", …) and
/// non-shebang lines are rejected. Extension support is unchanged; this only
/// recognizes the two shells the tool already handles.
fn is_posix_shell_shebang_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("#!") else {
        return false;
    };
    let mut tokens = rest.split_whitespace();
    let Some(first) = tokens.next() else {
        return false;
    };
    let basename = |t: &str| {
        Path::new(t)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(t)
            .to_string()
    };
    let interp = basename(first);
    let name = if interp == "env" {
        // Skip env options ("-S", "-i", …) to reach the interpreter token, so
        // "#!/usr/bin/env -S bash" is recognized.
        match tokens.find(|t| !t.starts_with('-')) {
            Some(arg) => basename(arg),
            None => return false,
        }
    } else {
        interp
    };
    matches!(name.as_str(), "sh" | "bash")
}

#[derive(Debug, Clone)]
pub struct Comment {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_col_byte: usize,
    pub text: String,
    pub force_passthrough: bool,
    /// True when non-whitespace bytes precede the comment marker on the same
    /// source line (e.g., "int x; // foo"). A merely indented comment whose
    /// preceding bytes are all whitespace is *not* trailing.
    pub is_trailing: bool,
    /// Verbatim leading-whitespace bytes on the comment's source line, from
    /// line-start up to the comment marker. Tabs stay tabs; spaces stay
    /// spaces. Used as the indent template for continuation/closer lines
    /// emitted by reflow, so the source's tab-or-space choice survives.
    pub line_indent_bytes: String,
    /// Line ending to use when the comment carries no interior break to copy
    /// (a single-source-line comment that reflow wraps). Resolved at extract
    /// time: the byte(s) immediately after the comment span, else the file's
    /// dominant ending, else LF. Multi-line comments ignore this and vote
    /// their own interior endings.
    pub fallback_ending: &'static str,
    /// True when nothing but an optional UTF-8 BOM precedes the comment's
    /// first byte, i.e. it sits at file offset 0. Drives the canonical
    /// block-comment form: a file-start block uses opener-alone ("/*" on its
    /// own line), every other block uses the inline ("/* word") form. This is
    /// position-based, not source-driven.
    pub at_file_start: bool,
    /// Set for a C/C++ block comment that sits between a function's signature
    /// and its "{" body, the classic X11 manual-page placement. Holds the
    /// byte offset of the enclosing function's line start (column 0), the
    /// single anchor for the relocation: "plan" splices the cleaned comment
    /// here, "normalize" reflows it with zero indent to match column 0, and
    /// "rewrite::validate" authorizes the zero-width insert only at this
    /// offset. "None" for every comment not in that position.
    pub relocate_before: Option<usize>,
    /// Set for a parameter comment in a C/C++ signature whose comments all
    /// drifted forward by one (see "collect_param_shifts"). Says where the
    /// verbatim comment is re-inserted (trailing the parameter it actually
    /// describes) and which way to delete it from its drifted spot. "None"
    /// for every other comment.
    pub param_shift: Option<ParamShift>,
}

/// How a drifted parameter comment moves. "insert_at" is the byte offset where
/// the verbatim comment is re-spliced (the end of the parameter's declarator);
/// "after_paren" is true for the comment trailing ")" (delete the whitespace
/// gap *before* the comment, keeping ")"), false for a leading "/* c */ TYPE"
/// comment (delete the gap *after* it, up to the type). The direction matters
/// so that adjacent comments in a multi-comment group delete disjoint spans
/// instead of fighting over the space between them.
#[derive(Debug, Clone, Copy)]
pub struct ParamShift {
    pub insert_at: usize,
    pub after_paren: bool,
}

impl Comment {
    /// For block comments only: "Some(true)" when the source's first line is
    /// exactly the opener marker with no following content (opener-alone),
    /// "Some(false)" when content follows on the same line (inline), "None"
    /// for line-comment styles. Doc blocks keep this source-driven opener
    /// choice; plain blocks use position ("at_file_start") instead.
    pub fn style_opener_alone(&self) -> Option<bool> {
        let first_line = self.text.split('\n').next()?;
        let trimmed = first_line.trim_end_matches('\r').trim_end();
        for marker in ["/**", "/*!", "/*"] {
            if trimmed == marker {
                return Some(true);
            }
            if trimmed.starts_with(marker) {
                return Some(false);
            }
        }
        None
    }
}

pub fn extract_comments(source: &str, lang: Language) -> Result<Vec<Comment>> {
    let mut pool = ParserPool::new();
    extract_comments_with(source, lang, &mut pool)
}

/// Reusable per-language "tree_sitter::Parser" cache. Creating a parser and
/// calling "set_language" is cheap individually (~ms) but adds up across a
/// batch run over many files. A pool kept on the caller's stack amortizes
/// the cost: the parser is built lazily on first use of each language and
/// reused for every subsequent file of the same language.
#[derive(Default)]
pub struct ParserPool {
    c: Option<Parser>,
    cpp: Option<Parser>,
    rust: Option<Parser>,
    shell: Option<Parser>,
}

impl ParserPool {
    pub fn new() -> Self {
        Self::default()
    }

    fn parser_for(&mut self, lang: Language) -> Result<&mut Parser> {
        let slot = match lang {
            // Assembly has no grammar; "extract_comments_with" scans it
            // directly and never reaches the pool.
            Language::Asm => bail!("assembly is scanned directly, not parsed"),
            Language::C => &mut self.c,
            Language::Cpp => &mut self.cpp,
            Language::Rust => &mut self.rust,
            Language::Shell => &mut self.shell,
        };
        if slot.is_none() {
            let mut parser = Parser::new();
            parser
                .set_language(&grammar(lang)?)
                .map_err(|e| anyhow!("set_language failed: {e}"))?;
            *slot = Some(parser);
        }
        Ok(slot.as_mut().expect("just initialized"))
    }
}

/// Pool-aware extractor. Same semantics as "extract_comments" but reuses
/// the per-language parser stored in "pool" across calls.
pub fn extract_comments_with(
    source: &str,
    lang: Language,
    pool: &mut ParserPool,
) -> Result<Vec<Comment>> {
    let mut comments = Vec::new();
    if lang == Language::Asm {
        // No grammar: a byte scanner over "/* */" (see "crate::asm"). None of
        // the C/C++-only transforms apply, so both move targets stay "None".
        // The preprocessor-directive guard is shared with the tree-sitter path:
        // a ".S" file is cpp-preprocessed, and a comment inside a "#define X \"
        // continuation must not be reflowed or blank-line-detached.
        for (start, end) in crate::asm::scan(source) {
            if is_inside_preprocessor_directive(start, source) {
                continue;
            }
            comments.push(make_comment(source, start, end, lang, None, None));
        }
    } else {
        let parser = pool.parser_for(lang)?;
        let tree: Tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow!("tree-sitter parse returned None"))?;

        let root = tree.root_node();
        let param_shifts = collect_param_shifts(root, source, lang);
        walk_collect(
            root,
            None,
            false,
            source,
            lang,
            &param_shifts,
            &mut comments,
        );
    }
    comments.sort_by_key(|c| c.start_byte);
    let mut merged = merge_comment_groups(comments, source);

    // Resolve the fallback ending now, while the file source is in hand: the
    // byte(s) right after the span, else the file's dominant ending (which
    // itself bottoms out at LF). Runs after merge so "end_byte" reflects the
    // final span of any merged group.
    let file_ending = dominant_line_ending(source);
    for c in &mut merged {
        let after = &source[c.end_byte..];
        c.fallback_ending = if after.starts_with("\r\n") {
            "\r\n"
        } else if after.starts_with('\n') {
            "\n"
        } else {
            file_ending
        };
    }
    Ok(merged)
}

/// Merge consecutive comment runs ("// foo\n// bar\n// baz" or adjacent
/// plain "/* ... */" blocks) into one synthetic Comment so the reflow
/// pipeline can repack the paragraph across the run rather than processing
/// each comment independently. Two adjacent comments merge when:
/// - they share the same marker style ("//", "///", "/*", etc.),
/// - they share the same start column (aligned indent),
/// - neither is trailing (only whitespace before the marker on each line),
/// - the bytes between them are exactly one newline plus optional
///   indentation that matches the next comment's indent.
fn merge_comment_groups(comments: Vec<Comment>, source: &str) -> Vec<Comment> {
    // Two-pass: first identify how far each group extends, then materialize the
    // merged text exactly once per group. The naive per-step rebuild was O(n²)
    // in total bytes copied; this is O(n) per group.
    let n = comments.len();
    let mut out: Vec<Comment> = Vec::with_capacity(n);
    let mut i = 0usize;
    while i < n {
        let mut j = i;
        while j + 1 < n && can_merge(&comments[j], &comments[j + 1], source) {
            j += 1;
        }
        if j == i {
            out.push(comments[i].clone());
            i += 1;
            continue;
        }
        let mut head = comments[i].clone();
        let final_end = comments[j].end_byte;
        head.end_byte = final_end;
        head.text = if marker_kind(&head.text) == Some(MarkerKind::PlainBlock) {
            merged_block_text(&comments[i..=j], &source[head.start_byte..final_end])
        } else {
            source[head.start_byte..final_end].to_string()
        };
        out.push(head);
        i = j + 1;
    }
    out
}

/// Distinct marker shapes for line-comment grouping. Hash-run kinds are
/// length-tagged because "# foo" and "## bar" must NOT merge into one
/// paragraph: shell convention treats the run length as a visual heading
/// level and merging would collapse the author's distinction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerKind {
    Slash3,
    SlashBang,
    Slash2,
    PlainBlock,
    /// Run length of leading "#" chars. Stored as "usize" so absurd inputs
    /// (256-hash, 65k-hash) never collide with smaller runs the way a
    /// fixed-width counter would saturate.
    HashRun(usize),
}

fn marker_kind(text: &str) -> Option<MarkerKind> {
    if text.starts_with("///") {
        Some(MarkerKind::Slash3)
    } else if text.starts_with("//!") {
        Some(MarkerKind::SlashBang)
    } else if text.starts_with("//") {
        Some(MarkerKind::Slash2)
    } else if text.starts_with("/*") && !block_is_doc(text) {
        Some(MarkerKind::PlainBlock)
    } else if text.starts_with("#!") {
        // Mid-file "#!" per kernel exec(2). Refuse to merge with regular "#"
        // comments since the prefix shape differs.
        None
    } else if text.starts_with('#') {
        let run = text.chars().take_while(|&c| c == '#').count();
        Some(MarkerKind::HashRun(run))
    } else {
        None
    }
}

fn can_merge(prev: &Comment, next: &Comment, source: &str) -> bool {
    if prev.force_passthrough || next.force_passthrough {
        return false;
    }
    if block_merge_has_preformatted(prev) || block_merge_has_preformatted(next) {
        return false;
    }
    if prev.is_trailing || next.is_trailing {
        return false;
    }
    let prev_marker = marker_kind(&prev.text);

    // An SPDX tag has to stay on the file's first line: that is where the
    // kernel's checker, "reuse", and scancode look for it. Merging "/*
    // SPDX-License-Identifier: ... */" into the header block below it turns
    // line 1 into a bare "/*" and moves the tag to line 2, which reads as an
    // unlicensed file.
    //
    // Scoped to a file-start *block* comment, which is the only shape that can
    // lose line 1. A "#" or "//" run is a series of one-line comments that
    // merge into one logical block; refusing there would fracture the run and
    // drop the separator lines inside it, and the tag keeps its own line either
    // way.
    if prev.at_file_start
        && prev_marker == Some(MarkerKind::PlainBlock)
        && prev.text.contains("SPDX-License-Identifier")
    {
        return false;
    }
    if prev_marker.is_none() || prev_marker != marker_kind(&next.text) {
        return false;
    }

    // "**" continuations are normalized to a single "*" everywhere now, so
    // merging blocks that use them (and downgrading the star count) is the
    // intended behavior, not a regression to guard against.
    if prev.start_col_byte != next.start_col_byte {
        return false;
    }
    if next.start_byte <= prev.end_byte {
        return false;
    }
    let between = &source[prev.end_byte..next.start_byte];

    // Exactly one newline plus optional whitespace, matching the next comment's
    // leading indent so the second source line starts at the same column as the
    // first.
    let Some(nl_pos) = between.find('\n') else {
        return false;
    };
    if !between[..nl_pos].trim().is_empty() {
        return false;
    }
    let next_indent = &between[nl_pos + 1..];
    if next_indent.contains('\n') {
        return false;
    }
    next_indent == next.line_indent_bytes
}

fn merged_block_text(comments: &[Comment], source_span: &str) -> String {
    let mut lines: Vec<String> = comments
        .iter()
        .flat_map(|c| block_body_lines(&c.text))
        .map(|line| line.trim().to_string())
        .collect();
    trim_blank_edges(&mut lines);

    if lines.is_empty() {
        return "/* */".to_string();
    }

    let eol = dominant_line_ending(source_span);

    // Position-based, matching "normalize": a merged block uses opener-alone
    // form only when the group itself starts at file offset 0.
    let opener_alone = comments.first().is_some_and(|c| c.at_file_start);

    // First physical line: the opener on its own (file-header style, every body
    // line becomes a continuation), or the opener fused with the first body
    // line (inline style). The rest are emitted identically either way.
    let mut out = String::new();
    let continuation_start = if opener_alone {
        out.push_str("/*");
        0
    } else {
        let first = &lines[0];
        if first.is_empty() {
            out.push_str("/*");
        } else {
            out.push_str("/* ");
            out.push_str(first);
        }
        1
    };
    for line in &lines[continuation_start..] {
        out.push_str(eol);
        if line.is_empty() {
            out.push_str(" *");
        } else {
            out.push_str(" * ");
            out.push_str(line);
        }
    }
    out.push_str(eol);
    out.push_str(" */");
    out
}

/// Drop leading and trailing all-blank lines in place.
fn trim_blank_edges(lines: &mut Vec<String>) {
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
}

fn dominant_line_ending(text: &str) -> &'static str {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count() - crlf;
    if crlf > lf { "\r\n" } else { "\n" }
}

fn block_body_lines(text: &str) -> Vec<String> {
    let raw: Vec<&str> = text.split('\n').collect();
    if raw.len() == 1 {
        let line = raw[0].trim_start();
        let body = line.strip_prefix("/*").unwrap_or(line);
        let body = body.strip_suffix("*/").unwrap_or(body).trim_end();
        let body = body.strip_prefix(' ').unwrap_or(body);
        return vec![body.to_string()];
    }

    let mut out: Vec<String> = raw
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let mut body = line.trim_start_matches([' ', '\t']);
            if i == 0 {
                body = body.strip_prefix("/*").unwrap_or(body).trim_start();
            }
            if i == raw.len() - 1 {
                body = body.strip_suffix("*/").unwrap_or(body).trim_end();
            }
            body = body.trim_start_matches([' ', '\t']);
            body = strip_block_line_prefix(body);
            body.trim_end().to_string()
        })
        .collect();
    trim_blank_edges(&mut out);
    out
}

fn block_merge_has_preformatted(c: &Comment) -> bool {
    marker_kind(&c.text) == Some(MarkerKind::PlainBlock)
        && block_body_lines(&c.text)
            .iter()
            .any(|line| block_line_is_preformatted(line))
}

fn block_line_is_preformatted(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }

    // A solid dash/equals/star rule is a banner frame, not art to preserve.
    // Letting it merge lets the bookend strip collapse the box to its content.
    if is_horizontal_rule(trimmed) {
        return false;
    }

    // Mostly the same predicates the normalize-stage classifier uses. The one
    // gap is solid rules, exempted just above: the classifier still pins them
    // as art, but "strip_decorative_bookends" resolves them before
    // classification runs, so the two stages never act on a rule at the same
    // time.
    is_table_row(trimmed)
        || fence_marker_run(trimmed).is_some_and(|(_, run)| run >= 3)
        || is_verbatim_open(trimmed)
        || is_indented_code(line)
        || is_art(trimmed)
}

/// clang-format's "// clang-format off" / "/* clang-format off */" toggle (and
/// its "on" counterpart) is load-bearing: this tool runs *before* clang-format,
/// so a directive merged into a neighbor or reflowed silently disables the
/// downstream formatter over the wrong span. Pin it as passthrough: never
/// merged, never reflowed. clang-format matches the body exactly (surrounding
/// whitespace aside), with an optional ": reason" suffix.
fn is_formatter_directive(text: &str) -> bool {
    let body = text
        .trim_start_matches(['/', '*'])
        .trim_end_matches(['/', '*'])
        .trim();
    matches!(body, "clang-format off" | "clang-format on")
        || body.strip_prefix("clang-format off:").is_some()
        || body.strip_prefix("clang-format on:").is_some()
}

/// cppcheck's inline suppressions ("cppcheck-suppress <id>" and the
/// "-begin"/"-end"/"-file"/"-macro" variants, plus the bracketed
/// "cppcheck-suppress[id]" form) are load-bearing machine directives, same as
/// clang-format's toggles: this tool runs *before* cppcheck, so wrapping the id
/// onto a continuation line or merging the comment into a neighbor detaches the
/// suppression from the guarded code line and cppcheck re-flags it. Pin the
/// whole comment as passthrough. Any content line may carry the directive (a
/// block form often opens with prose and puts the suppression below it), so we
/// scan them all. Over-matching a comment that merely mentions the token only
/// skips a reflow, which is the tool's "when in doubt, do not reflow" bias.
fn is_cppcheck_suppress(text: &str) -> bool {
    text.lines().any(|l| {
        l.trim()
            .trim_start_matches(['/', '*'])
            .trim()
            .strip_prefix("cppcheck-suppress")
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t', '[', '-']))
    })
}

/// Frama-C ACSL annotations are C comments with syntax inside ("/*@ ... */",
/// "//@ ..."). Reflowing or style-converting them changes parser-visible code,
/// so they are machine directives, not prose. (Splint's "/*@only@*/" etc. ride
/// the "/*@" prefix too: same rail, no extra keyword needed.)
fn is_acsl_annotation(text: &str) -> bool {
    text.starts_with("/*@") || text.starts_with("//@")
}

/// First keyword of a comment content line: comment markers ("//", "/*", "*",
/// "#") and surrounding space stripped, then the token up to the first space or
/// argument delimiter ("["/"("/":"/"="). "// NOLINT(rule)" → "NOLINT",
/// "# shellcheck disable=SC1" → "shellcheck", "* coverity[deref]" → "coverity".
fn directive_keyword(line: &str) -> &str {
    let body = line.trim().trim_start_matches(['/', '*', '#']).trim_start();
    let end = body
        .find(|c: char| c.is_whitespace() || matches!(c, '[' | '(' | ':' | '='))
        .unwrap_or(body.len());
    &body[..end]
}

/// Inline directives for the other analyzers that read comments the way
/// clang-format and cppcheck do: clang-tidy's "NOLINT" family, Coverity,
/// include-what-you-use, and SonarQube's "NOSONAR" (C/C++); shellcheck (shell);
/// cbindgen (Rust). commentflow runs *before* all of them, so wrapping or
/// merging one of these lands the suppression on the wrong line and it silently
/// misfires, the same failure mode as ACSL and clang-format toggles. Unlike
/// cppcheck's block form, every one of these is a single-line directive, so the
/// keyword is matched on the *first* line only: a multi-line prose block that
/// merely names "coverity"/"shellcheck"/etc. on an interior line still reflows.
fn is_lint_directive(text: &str, lang: Language) -> bool {
    let keywords: &[&str] = match lang {
        Language::C | Language::Cpp => &[
            "NOLINT",
            "NOLINTNEXTLINE",
            "NOLINTBEGIN",
            "NOLINTEND",
            "coverity",
            "IWYU",
            "NOSONAR",
        ],
        Language::Shell => &["shellcheck"],
        Language::Rust => &["cbindgen"],
        // No comment-reading analyzer targets assembly.
        Language::Asm => &[],
    };
    text.lines()
        .next()
        .is_some_and(|l| keywords.contains(&directive_keyword(l)))
}

/// A comment carrying machine syntax that a downstream analyzer reads back:
/// clang-format, cppcheck, Frama-C/Splint, clang-tidy, Coverity, IWYU, Sonar,
/// shellcheck, cbindgen. commentflow must never reflow, merge, relocate, or
/// blank-line-detach these. (The Rust nested-"/* */" case is a grammar
/// constraint, not a tool directive, so it stays inline at the one call site.)
pub(crate) fn is_passthrough_directive(text: &str, lang: Language) -> bool {
    is_formatter_directive(text)
        || is_cppcheck_suppress(text)
        || (matches!(lang, Language::C | Language::Cpp) && is_acsl_annotation(text))
        || is_lint_directive(text, lang)
}

/// True when a comment is pinned as passthrough for the ACSL reason and *only*
/// that reason, and its closing "*/" may be moved onto its own line.
///
/// "force_passthrough" is one bit standing for several unrelated reasons, so
/// "this is an ACSL annotation" does not mean "ACSL is why it is pinned". A
/// bare CR still makes any rewrite unsafe (every visual line after the first is
/// inside the node, so the emitted text is not what it looks like), and an
/// annotation carrying a cppcheck suppression on an interior line is pinned by
/// that suppression too. Each reason keeps its own veto.
///
/// The other two directive rails cannot co-occur with ACSL and are not tested
/// for: "is_formatter_directive" and "is_lint_directive" both read the FIRST
/// line, and both strip only "/" and "*" off it, so the "@" of "/*@" survives
/// and their keyword match can never fire on an annotation. Only cppcheck's
/// rail scans every line, which is what puts it within reach.
///
/// The last line must carry real annotation text before the "*/", and the
/// closer must not be spelled "@*/". Splint annotations ride the same "/*@"
/// prefix and "@*/" is Splint's REQUIRED closing delimiter, so splitting it
/// deletes the delimiter; the same spelling is the idiomatic "@"-marker ACSL
/// closer, where splitting only strands a bare "@" on a line of its own. One
/// rule covers both: a closer that already carries its marker is left alone.
///
/// Trailing annotations are excluded. "line_indent_bytes" is empty for a
/// comment that shares its line with code, so the closer would land at column 1
/// instead of under the opener's "*".
pub(crate) fn acsl_closer_split_allowed(source: &str, c: &Comment, lang: Language) -> bool {
    if !matches!(lang, Language::C | Language::Cpp) || !is_acsl_annotation(&c.text) {
        return false;
    }
    if c.is_trailing
        || spans_bare_cr(source, c.start_byte, c.end_byte)
        || is_cppcheck_suppress(&c.text)
    {
        return false;
    }
    let last = c.text.rsplit('\n').next().unwrap_or("").trim_end();
    if last.ends_with("@*/") {
        return false;
    }
    last.strip_suffix("*/")
        .is_some_and(|before| !before.trim().trim_matches(['@', '*']).is_empty())
}

fn is_verbatim_open(trimmed: &str) -> bool {
    ["@code", "\\code", "@verbatim", "\\verbatim"]
        .iter()
        .any(|marker| {
            trimmed == *marker
                || trimmed
                    .strip_prefix(marker)
                    .is_some_and(|rest| rest.starts_with([' ', '\t']))
        })
}

fn strip_block_line_prefix(line: &str) -> &str {
    for marker in ["**", "*"] {
        if let Some(rest) = line.strip_prefix(marker)
            && matches!(rest.chars().next(), None | Some(' ' | '\t'))
        {
            return rest.strip_prefix([' ', '\t']).unwrap_or(rest);
        }
    }
    line
}

/// True when nothing but an optional BOM, whitespace, and at most a lone SPDX
/// tag comment precedes the span.
///
/// The SPDX line counts as "still the file header" because "can_merge" refuses
/// to fold it into the block below it (that would push the tag off line 1).
/// Without this, blocking the merge would also demote the header block out of
/// file-header form, so a kernel-style "/* SPDX */" plus copyright banner would
/// lose its opener-alone layout and emit a line past the column limit.
fn is_file_start_prefix(before: &str) -> bool {
    let t = before.trim_start_matches('\u{feff}').trim();
    if t.is_empty() {
        return true;
    }

    // Exactly one block comment, carrying the tag: a second "*/" inside means
    // more than one comment precedes us, so this is no longer the header.
    t.starts_with("/*")
        && t.ends_with("*/")
        && t.contains("SPDX-License-Identifier")
        && !t[2..t.len() - 2].contains("*/")
}

/// True when the span contains a carriage return that is not part of a CRLF.
///
/// No grammar here ends a line comment at a bare "\r", so in a classic-Mac
/// source every visual line after the first is inside the comment node as far
/// as the parser is concerned. Reflowing that packs the following CODE into the
/// comment text ("// a\rint x;\r" came back as "// a int x;"), and converting
/// the run to a block wrapped that code in "/* */". Both destroy code, which no
/// amount of downstream care can undo, so refuse the comment outright.
///
/// A trailing "\r" whose "\n" sits just past "end_byte" is a CRLF and fine:
/// tree-sitter-bash captures that "\r" inside the node while the C grammars
/// leave it out.
fn spans_bare_cr(source: &str, start_byte: usize, end_byte: usize) -> bool {
    let bytes = source.as_bytes();
    bytes[start_byte..end_byte]
        .iter()
        .enumerate()
        .any(|(i, &b)| b == b'\r' && bytes.get(start_byte + i + 1) != Some(&b'\n'))
}

/// True when a C/C++ line comment carries a backslash-newline line splice.
///
/// A "\" at the end of a line splices the next line onto it before the comment
/// is even lexed, so a "//" comment written that way already extends past its
/// own line and the grammar reports one node spanning both. Two things then go
/// wrong, and neither is recoverable downstream:
///
/// - Reflow can leave the "\" as the last thing on the LAST emitted line, and
///   the splice then swallows the CODE line below into the comment. It costs
///   nothing to write ("// a b c \" packs exactly that way at the right width).
/// - Deleting an interior line that is nothing but whitespace, which is
///   correct as comment-interior whitespace, re-points an existing splice from
///   that blank line onto the next code line. The comment text need not be
///   reflowed at all for this to fire, so no width is safe.
///
/// The second pass then reads the swallowed code as comment prose and packs it,
/// so "--check" converges on the damage instead of reporting it. This is the
/// "spans_bare_cr" case exactly: it destroys code, no amount of downstream care
/// can undo it, so refuse the comment outright.
///
/// Only C/C++ line comments. Rust has no line splicing, shell comments end at
/// the newline, and assembly claims only "/* */". A block comment is safe in
/// every language: "*/" terminates it whatever the preceding byte was. A "\"
/// anywhere else in the prose (a path, an escape) is untouched: only one
/// followed by nothing but horizontal whitespace to the line's end splices.
fn spans_line_splice(text: &str, lang: Language) -> bool {
    if !matches!(lang, Language::C | Language::Cpp) || !text.starts_with("//") {
        return false;
    }
    text.lines().any(ends_with_line_splice)
}

/// True when this physical line is continued onto the next one by a trailing
/// "\", the translation-phase-2 splice.
///
/// The one place this rule is written down. It had three encodings before, two
/// of them "trim_end().ends_with('\\')" and this one a byte-class walk, and
/// they disagreed: "trim_end" strips Unicode whitespace, so a "\" followed by a
/// no-break space was a splice to two callers and not to the third. It is not
/// one to any compiler either, which is why the horizontal-whitespace set is
/// the form that survived. The callers ask the same question for different
/// reasons (does this comment sit inside a macro, may transform 5 put a blank
/// line here, is this comment's extent load-bearing), and only one definition
/// of a cpp rule should exist to answer them.
pub(crate) fn ends_with_line_splice(line: &str) -> bool {
    line.trim_end_matches([' ', '\t', '\r']).ends_with('\\')
}

/// Build a "Comment" from a byte span, deriving everything else from the
/// source. Shared by the tree-sitter walk and the assembly scanner so the two
/// producers can't drift on indent capture, trailing detection, or the
/// file-start rule.
fn make_comment(
    source: &str,
    start_byte: usize,
    end_byte: usize,
    lang: Language,
    relocate_before: Option<usize>,
    param_shift: Option<ParamShift>,
) -> Comment {
    let text = source[start_byte..end_byte].to_string();
    let force_passthrough = (lang == Language::Rust && rust_block_has_nested(&text))
        || is_passthrough_directive(&text, lang)
        || spans_bare_cr(source, start_byte, end_byte)
        || spans_line_splice(&text, lang);
    let line_start = line_start_before(source, start_byte);
    let line_prefix = &source[line_start..start_byte];
    let is_trailing = line_prefix.chars().any(|c| !c.is_whitespace());
    let line_indent_bytes = if is_trailing {
        String::new()
    } else {
        line_prefix.to_string()
    };

    // File-start iff only an optional UTF-8 BOM precedes the marker. Computed
    // here (not the post-pass) so a merged group's head carries the right value
    // before merged_block_text reads it; start_byte does not change during
    // merging.
    let before = &source[..start_byte];
    Comment {
        start_byte,
        end_byte,
        start_col_byte: start_byte - line_start,
        text,
        force_passthrough,
        is_trailing,
        line_indent_bytes,

        // Placeholder; resolved in a post-pass once merging settles the final
        // end_byte and the file source is in hand.
        fallback_ending: "\n",
        at_file_start: is_file_start_prefix(before),
        relocate_before,
        param_shift,
    }
}

/// Collect every comment node under "node", in source order.
///
/// The two facts a comment needs about its position, its immediate parent and
/// whether any ancestor is a macro-definition directive, are threaded down the
/// recursion rather than looked up per comment. Tree-sitter nodes carry no
/// parent pointer, so "Node::parent" re-descends from the root and scans the
/// sibling list; asking once per comment made a long run of sibling comments
/// quadratic (20k consecutive "//" lines took ten seconds, nearly all of it in
/// "ts_node_parent"). The walk already knows both facts by construction.
fn walk_collect(
    node: Node,
    parent: Option<Node>,
    in_macro_directive: bool,
    source: &str,
    lang: Language,
    param_shifts: &HashMap<usize, ParamShift>,
    out: &mut Vec<Comment>,
) {
    if is_comment_node(node, lang) {
        if !skips_preproc_directive(node, in_macro_directive, lang, source)
            && !is_shell_shebang(node, source, lang)
        {
            let start_byte = node.start_byte();
            let end_byte = node.end_byte();

            // The after-")" drifted comment satisfies
            // "manpage_relocate_target"'s position test too. A "param_shift"
            // comment is never manpage-hoisted ("plan" handles it first), so
            // drop the redundant "relocate_before" rather than leave the
            // comment dual-tagged. A passthrough directive never carries either
            // target: "detect_param_drift" aborts the whole signature when a
            // directive participates (the drift transform is atomic), and
            // "manpage_relocate_target" can't match a directive.
            let param_shift = param_shifts.get(&start_byte).copied();
            let relocate_before = if param_shift.is_some() {
                None
            } else {
                manpage_relocate_target(node, parent, source, lang)
            };
            out.push(make_comment(
                source,
                start_byte,
                end_byte,
                lang,
                relocate_before,
                param_shift,
            ));
        }
        return;
    }

    // A conditional-compilation block wraps ordinary code, so it is transparent
    // here; only a single-line directive ("#define", "#include") makes its
    // children part of a directive.
    let kind = node.kind();
    let in_macro_directive =
        in_macro_directive || (kind.starts_with("preproc_") && !is_preproc_conditional_block(kind));
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_collect(
            child,
            Some(node),
            in_macro_directive,
            source,
            lang,
            param_shifts,
            out,
        );
    }
}

fn is_comment_node(node: Node, lang: Language) -> bool {
    let kind = node.kind();
    match lang {
        Language::C | Language::Cpp => kind == "comment",
        Language::Rust => kind == "line_comment" || kind == "block_comment",

        // Bash's "shebang" node is deliberately excluded; it is shell syntax
        // (line 1 interpreter directive), not a comment.
        Language::Shell => kind == "comment",
        // Never reached: assembly bypasses the tree walk entirely.
        Language::Asm => false,
    }
}

/// True when the comment lives inside a single-line preprocessor directive
/// (C/C++ only): a "#define" body, "#include", or bare "#"-call, where
/// reflowing could insert a newline that silently terminates the macro,
/// violating the no-non-comment-byte rule.
///
/// "in_macro_directive" is the walk's running answer to "is any ancestor such a
/// directive?". Conditional-compilation blocks ("#if" / "#ifdef" / "#ifndef" /
/// "#else" / "#elif") do not count: tree-sitter nests the entire conditional
/// body (ordinary declarations and comments included) under the "preproc_*"
/// node, so a file wrapped in an include guard would otherwise have every
/// comment skipped.
fn skips_preproc_directive(
    node: Node,
    in_macro_directive: bool,
    lang: Language,
    source: &str,
) -> bool {
    if matches!(lang, Language::Rust | Language::Shell) {
        return false;
    }
    if in_macro_directive {
        return true;
    }

    // No macro-definition ancestor in the AST. Two cases still need skipping: a
    // backslash-continued "#define" whose comment line trips tree-sitter into
    // ERROR recovery (no preproc_ ancestor at all), and a comment on a
    // directive/condition line that the transparent-conditional rule above
    // walked straight past. A cheap syntactic backstop catches both, and it is
    // free for ordinary comments, where it inspects a single line and returns
    // immediately.
    is_inside_preprocessor_directive(node.start_byte(), source)
}

/// True when the comment starting at "start_byte" is physically inside a
/// preprocessor directive: its line, or a backslash-continuation chain reaching
/// back from it, starts with "#". Allocation-free: it walks backward only
/// across genuine "\"-continued lines, so a normal comment costs one line
/// inspection.
///
/// Takes an offset rather than a "Node" because assembly has no tree: a ".S"
/// file is cpp-preprocessed, so it is the language *most* likely to carry
/// "#define FOO \" macros, and its scanner path needs the same guard. Inside a
/// continuation chain a newline is a terminator, not whitespace, so reflowing
/// a comment there (or letting transform 5 prepend a blank line) truncates the
/// macro body and silently changes the generated code.
fn is_inside_preprocessor_directive(start_byte: usize, source: &str) -> bool {
    let mut line_end = start_byte;
    loop {
        let line_start = line_start_before(source, line_end);
        if source[line_start..line_end].trim_start().starts_with('#') {
            return true;
        }
        if line_start == 0 {
            return false;
        }

        // Continue up the chain only if the preceding physical line ends with a
        // "\" line-continuation; otherwise this comment stands on its own.
        let prev_end = line_start - 1;
        let prev_start = line_start_before(source, prev_end);
        if !ends_with_line_splice(&source[prev_start..prev_end]) {
            return false;
        }
        line_end = prev_end;
    }
}

/// Byte offset of the start of the physical line that ends at "end" (the index
/// just past the preceding "\n", or 0 at the start of the source). The single
/// "line start" idiom for the manual-page relocation: it yields both the
/// column-0 insert target (function line start, stored in "relocate_before")
/// and the delete's start (the moved comment's line start).
pub(crate) fn line_start_before(source: &str, end: usize) -> usize {
    source[..end].rfind('\n').map_or(0, |i| i + 1)
}

/// Conditional-compilation block kinds that wrap a body of ordinary code,
/// as opposed to a single-line macro directive. tree-sitter-c spells
/// "#ifdef" and "#ifndef" both as "preproc_ifdef".
fn is_preproc_conditional_block(kind: &str) -> bool {
    matches!(
        kind,
        "preproc_if"
            | "preproc_ifdef"
            | "preproc_else"
            | "preproc_elif"
            | "preproc_elifdef"
            | "preproc_elifndef"
    )
}

/// True when a shell "comment" node is actually the file-leading shebang.
/// Tree-sitter-bash uses the same "comment" node kind for both, so we
/// recognise the shebang by position (starts at byte 0, optionally after a
/// UTF-8 BOM) and prefix ("#!"). A "#!" mid-file is a regular comment per
/// the kernel exec(2) contract and stays in the reflow stream.
fn is_shell_shebang(node: Node, source: &str, lang: Language) -> bool {
    if !matches!(lang, Language::Shell) {
        return false;
    }
    let start = node.start_byte();
    let bom_offset = if source.as_bytes().starts_with(b"\xef\xbb\xbf") {
        3
    } else {
        0
    };
    if start != bom_offset {
        return false;
    }
    source[start..node.end_byte()].starts_with("#!")
}

/// True when a Rust block comment body (between the outer "/*" and "*/")
/// contains a literal "/*" or "*/" byte sequence, i.e. the comment is
/// nested. Force whole-comment pass-through in that case.
fn rust_block_has_nested(text: &str) -> bool {
    if !text.starts_with("/*") || !text.ends_with("*/") || text.len() < 4 {
        return false;
    }
    let interior = &text[2..text.len() - 2];
    interior.contains("/*") || interior.contains("*/")
}

/// Assembly is the one language with no grammar, and it returns an error here
/// rather than panicking. "parser_for" already rejects it a frame up, so this
/// arm is unreachable today; a panic in a tool that rewrites source files in
/// place is a worse way to learn that a future caller found a path around that
/// guard than a message on stderr and a non-zero exit.
fn grammar(lang: Language) -> Result<tree_sitter::Language> {
    Ok(match lang {
        Language::C => tree_sitter_c::LANGUAGE.into(),
        Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::Shell => tree_sitter_bash::LANGUAGE.into(),
        Language::Asm => bail!("assembly has no grammar; scanned by crate::asm"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn clang_format_directive_never_merges() {
        // The trailing "/* clang-format off */" must stay its own comment, not
        // get packed into the doc block above it. Merging would delete the
        // directive and let clang-format reformat the guarded macro.
        let src = "/*\n * Format: _(a, b)\n */\n/* clang-format off */\n#define M(_) _(A, B)\n";
        let cs = extract_comments(src, Language::C).unwrap();
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[1].text, "/* clang-format off */");
        assert!(cs[1].force_passthrough);
    }

    #[test]
    fn formatter_directive_recognizes_both_forms() {
        assert!(is_formatter_directive("/* clang-format off */"));
        assert!(is_formatter_directive("// clang-format on"));
        assert!(is_formatter_directive("// clang-format off: generated"));
        assert!(is_formatter_directive("// clang-format on: resume"));
        assert!(!is_formatter_directive("// clang-format off later"));
        assert!(!is_formatter_directive("/* Format: _(a, b) */"));
    }

    #[test]
    fn acsl_annotations_are_passthrough() {
        let src = "/*@ requires n >= 0;\n  @ ensures \\result >= 0;\n  */\nint f(int n);\n//@ assert x >= 0;\n";
        let cs = extract_comments(src, Language::C).unwrap();
        assert_eq!(cs.len(), 2);
        assert!(cs.iter().all(|c| c.force_passthrough));
        assert!(is_acsl_annotation("/*@ assigns \\nothing; */"));
        assert!(is_acsl_annotation("//@ loop invariant i <= n;"));
        assert!(!is_acsl_annotation("/* @ is just prose here */"));
    }

    #[test]
    fn lint_directives_are_passthrough() {
        // clang-tidy / Coverity / IWYU / SonarQube in C, shellcheck in shell,
        // cbindgen in Rust. Each must survive byte-identical, and each must not
        // fire for the wrong language or for prose that merely names the tool.
        assert_eq!(directive_keyword("// NOLINT(google-explicit)"), "NOLINT");
        assert_eq!(directive_keyword("  * coverity[deref_ptr]"), "coverity");
        assert_eq!(
            directive_keyword("# shellcheck disable=SC2086"),
            "shellcheck"
        );
        assert_eq!(directive_keyword("/// cbindgen:ignore"), "cbindgen");

        assert!(is_lint_directive("// NOLINTNEXTLINE(x)", Language::Cpp));
        assert!(is_lint_directive("// IWYU pragma: keep", Language::C));
        assert!(is_lint_directive("// NOSONAR reason", Language::C));
        assert!(is_lint_directive(
            "# shellcheck shell=bash",
            Language::Shell
        ));
        assert!(is_lint_directive(
            "/// cbindgen:field-names=[x]",
            Language::Rust
        ));
        // Wrong language: shellcheck keyword only counts for shell.
        assert!(!is_lint_directive("# shellcheck disable=SC1", Language::C));
        assert!(!is_lint_directive("// NOLINT here", Language::Shell));
        // Prose that mentions a tool mid-sentence is not a directive.
        assert!(!is_lint_directive(
            "// run coverity before merging",
            Language::C
        ));

        // A multi-line prose block whose *interior* line names a tool still
        // reflows: only the first line is inspected.
        assert!(!is_lint_directive(
            "/* Notes.\n   coverity found nothing here. */",
            Language::C
        ));

        let c = extract_comments("int x; // NOLINT(cert)\n", Language::C).unwrap();
        assert!(c[0].force_passthrough);
        let sh = extract_comments("ls $x # shellcheck disable=SC2086\n", Language::Shell).unwrap();
        assert!(sh[0].force_passthrough);
    }

    #[test]
    fn drift_with_passthrough_directive_aborts_whole_signature() {
        // A directive in the drifted set aborts the transform for the entire
        // signature: no comment shifts. Shifting the siblings while the
        // directive stays put would scramble the parameter/comment pairing, so
        // the drift map must be empty for every comment in the function, not
        // just the directive's own entry.
        let src = "void f(int a, /* NOLINT */ int b, /* d */ int c) /* e */ {}\n";
        let cs = extract_comments(src, Language::C).unwrap();
        assert!(cs.iter().all(|c| c.param_shift.is_none()));
        let directive = cs.iter().find(|c| c.text.contains("NOLINT")).unwrap();
        assert!(directive.force_passthrough);
    }

    #[test]
    fn backslash_continued_line_comment_is_passthrough() {
        // The shape that deleted a line of code: the comment's own continuation
        // is whitespace-only, so removing it (correct, as comment-interior
        // whitespace) re-points the splice onto "int x = 1;" and the next pass
        // reads the declaration as prose. Nothing downstream can undo that, so
        // the comment never enters the pipeline.
        let cs = extract_comments("// aaa bbb \\\n   \nint x = 1;\n", Language::C).unwrap();
        assert!(cs[0].force_passthrough);

        // Also when the continuation carries text: reflow is free to pack the
        // "\" onto the last emitted line, where it swallows the code below.
        let cs = extract_comments("// aaa \\\nbbb ccc\nint x = 1;\n", Language::Cpp).unwrap();
        assert!(cs[0].force_passthrough);

        // A "\" as the file's very last byte is NOT this: the grammar leaves it
        // out of the node ("// aaa bbb " comes back), and there is no line
        // after EOF for a splice to swallow anyway.
        let cs = extract_comments("// aaa bbb \\", Language::C).unwrap();
        assert!(!cs[0].force_passthrough);
    }

    #[test]
    fn backslash_inside_prose_still_reflows() {
        // Only a "\" with nothing but horizontal whitespace after it splices. A
        // path or an escape mid-line is ordinary text, and freezing every
        // comment that mentions one would gut the tool.
        let cs = extract_comments("// see C:\\Users\\me for the log file\n", Language::C).unwrap();
        assert!(!cs[0].force_passthrough);

        // A block comment is safe whatever precedes the newline: "*/" ends it.
        let cs = extract_comments("/* aaa \\\n * bbb */\n", Language::C).unwrap();
        assert!(!cs[0].force_passthrough);

        // Rust has no line splicing, so its line comments are never frozen.
        let cs = extract_comments("// aaa \\\n// bbb\n", Language::Rust).unwrap();
        assert!(!cs[0].force_passthrough);
    }

    #[test]
    fn cppcheck_suppress_block_is_passthrough() {
        // The id sits on the first content line and prose fills the rest.
        // Reflow must not wrap "negativeIndex" together with the prose below,
        // or cppcheck stops matching the suppression against the guarded line.
        let src = "/* cppcheck-suppress negativeIndex\n * RANGE_CHECK guards mac_idx >= 0 before the access.\n */\nx = a[i];\n";
        let cs = extract_comments(src, Language::C).unwrap();
        assert!(cs[0].force_passthrough);
    }

    #[test]
    fn cppcheck_suppress_recognizes_variants() {
        assert!(is_cppcheck_suppress("// cppcheck-suppress nullPointer"));
        assert!(is_cppcheck_suppress(
            "/* cppcheck-suppress negativeIndex */"
        ));
        assert!(is_cppcheck_suppress(
            "// cppcheck-suppress[uninitvar, nullPointer]"
        ));
        assert!(is_cppcheck_suppress("// cppcheck-suppress-begin memleak"));
        assert!(is_cppcheck_suppress(
            "/*\n * cppcheck-suppress-file allocaCalled\n */"
        ));
        // Directive below prose in a block still counts: scan all lines.
        assert!(is_cppcheck_suppress(
            "/*\n * why this is safe\n * cppcheck-suppress negativeIndex\n */"
        ));
        assert!(!is_cppcheck_suppress(
            "// cppcheck-suppresses this behavior"
        ));
        assert!(!is_cppcheck_suppress("// see cppcheck-suppress docs"));
    }

    #[test]
    fn detect_c_from_extension() {
        assert_eq!(
            detect_language(&PathBuf::from("foo.c")).unwrap(),
            Language::C
        );
        assert_eq!(
            detect_language(&PathBuf::from("foo.h")).unwrap(),
            Language::C
        );
        assert_eq!(
            detect_language(&PathBuf::from("foo.H")).unwrap(),
            Language::C
        );
    }

    #[test]
    fn detect_cpp_from_extension() {
        for ext in &["cc", "cpp", "cxx", "c++", "hh", "hpp", "hxx", "h++"] {
            let p = PathBuf::from(format!("foo.{ext}"));
            assert_eq!(detect_language(&p).unwrap(), Language::Cpp, "ext={ext}");
        }
    }

    #[test]
    fn detect_objc_from_extension() {
        // Objective-C rides the C grammar, Objective-C++ the C++ grammar.
        // comments are lexically identical and extract cleanly amid ObjC
        // syntax.
        assert_eq!(
            detect_language(&PathBuf::from("foo.m")).unwrap(),
            Language::C
        );
        assert_eq!(
            detect_language(&PathBuf::from("foo.mm")).unwrap(),
            Language::Cpp
        );
        assert_eq!(
            detect_language(&PathBuf::from("foo.MM")).unwrap(),
            Language::Cpp
        );
    }

    #[test]
    fn detect_rust_from_extension() {
        assert_eq!(
            detect_language(&PathBuf::from("foo.rs")).unwrap(),
            Language::Rust
        );
    }

    #[test]
    fn detect_shell_from_extension() {
        for ext in &["sh", "bash", "SH", "BASH"] {
            let p = PathBuf::from(format!("foo.{ext}"));
            assert_eq!(detect_language(&p).unwrap(), Language::Shell, "ext={ext}");
        }
    }

    #[test]
    fn rejects_other_shell_flavors() {
        for ext in &["zsh", "fish", "ksh", "dash"] {
            let p = PathBuf::from(format!("foo.{ext}"));
            assert!(detect_language(&p).is_err(), "ext={ext} must fail");
        }
    }

    #[test]
    fn unknown_extension_errors() {
        assert!(detect_language(&PathBuf::from("foo.txt")).is_err());
        assert!(detect_language(&PathBuf::from("foo")).is_err());
    }

    #[test]
    fn posix_shell_shebang_recognized() {
        for line in &[
            "#!/bin/sh",
            "#!/bin/bash",
            "#!/usr/bin/env bash",
            "#!/usr/bin/env sh",
            "#!/bin/sh -e",
            "#!/usr/local/bin/bash",
            "#!/usr/bin/env -S bash",
        ] {
            assert!(is_posix_shell_shebang_line(line), "must accept: {line}");
        }
    }

    #[test]
    fn non_shell_shebang_rejected() {
        for line in &[
            "#!/bin/zsh",
            "#!/usr/bin/env python3",
            "#!/usr/bin/perl",
            "#!/bin/dash",
            "#!/usr/bin/env",
            "#! ",
            "// not a shebang",
            "# regular comment",
            "",
        ] {
            assert!(!is_posix_shell_shebang_line(line), "must reject: {line:?}");
        }
    }

    #[test]
    fn nested_block_detection() {
        assert!(rust_block_has_nested("/* outer /* inner */ outer */"));
        assert!(rust_block_has_nested("/* /* */ */"));
        assert!(!rust_block_has_nested("/* outer */"));
        assert!(!rust_block_has_nested("/* */"));
    }
}
