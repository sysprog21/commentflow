//! Per-line classification: deciding whether a comment line is prose to be
//! reflowed or layout to be preserved. Everything downstream (paragraph
//! grouping, reflow, emission) is driven by the "LineKind" this module
//! assigns, so this is the tool's central judgment call.
//!
//! Split out of "normalize" because the order of the checks in
//! "classify_lines" is load-bearing correctness that deserves to be read in
//! one sitting, not found by scrolling past prefix-stripping and two content
//! transforms.

use crate::classify::DocFlavor;
use crate::textline::{
    FAST_PATH_TAB_WIDTH, advance_col, bookend_match, fence_marker_run, is_art, is_assignment_line,
    is_indented_code, is_kernel_doc_tag, is_table_row, keep_complete_row_runs, line_is_art_only,
    one_sided_banner,
};

/// One comment source line with its marker prefix split off: "prefix" is what
/// the emitter puts back ("/// ", " * "), "body" is the content this module
/// classifies. Built by "normalize::strip_prefixes", consumed here.
#[derive(Debug, Clone)]
pub(crate) struct StrippedLine {
    pub(crate) prefix: String,
    pub(crate) body: String,
    pub(crate) had_crlf: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Blank,
    Prose,
    FenceOpen,
    FenceContent,
    FenceClose,
    DoxyVerbatimOpen,
    DoxyVerbatimContent,
    DoxyVerbatimClose,
    IndentedCode,
    TableRow,
    /// A compact assignment or formula row (for example, "ir->imm = value").
    /// These rows document a mapping, not a sentence, so preserve their
    /// one-row layout instead of merging them into adjacent prose. Only a run
    /// of two or more adjacent rows qualifies, and the run freezes whole; see
    /// "textline::keep_complete_row_runs".
    AssignmentForm,
    Blockquote,
    ReferenceLink,
    SetextUnderline,
    AtxHeader,
    Art,
    ListItem,
    DoxygenTag,
    /// License/copyright/SPDX-style metadata. Each detected line is its own
    /// paragraph and emits verbatim: wrapping these would corrupt the
    /// identifier expected by license-detection tooling.
    Metadata,
    /// A row of a "Key: value" banner ("File:" / "Task:"). Pinned to its own
    /// line like "Metadata", but its bytes are ordinary text rather than a
    /// license identifier, so it re-emits behind the canonical prefix instead
    /// of replaying raw source. Keeping the two kinds apart leaves license
    /// blocks byte-identical.
    LabelRow,
    /// A banner with its rule run on one side only ("label -------"). Emitted
    /// exactly like "LabelRow"; a separate kind because the reason differs, and
    /// so a reader chasing "Key: value" behavior is not sent to a dash rule.
    /// See "textline::one_sided_banner".
    Banner,
}

impl LineKind {
    /// Does this kind go out verbatim instead of being packed into a prose
    /// paragraph? "normalize::group_paragraphs" keys paragraph boundaries on
    /// this, so a kind that answers wrong is reflowed as prose.
    ///
    /// Exhaustive on purpose: a new variant must answer this question before
    /// it compiles, rather than defaulting to "reflow it" and being found by
    /// a corrupted diagram later.
    pub(crate) fn is_preformatted(self) -> bool {
        match self {
            LineKind::FenceOpen
            | LineKind::FenceContent
            | LineKind::FenceClose
            | LineKind::DoxyVerbatimOpen
            | LineKind::DoxyVerbatimContent
            | LineKind::DoxyVerbatimClose
            | LineKind::IndentedCode
            | LineKind::TableRow
            | LineKind::AssignmentForm
            | LineKind::Blockquote
            | LineKind::ReferenceLink
            | LineKind::Metadata
            | LineKind::LabelRow
            | LineKind::Banner
            | LineKind::Art => true,
            LineKind::Blank
            | LineKind::Prose
            | LineKind::SetextUnderline
            | LineKind::AtxHeader
            | LineKind::ListItem
            | LineKind::DoxygenTag => false,
        }
    }

    /// The kinds that re-emit their body behind the canonical prefix instead
    /// of replaying their raw source line: rows whose bytes are ordinary text,
    /// not layout, so a drifted "**" marker and a stripped decorative bookend
    /// land like every reflowed sibling. Raw replay is for the kinds whose
    /// exact source bytes are the content (art, code, tables) and for
    /// metadata, which is not ours to retouch.
    ///
    /// Named once so "emits_canonically" and the invariant test that it
    /// implies "is_preformatted" cannot drift apart.
    #[cfg(test)]
    pub(crate) const CANONICAL: [LineKind; 3] = [
        LineKind::LabelRow,
        LineKind::Banner,
        LineKind::AssignmentForm,
    ];

    /// See "CANONICAL". Unlike "is_preformatted" this is not exhaustive: the
    /// safe default is raw replay, and a new variant is already forced into
    /// this impl block by the exhaustive match above.
    pub(crate) fn emits_canonically(self) -> bool {
        matches!(
            self,
            LineKind::LabelRow | LineKind::Banner | LineKind::AssignmentForm
        )
    }
}

/// What a Doxygen tag does to the line that carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TagShape {
    /// Takes a name before the description: "@param buf the buffer".
    Named,
    /// Description only: "@brief does the thing".
    Plain,
    /// Opens a verbatim region that runs until its closer.
    VerbatimOpen,
    /// Closes one.
    VerbatimClose,
}

/// Every Doxygen tag the tool recognizes, spelled without the leading marker
/// ("@tag" and "\tag" are interchangeable). One table, because two stages read
/// it for different reasons and must not drift: "classify_lines" below decides
/// whether a line is a tag paragraph, and reflow decides where the
/// continuation lines of a wrapped tag line align.
///
/// This is the vocabulary the tool *recognizes*. It is deliberately not the
/// narrower vocabulary the tool *converts*, which lives with the conversion in
/// "normalize::kdoc_tag_of_keyword" and answers a different question.
const DOXY_TAGS: &[(&str, TagShape)] = &[
    ("param", TagShape::Named),
    ("tparam", TagShape::Named),
    ("retval", TagShape::Named),
    ("throws", TagShape::Named),
    ("throw", TagShape::Named),
    ("exception", TagShape::Named),
    ("brief", TagShape::Plain),
    ("return", TagShape::Plain),
    ("returns", TagShape::Plain),
    ("note", TagShape::Plain),
    ("warning", TagShape::Plain),
    ("see", TagShape::Plain),
    ("sa", TagShape::Plain),
    ("pre", TagShape::Plain),
    ("post", TagShape::Plain),
    ("since", TagShape::Plain),
    ("deprecated", TagShape::Plain),
    ("details", TagShape::Plain),
    ("short", TagShape::Plain),
    ("author", TagShape::Plain),
    ("date", TagShape::Plain),
    ("version", TagShape::Plain),
    ("copyright", TagShape::Plain),
    ("file", TagShape::Plain),
    ("ingroup", TagShape::Plain),
    ("defgroup", TagShape::Plain),
    ("addtogroup", TagShape::Plain),
    ("code", TagShape::VerbatimOpen),
    ("verbatim", TagShape::VerbatimOpen),
    ("endcode", TagShape::VerbatimClose),
    ("endverbatim", TagShape::VerbatimClose),
];

pub(crate) fn doxy_tag(keyword: &str) -> Option<TagShape> {
    DOXY_TAGS
        .iter()
        .find(|(name, _)| *name == keyword)
        .map(|&(_, shape)| shape)
}

/// The tag keyword sitting after a "@" or "\" marker: the leading run of ASCII
/// letters. "param[in] buf" reads as "param", so a Doxygen direction
/// annotation neither hides the tag nor becomes part of it.
pub(crate) fn doxy_keyword(after_marker: &str) -> &str {
    let end = after_marker
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(after_marker.len());
    &after_marker[..end]
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
enum FenceState {
    #[default]
    Closed,
    Open {
        marker: char,
        run: usize,
    },
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
enum DoxyState {
    #[default]
    Closed,
    Open,
}

/// Assign a "LineKind" to every line of one comment. Everything downstream
/// (paragraph grouping, reflow, emission) is driven by the result, so this is
/// where "is this text or is this layout?" gets decided.
///
/// THE ORDER OF THE CHECKS BELOW IS LOAD-BEARING. Each one runs only on lines
/// the earlier ones rejected, so moving a check changes what it sees. What each
/// position is buying, in order:
///
///   1. Fence and Doxygen-verbatim state. These are RANGES, not lines: once
///      open, every line is content until the closer, whatever it looks like.
///      They must come first or a "#" inside a code fence reads as a header.
///   2. Blank.
///   3. "is_art_adjacent_bookend", before the bookend-ish checks that would
///      otherwise claim a row of an ASCII drawing.
///   4. Fence open.
///   5. Doxygen tags, then the kernel-doc "@name:" form. Before ATX, because a
///      tag line can carry punctuation that later checks would grab.
///   6. Setext underline, which needs the PREVIOUS line's verdict, so it has to
///      run after that line was classified (the loop is in source order).
///   7. ATX header, then metadata (license/SPDX).
///   8. Blockquote, reference link, table row, indented code. All preformatted.
///   9. Label runs. Deliberately AFTER indented code: a "Key: value" line with a
///      code sample's indentation belongs to the sample. See "label_runs".
///  10. One-sided banners ("label -------"), which freeze only when they stand
///      alone in a paragraph. See "textline::one_sided_banner".
///  11. Art, list item, and finally prose as the fallback.
///
/// Mapping runs are not in that order at all: they are decided after the loop,
/// over the lines nothing else claimed, so a run cannot reach through a code
/// sample or a table. See "mark_assignment_runs".
///
/// A check that yields a preformatted kind can be reordered against other
/// preformatted checks that share its emit path without changing output; the
/// two paths are "LineKind::emits_canonically". Reordering across that
/// boundary, or moving any non-preformatted check, needs the reasoning above
/// rechecked.
pub(crate) fn classify_lines(
    lines: &[StrippedLine],
    flavor: DocFlavor,
    label_budget: usize,
) -> Vec<LineKind> {
    let mut out = vec![LineKind::Prose; lines.len()];
    let in_label_run = label_runs(lines, label_budget);
    let mut fence = FenceState::Closed;
    let mut doxy = DoxyState::Closed;

    for (i, l) in lines.iter().enumerate() {
        let body = l.body.as_str();

        match doxy {
            DoxyState::Open => {
                if let Some(stripped) = body.trim_start().strip_prefix(['@', '\\'])
                    && doxy_tag(doxy_keyword(stripped)) == Some(TagShape::VerbatimClose)
                {
                    out[i] = LineKind::DoxyVerbatimClose;
                    doxy = DoxyState::Closed;
                    continue;
                }
                out[i] = LineKind::DoxyVerbatimContent;
                continue;
            }
            DoxyState::Closed => {}
        }

        match fence {
            FenceState::Open { marker, run } => {
                if let Some((c, len)) = fence_marker_run(body)
                    && c == marker
                    && len >= run
                    && body.trim().chars().all(|ch| ch == marker)
                {
                    out[i] = LineKind::FenceClose;
                    fence = FenceState::Closed;
                    continue;
                }
                out[i] = LineKind::FenceContent;
                continue;
            }
            FenceState::Closed => {}
        }

        if body.trim().is_empty() {
            out[i] = LineKind::Blank;
            continue;
        }

        if is_art_adjacent_bookend(lines, i) {
            out[i] = LineKind::Art;
            continue;
        }

        if let Some((c, run)) = fence_marker_run(body)
            && run >= 3
        {
            out[i] = LineKind::FenceOpen;
            fence = FenceState::Open { marker: c, run };
            continue;
        }

        let trimmed = body.trim_start();

        if let Some(stripped) = trimmed.strip_prefix(['@', '\\']) {
            let shape = doxy_tag(doxy_keyword(stripped));
            if shape == Some(TagShape::VerbatimOpen) {
                out[i] = LineKind::DoxyVerbatimOpen;
                doxy = DoxyState::Open;
                continue;
            }
            if matches!(flavor, DocFlavor::Doxygen)
                && shape.is_some()
                && !looks_like_email_or_path(stripped)
            {
                out[i] = LineKind::DoxygenTag;
                continue;
            }
        }

        // Kernel-doc parameter form "@name:" / "@name :" gets its own tag
        // paragraph in any flavor. convert_kernel_doc emits this shape, so a
        // second pass over already-converted output must keep each param
        // standalone (identical-bytes invariant) instead of merging them.
        if is_kernel_doc_tag(trimmed) {
            out[i] = LineKind::DoxygenTag;
            continue;
        }

        if let Some(kind) = setext_underline_kind(body)
            && i > 0
            && matches!(out[i - 1], LineKind::Prose)
        {
            out[i] = kind;
            continue;
        }

        if is_atx_header(body) {
            out[i] = LineKind::AtxHeader;
            continue;
        }

        if is_metadata_line(body) {
            out[i] = LineKind::Metadata;
            continue;
        }

        if is_blockquote(body) {
            out[i] = LineKind::Blockquote;
            continue;
        }

        if is_reference_link(body) {
            out[i] = LineKind::ReferenceLink;
            continue;
        }

        if is_table_row(body) {
            out[i] = LineKind::TableRow;
            continue;
        }

        if is_indented_code(body) {
            out[i] = LineKind::IndentedCode;
            continue;
        }

        // After IndentedCode: a banner may pad one space past the marker for
        // alignment, which is below the indented-code threshold. A deeper
        // indent belongs to a code sample and is already claimed above.
        if in_label_run[i] {
            out[i] = LineKind::LabelRow;
            continue;
        }

        // A one-sided banner freezes only when it stands alone in its
        // paragraph. Inside a paragraph, a line ending in "---" is far more
        // often a sentence wrapped right after an em-dash written as three
        // hyphens ("a fundamentally unsound strategy ---" in apr_pools.h), and
        // freezing that would strand the rest of the sentence on its own.
        let alone_above = i == 0 || lines[i - 1].body.trim().is_empty();
        let alone_below = lines.get(i + 1).is_none_or(|l| l.body.trim().is_empty());
        if alone_above && alone_below && one_sided_banner(body) {
            out[i] = LineKind::Banner;
            continue;
        }

        if is_art(body) {
            out[i] = LineKind::Art;
            continue;
        }

        if is_list_item(body) {
            out[i] = LineKind::ListItem;
            continue;
        }

        out[i] = LineKind::Prose;
    }
    mark_assignment_runs(lines, &mut out);

    out
}

/// Mapping runs, decided after every other check rather than inside the loop.
///
/// A row's neighbour has to be a row in the OUTPUT, not merely row-shaped in
/// the source. An indented code sample reads as a row once trimmed, so judging
/// on shape alone let "  ret = foo(a, b);" pair with the sentence under it and
/// freeze that sentence on its own -- the lone-row case the run rule exists to
/// reflow. Only lines nothing else claimed are eligible, so the run cannot
/// reach through a code sample, a table row, a blockquote, or a fence.
///
/// Running last costs nothing against the kinds that would have come after it.
/// A label row and a mapping row are emitted the same way, so which one claims
/// a line that could be either does not change the output; art already declines
/// a mapping row; and a list item or a lone banner is never row-shaped.
///
/// No width budget here, unlike a label run: a mapping row cannot be wrapped
/// without destroying the mapping, so a frozen run may overflow the column
/// limit exactly as a table row or an indented code sample may. See
/// "preformatted_assignment_run_may_overflow_by_design".
fn mark_assignment_runs(lines: &[StrippedLine], out: &mut [LineKind]) {
    // Most comments carry no "=" at all, and skipping them keeps the common
    // case free of the vector below.
    if !lines.iter().any(|l| l.body.as_bytes().contains(&b'=')) {
        return;
    }
    let mut rows: Vec<bool> = out
        .iter()
        .zip(lines)
        .map(|(kind, l)| *kind == LineKind::Prose && is_assignment_line(&l.body))
        .collect();
    keep_complete_row_runs(&mut rows);
    for (i, in_run) in rows.into_iter().enumerate() {
        if in_run {
            out[i] = LineKind::AssignmentForm;
        }
    }
}

// Classify-pass twin of the strip pass's "is_protective" adjacency check (see
// "strip_decorative_bookends"): a bookend that survived stripping because a
// neighbor is art is itself emitted as Art (verbatim), not reflowed as prose.
// Same "neighbor is protective" notion: "line_is_art_only" here equals "ArtOnly
// | BookendBare" there. Keep both in step.
fn is_art_adjacent_bookend(lines: &[StrippedLine], i: usize) -> bool {
    if bookend_match(&lines[i].body).is_none() {
        return false;
    }
    let above_art = i > 0 && line_is_art_only(&lines[i - 1].body);
    let below_art = i + 1 < lines.len() && line_is_art_only(&lines[i + 1].body);
    above_art || below_art
}

pub(crate) fn looks_like_email_or_path(stripped: &str) -> bool {
    let token: String = stripped
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    token.contains('.') || token.contains('/')
}

fn setext_underline_kind(body: &str) -> Option<LineKind> {
    let t = body.trim();
    if t.len() < 2 {
        return None;
    }
    let c = t.chars().next()?;
    if (c == '=' || c == '-') && t.chars().all(|x| x == c) {
        Some(LineKind::SetextUnderline)
    } else {
        None
    }
}

fn is_atx_header(body: &str) -> bool {
    let t = body.trim_start();
    let hashes: String = t.chars().take_while(|c| *c == '#').collect();
    if hashes.is_empty() || hashes.len() > 6 {
        return false;
    }
    let rest = &t[hashes.len()..];
    let after_space = rest.chars().next();
    matches!(after_space, Some(' ' | '\t')) && rest.trim().chars().any(|c| !c.is_whitespace())
}

fn is_blockquote(body: &str) -> bool {
    // Anchored at column 0 of the post-prefix body. A leading-space-then-">" is
    // IndentedCode (or just prose), not a blockquote.
    body.starts_with('>')
}

fn is_reference_link(body: &str) -> bool {
    let t = body.trim_start();
    if !t.starts_with('[') {
        return false;
    }
    let end = t.find(']');
    let Some(end) = end else { return false };
    if end == 1 {
        return false;
    }
    let after = &t[end + 1..];
    after.starts_with(':') && after[1..].trim_start().chars().any(|c| !c.is_whitespace())
}

/// License/copyright/SPDX-style metadata lines that must not be merged into
/// surrounding prose paragraphs. Each detected line is its own paragraph and
/// is emitted verbatim: wrapping it across lines would corrupt the
/// identifier expected by license-detection tooling.
fn is_metadata_line(body: &str) -> bool {
    let t = body.trim_start();

    // Copyright lines require a stronger signal than just the word "Copyright"
    // followed by a space, which would false-positive on prose like "Copyright
    // law forbids this." Require one of:
    //   - "Copyright (c)" or "Copyright (C)" or "Copyright ©"
    //   - "Copyright <year>" where <year> is a 4-digit number
    if let Some(rest) = t.strip_prefix("Copyright ") {
        let r = rest.trim_start();
        if r.starts_with("(c)") || r.starts_with("(C)") || r.starts_with('©') {
            return true;
        }
        let first_token: String = r.chars().take_while(|c| !c.is_whitespace()).collect();
        // "2026", "2026-2027", "2026, 2027" all start with 4+ digits.
        let leading_digits = first_token.chars().take_while(char::is_ascii_digit).count();
        if leading_digits >= 4 {
            return true;
        }
    }
    // SPDX-License-Identifier:, SPDX-FileCopyrightText:, etc.
    if t.starts_with("SPDX-") && t.contains(':') {
        return true;
    }
    // Common license-header signals.
    if t.starts_with("All rights reserved") {
        return true;
    }
    // Author:, License:, Version: at line start with a non-empty value.
    for key in &["Author:", "License:", "Version:", "Permission:"] {
        if let Some(rest) = t.strip_prefix(key)
            && rest
                .trim_start()
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace())
        {
            return true;
        }
    }
    false
}

/// "Key: value" banner lines, the "File:" / "Task:" shape of a file header,
/// keep their own line instead of packing into one paragraph, on the shared
/// row-run rule. A label that does not already fit the budget is not a row:
/// unlike a mapping row it is a wrappable sentence, so freezing it would park
/// it over the column limit forever.
fn label_runs(lines: &[StrippedLine], budget: usize) -> Vec<bool> {
    let label: Vec<bool> = lines
        .iter()
        .map(|l| is_label_line(&l.body, budget))
        .collect();

    // A label joins a run only when the line below ends the run cleanly: blank,
    // gone, or another label. A label followed by ordinary prose is the head of
    // a badly wrapped paragraph, and freezing it strands the tail. A mapping
    // row deliberately forgoes this: its value is code, not a clause, so it
    // does not wrap into the line below. That conjunct is the whole difference
    // between the two rules; the run arithmetic is shared.
    let mut eligible: Vec<bool> = (0..lines.len())
        .map(|j| {
            label[j]
                && lines
                    .get(j + 1)
                    .is_none_or(|next| next.body.trim().is_empty() || label[j + 1])
        })
        .collect();
    keep_complete_row_runs(&mut eligible);
    eligible
}

fn is_label_line(body: &str, budget: usize) -> bool {
    let t = body.trim_start();
    let Some(colon) = t.find(':') else {
        return false;
    };

    // Padding before the colon is alignment, not prose: a banner routinely
    // columns its separators ("File : x" / "Task : y"). Only spaces may sit
    // there, so "Note that we do this : ..." is still prose.
    let key = t[..colon].trim_end_matches([' ', '\t']);

    // The single-word test below is what separates a key from prose; the length
    // cap is only a sanity bound, so it has to clear real keys like
    // "APR_LDAP_STARTTLS".
    if key.is_empty() || key.chars().count() > 32 || !key.starts_with(char::is_uppercase) {
        return false;
    }
    if !key
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return false;
    }
    if t[colon + 1..].trim().is_empty() {
        return false;
    }

    // A frozen line is emitted as-is, so freezing an over-long one would park
    // it over the column limit forever. Unlike an SPDX tag, a "Warning: <long
    // sentence>" has no parser depending on it staying one line: wrap it.
    body.chars()
        .fold(0usize, |col, c| advance_col(col, c, FAST_PATH_TAB_WIDTH))
        <= budget
}

fn is_list_item(body: &str) -> bool {
    let t = body.trim_start();
    if let Some(rest) = t.strip_prefix(['-', '*', '+']) {
        return rest.starts_with([' ', '\t']);
    }
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return false;
    }
    let rest = &t[digits.len()..];
    if let Some(after) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
        return after.starts_with([' ', '\t']);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stripped(body: &str) -> StrippedLine {
        StrippedLine {
            prefix: " * ".to_string(),
            body: body.to_string(),
            had_crlf: false,
        }
    }

    /// Classify a whole comment, one string per body line.
    fn kinds(bodies: &[&str], flavor: DocFlavor) -> Vec<LineKind> {
        let lines: Vec<StrippedLine> = bodies.iter().map(|b| stripped(b)).collect();
        classify_lines(&lines, flavor, 80)
    }

    fn classify_one(body: &str, flavor: DocFlavor) -> LineKind {
        let lines = vec![StrippedLine {
            prefix: " * ".to_string(),
            body: body.to_string(),
            had_crlf: false,
        }];
        classify_lines(&lines, flavor, 80)[0]
    }

    #[test]
    fn atx_header_detection() {
        assert_eq!(
            classify_one("# Returns", DocFlavor::Rustdoc),
            LineKind::AtxHeader
        );
        assert_eq!(
            classify_one("## Errors", DocFlavor::Rustdoc),
            LineKind::AtxHeader
        );
        assert_eq!(
            classify_one("###### foo", DocFlavor::None),
            LineKind::AtxHeader
        );
        assert_eq!(
            classify_one("####### too many", DocFlavor::None),
            LineKind::Prose
        );
        assert_eq!(classify_one("#Returns", DocFlavor::None), LineKind::Prose);
    }

    #[test]
    fn blockquote_detection() {
        assert_eq!(
            classify_one("> quoted", DocFlavor::None),
            LineKind::Blockquote
        );
        assert_eq!(
            classify_one(">no space", DocFlavor::None),
            LineKind::Blockquote
        );
    }

    #[test]
    fn reference_link_detection() {
        assert_eq!(
            classify_one("[foo]: https://example.com", DocFlavor::None),
            LineKind::ReferenceLink
        );
    }

    #[test]
    fn fence_open_detection() {
        let lines = vec![
            StrippedLine {
                prefix: " * ".into(),
                body: "```rust,no_run".into(),
                had_crlf: false,
            },
            StrippedLine {
                prefix: " * ".into(),
                body: "let x = 1;".into(),
                had_crlf: false,
            },
            StrippedLine {
                prefix: " * ".into(),
                body: "```".into(),
                had_crlf: false,
            },
        ];
        let k = classify_lines(&lines, DocFlavor::Rustdoc, 80);
        assert_eq!(k[0], LineKind::FenceOpen);
        assert_eq!(k[1], LineKind::FenceContent);
        assert_eq!(k[2], LineKind::FenceClose);
    }

    #[test]
    fn tilde_fence_detection() {
        let lines = vec![
            StrippedLine {
                prefix: " * ".into(),
                body: "~~~".into(),
                had_crlf: false,
            },
            StrippedLine {
                prefix: " * ".into(),
                body: "code".into(),
                had_crlf: false,
            },
            StrippedLine {
                prefix: " * ".into(),
                body: "~~~".into(),
                had_crlf: false,
            },
        ];
        let k = classify_lines(&lines, DocFlavor::Rustdoc, 80);
        assert_eq!(k[0], LineKind::FenceOpen);
        assert_eq!(k[1], LineKind::FenceContent);
        assert_eq!(k[2], LineKind::FenceClose);
    }

    #[test]
    fn fence_inside_hash_not_header() {
        let lines = vec![
            StrippedLine {
                prefix: "/// ".into(),
                body: "```".into(),
                had_crlf: false,
            },
            StrippedLine {
                prefix: "/// ".into(),
                body: "# use foo;".into(),
                had_crlf: false,
            },
            StrippedLine {
                prefix: "/// ".into(),
                body: "```".into(),
                had_crlf: false,
            },
        ];
        let k = classify_lines(&lines, DocFlavor::Rustdoc, 80);
        assert_eq!(k[1], LineKind::FenceContent);
    }

    #[test]
    fn art_detection_with_word_guard() {
        assert_eq!(
            classify_one("when a > b && c < d, return early", DocFlavor::None),
            LineKind::Prose
        );
        assert_eq!(
            classify_one("+----+----+", DocFlavor::None),
            LineKind::TableRow
        );
    }

    #[test]
    fn unicode_art_passes_box_drawing() {
        assert_eq!(classify_one("┌─┐", DocFlavor::None), LineKind::Art);
        assert_eq!(classify_one("┌─┐ │ │ └─┘", DocFlavor::None), LineKind::Art);
        assert_eq!(classify_one("█ ▓ ▒ ░", DocFlavor::None), LineKind::Art);
    }

    #[test]
    fn unicode_arrow_single_is_prose() {
        assert_eq!(
            classify_one("the symbol arrow means yields", DocFlavor::None),
            LineKind::Prose
        );
    }

    #[test]
    fn list_item_detection() {
        assert_eq!(classify_one("- one", DocFlavor::None), LineKind::ListItem);
        assert_eq!(classify_one("1. one", DocFlavor::None), LineKind::ListItem);
    }

    #[test]
    fn canonical_emit_implies_preformatted() {
        // Raw replay is the fallback in "reflow::emit_paragraphs", reached only
        // inside a preformatted paragraph. A kind that emits canonically but is
        // not preformatted would never reach either branch.
        for k in LineKind::CANONICAL {
            assert!(k.emits_canonically(), "{k:?} is missing from the matches!");
            assert!(
                k.is_preformatted(),
                "{k:?} emits canonically but is not preformatted"
            );
        }
    }

    #[test]
    fn assignment_runs_are_preformatted() {
        use LineKind::AssignmentForm as A;

        // A run of two or more rows freezes; every spelling of the operator and
        // any alignment padding counts as the same row shape.
        assert_eq!(kinds(&["X = Y", "Z = W"], DocFlavor::None), vec![A, A]);
        assert_eq!(
            kinds(
                &[
                    "ir->imm   = immediate",
                    "ir->imm2  = offset",
                    "ir->rd = dest"
                ],
                DocFlavor::None
            ),
            vec![A, A, A]
        );
        assert_eq!(
            kinds(&["a=b", "c += 1", "flags |= MASK"], DocFlavor::None),
            vec![A, A, A]
        );
        assert_eq!(
            kinds(&["Foo::bar = x", "f(x) = y", "%eax = 0"], DocFlavor::None),
            vec![A, A, A]
        );
    }

    #[test]
    fn a_lone_assignment_row_is_prose() {
        use LineKind::Prose as P;

        // One row is a sentence: "count = zero ..." must reflow with the
        // paragraph it belongs to, not freeze and strand the tail.
        assert_eq!(classify_one("X = Y", DocFlavor::None), LineKind::Prose);
        assert_eq!(
            kinds(
                &[
                    "The mapping is fixed by the header:",
                    "offset = base plus the length of the header, rounded up to",
                    "the next multiple of the alignment the caller requested.",
                ],
                DocFlavor::None
            ),
            vec![P, P, P]
        );
        // A row whose successor is prose heads a badly wrapped paragraph.
        assert_eq!(
            kinds(
                &["a = 1", "and then we continue the sentence"],
                DocFlavor::None
            ),
            vec![P, P]
        );
    }

    #[test]
    fn arrow_spellings_are_not_assignments() {
        // "=>" is an arrow and "=<" is nothing at all. Neither opens a mapping
        // row, however many of them sit together.
        for op in ["=>", "=<"] {
            let k = kinds(
                &[&format!("a {op} 1"), &format!("b {op} 2")],
                DocFlavor::None,
            );
            assert!(
                !k.contains(&LineKind::AssignmentForm),
                "{op} must not classify as a mapping row, got {k:?}"
            );
        }
        // Tested unspaced, so a real value opening with "<" still qualifies.
        assert_eq!(
            kinds(&["first = <unknown>", "second = <unset>"], DocFlavor::None),
            vec![LineKind::AssignmentForm; 2]
        );
    }

    #[test]
    fn comparisons_are_not_assignments() {
        assert_eq!(
            kinds(
                &["when a == b, continue", "and c === d too"],
                DocFlavor::None
            ),
            vec![LineKind::Prose, LineKind::Prose]
        );

        // "!=", "<=", ">=" never assign. (A bare "c <= d" is short and dense
        // enough that the art rule claims it first; what matters here is only
        // that the assignment rule does not.)
        assert!(
            !kinds(&["a != b", "c <= d", "e >= f"], DocFlavor::None)
                .contains(&LineKind::AssignmentForm)
        );
        // Multi-token left-hand sides are prose, however they are spelled.
        assert_eq!(
            kinds(
                &["the value = means enabled", "the other = means disabled"],
                DocFlavor::None
            ),
            vec![LineKind::Prose, LineKind::Prose]
        );
        // "<<=" and ">>=" do assign, unlike "<=" and ">=".
        assert_eq!(
            kinds(&["mask <<= 1", "other >>= 2"], DocFlavor::None),
            vec![LineKind::AssignmentForm, LineKind::AssignmentForm]
        );
    }

    #[test]
    fn doxygen_tag_recognized() {
        assert_eq!(
            classify_one("@param x foo", DocFlavor::Doxygen),
            LineKind::DoxygenTag
        );
        assert_eq!(
            classify_one("@return value", DocFlavor::Doxygen),
            LineKind::DoxygenTag
        );
        assert_eq!(
            classify_one("@param x foo", DocFlavor::Rustdoc),
            LineKind::Prose
        );
    }

    #[test]
    fn email_at_tag_not_recognized() {
        assert_eq!(
            classify_one("@example.com", DocFlavor::Doxygen),
            LineKind::Prose
        );
        assert_eq!(
            classify_one("@user/repo", DocFlavor::Doxygen),
            LineKind::Prose
        );
    }

    #[test]
    fn art_adjacent_labeled_bookend_classifies_as_art() {
        let lines = vec![
            StrippedLine {
                prefix: " * ".into(),
                body: "----------   diagram   ----------".into(),
                had_crlf: false,
            },
            StrippedLine {
                prefix: " * ".into(),
                body: "|      |".into(),
                had_crlf: false,
            },
        ];
        let k = classify_lines(&lines, DocFlavor::None, 80);
        assert_eq!(k[0], LineKind::Art);
        assert_eq!(k[1], LineKind::TableRow);
    }

    /// The ordering contract from "classify_lines"'s doc comment, one case per
    /// numbered step. These are the pairs where an earlier check has to win: if
    /// a check moves, one of these flips.
    #[test]
    fn check_order_is_load_bearing() {
        // 1. A fence range swallows a line that would otherwise be an ATX
        //    header, and a Doxygen verbatim range swallows everything to its
        //    closer.
        assert_eq!(
            kinds(&["```", "# use foo;", "```"], DocFlavor::Rustdoc),
            [
                LineKind::FenceOpen,
                LineKind::FenceContent,
                LineKind::FenceClose
            ]
        );
        assert_eq!(
            kinds(&["@code", "# not a header", "@endcode"], DocFlavor::Doxygen),
            [
                LineKind::DoxyVerbatimOpen,
                LineKind::DoxyVerbatimContent,
                LineKind::DoxyVerbatimClose
            ]
        );

        // 3. A bookend next to art stays art instead of being stripped.
        assert_eq!(
            kinds(&["--- label ---", "|  |"], DocFlavor::None)[0],
            LineKind::Art
        );

        // 5. A Doxygen tag beats the ATX reading of a "#" that follows it, and
        //    the kernel-doc "@name:" form is a tag in any flavor.
        assert_eq!(
            kinds(&["@param x # not a header"], DocFlavor::Doxygen)[0],
            LineKind::DoxygenTag
        );
        assert_eq!(
            kinds(&["@name : desc"], DocFlavor::Rustdoc)[0],
            LineKind::DoxygenTag
        );

        // 6. A setext underline needs prose directly above it; alone it is a
        //    bare rule, not an underline.
        assert_eq!(
            kinds(&["Heading", "======="], DocFlavor::None)[1],
            LineKind::SetextUnderline
        );

        // 7. ATX beats metadata, metadata beats the checks below it.
        assert_eq!(
            kinds(&["# Errors"], DocFlavor::Rustdoc)[0],
            LineKind::AtxHeader
        );
        assert_eq!(
            kinds(&["SPDX-License-Identifier: GPL-2.0"], DocFlavor::None)[0],
            LineKind::Metadata
        );

        // 9. Assignment runs come after indented code too: an aligned mapping
        //    row carrying a code sample's indentation belongs to the sample.
        assert_eq!(
            kinds(&["  a = 1", "  b = 2"], DocFlavor::None),
            [LineKind::IndentedCode, LineKind::IndentedCode]
        );
        assert_eq!(
            kinds(&["a = 1", "b = 2"], DocFlavor::None),
            [LineKind::AssignmentForm, LineKind::AssignmentForm]
        );

        // 10. Label runs come AFTER indented code, so a "Key: value" line with
        //    a code sample's indentation belongs to the sample.
        assert_eq!(
            kinds(&["  File: x.c", "  Task: y"], DocFlavor::None),
            [LineKind::IndentedCode, LineKind::IndentedCode]
        );
        assert_eq!(
            kinds(&["File: x.c", "Task: y"], DocFlavor::None),
            [LineKind::LabelRow, LineKind::LabelRow]
        );

        // 12. Art, list item, then prose as the fallback.
        assert_eq!(kinds(&["- one"], DocFlavor::None)[0], LineKind::ListItem);
        assert_eq!(
            kinds(&["ordinary sentence"], DocFlavor::None)[0],
            LineKind::Prose
        );
    }

    #[test]
    fn label_run_needs_two_rows_that_fit() {
        // A lone "Note: ..." opening a paragraph is a sentence, not a banner.
        assert_eq!(
            kinds(
                &["Note: this is prose", "and it continues here"],
                DocFlavor::None
            )[0],
            LineKind::Prose
        );

        // A row that does not already fit the budget must not be frozen, or it
        // stays over the limit forever.
        let long = "Warning: ".to_string() + &"x".repeat(200);
        let lines = vec![stripped(&long), stripped("Task: y")];
        assert_eq!(
            classify_lines(&lines, DocFlavor::None, 80)[0],
            LineKind::Prose
        );
    }

    #[test]
    fn verbatim_region_survives_an_unclosed_opener() {
        // No closer: every later line stays content rather than reverting to
        // prose halfway through the sample.
        assert_eq!(
            kinds(&["@verbatim", "raw", "more raw"], DocFlavor::Doxygen),
            [
                LineKind::DoxyVerbatimOpen,
                LineKind::DoxyVerbatimContent,
                LineKind::DoxyVerbatimContent
            ]
        );
    }

    #[test]
    fn doxygen_tags_are_flavor_gated() {
        assert_eq!(
            kinds(&["@param x foo"], DocFlavor::Doxygen)[0],
            LineKind::DoxygenTag
        );
        assert_eq!(
            kinds(&["@param x foo"], DocFlavor::None)[0],
            LineKind::Prose
        );
        // A verbatim opener is structural, so it fires in any flavor.
        assert_eq!(
            kinds(&["@code"], DocFlavor::Rustdoc)[0],
            LineKind::DoxyVerbatimOpen
        );
    }

    #[test]
    fn tag_table_shapes() {
        assert_eq!(doxy_tag("param"), Some(TagShape::Named));
        assert_eq!(doxy_tag("brief"), Some(TagShape::Plain));
        assert_eq!(doxy_tag("code"), Some(TagShape::VerbatimOpen));
        assert_eq!(doxy_tag("endverbatim"), Some(TagShape::VerbatimClose));
        assert_eq!(doxy_tag("nosuchtag"), None);
        // The direction annotation is not part of the keyword.
        assert_eq!(doxy_keyword("param[in] buf"), "param");
        assert_eq!(doxy_keyword("return the value"), "return");
        assert_eq!(doxy_keyword("123"), "");
    }
}
