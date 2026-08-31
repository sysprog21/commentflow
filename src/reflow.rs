use unicode_width::UnicodeWidthStr;

use crate::classify::{DocFlavor, Style};
use crate::config::IndentConfig;
use crate::linekind::{TagShape, doxy_keyword, doxy_tag, looks_like_email_or_path};
use crate::normalize::{LineKind, NormalizedDoc, Paragraph, ParagraphKind};
use crate::textline::{advance_col, bookend_match, is_kernel_doc_tag, split_at_return_boundary};

const MIN_WRAP_WIDTH: usize = 20;
const CONTINUATION_INDENT: usize = 4;

/// Display-column width of a string, honoring tab-stop semantics: a tab
/// advances to the next multiple of "tab_width" (matching what terminals
/// and clang-format do), not always "+tab_width". Shares the per-char
/// stepper with normalize::advance_col.
fn display_width(s: &str, tab_width: usize) -> usize {
    s.chars()
        .fold(0usize, |col, c| advance_col(col, c, tab_width))
}

/// Lay out one normalized comment as the source text that replaces it. An
/// empty result means "emit nothing"; the caller then leaves the comment as it
/// found it.
pub fn reflow(doc: &NormalizedDoc, column_limit: usize, indent_cfg: IndentConfig) -> String {
    let mut out_lines: Vec<String> = Vec::new();
    let prefix = line_prefix_string(doc);
    let tab_width = indent_cfg.tab_width;

    if matches!(doc.style, Style::Block | Style::DocBlock) {
        let mut body_lines: Vec<String> = Vec::new();
        emit_paragraphs(doc, &prefix, column_limit, &mut body_lines, true, tab_width);

        let opener_marker = doc
            .block_opener_marker
            .as_deref()
            .unwrap_or(match doc.style {
                Style::DocBlock => "/**",
                _ => "/*",
            });
        let opener_indent = &doc.indent;
        let closer = format!("{opener_indent} */");

        // The first line of the replacement starts AT the comment's start_byte;
        // whatever indentation exists on the source line before that byte is
        // outside the replacement range and stays as-is. So the opener line
        // emits the marker without prepending "opener_indent", which would
        // double the indent. Continuation lines and the closer DO get the
        // indent because they follow newlines we generate.
        if doc.is_file_header {
            out_lines.push(opener_marker.to_string());
            out_lines.extend(body_lines);
        } else if doc.lines.len() == 1
            && body_lines.iter().all(|line| {
                strip_known_prefix(line, &prefix, &doc.default_continuation_prefix).is_empty()
            })
        {
            out_lines.push(format!("{opener_marker} */"));
            return join_lines(&out_lines, doc);
        } else if let Some(first) = body_lines.first().cloned() {
            let first_trimmed = first.trim_start_matches([' ', '\t']);
            let first_has_raw_opener = first_trimmed.starts_with(opener_marker);
            let first_has_raw_closer = first_trimmed.ends_with("*/");
            if first_has_raw_opener {
                // Preformatted paragraphs replay raw source lines. For an
                // inline block comment whose first body line is preformatted,
                // that raw line already includes the original block opener, so
                // do not wrap it with another opener.
                out_lines.push(first);
            } else {
                // Strip the exact continuation prefix that emit_paragraphs put
                // on the line, not a generic "*"-plus-optional-space pattern.
                // Using the generic pattern leaks bytes when the source used
                // "**" continuation (the second star would survive into the
                // content).
                let body_content =
                    strip_known_prefix(&first, &prefix, &doc.default_continuation_prefix);
                let first_line = format!("{opener_marker} {body_content}");
                let first_line = first_line.trim_end().to_string();

                // Collapse a one-content-line block to "/* text */" when the
                // single-line form still fits. The body's leading indent lives
                // outside the replacement range, so charge it explicitly. Only
                // the plain prose case reaches here: file headers, preformatted
                // bodies (raw opener), and raw-closer lines take other branches
                // and keep their shape.
                if body_lines.len() == 1 && !first_has_raw_closer {
                    let collapsed = format!("{first_line} */");

                    // Measure indent + line as ONE string: a tab in the content
                    // advances to a stop relative to the indent, so summing the
                    // two widths separately would misjudge the fit.
                    let width = display_width(&format!("{}{collapsed}", doc.indent), tab_width);
                    if width <= column_limit {
                        out_lines.push(collapsed);
                        return join_lines(&out_lines, doc);
                    }
                }
                out_lines.push(first_line);
            }
            out_lines.extend(body_lines.into_iter().skip(1));
            if first_has_raw_closer {
                return join_lines(&out_lines, doc);
            }
        } else {
            out_lines.push(opener_marker.to_string());
        }

        // An unterminated fence (no matching closer inside the comment) absorbs
        // every subsequent line as FenceContent, including the comment's own
        // "*/" line. That line is then replayed raw by the preformatted
        // emitter, so the body already ends with the closer. Appending another
        // "*/" here would duplicate it, and each subsequent pass would add one
        // more. "*/" can never legitimately appear inside a C block-comment
        // body (tree-sitter would split the comment there), so a body line
        // ending in "*/" always means the closer was raw-captured.
        let body_ends_with_closer = out_lines
            .last()
            .is_some_and(|l| l.trim_end().ends_with("*/"));
        if !body_ends_with_closer {
            out_lines.push(closer);
        }
    } else {
        emit_paragraphs(doc, &prefix, column_limit, &mut out_lines, false, tab_width);

        // The first line of the replacement starts AT the comment's start_byte;
        // the source line's leading whitespace before that byte is outside the
        // replacement range. Emit the first line WITHOUT the indent, or we
        // double-indent it. Continuation lines (if any) keep the indent because
        // they follow newlines we generate.
        if let Some(first) = out_lines.first_mut()
            && !doc.indent.is_empty()
            && let Some(stripped) = first.strip_prefix(doc.indent.as_str())
        {
            *first = stripped.to_string();
        }
    }

    join_lines(&out_lines, doc)
}

fn line_prefix_string(doc: &NormalizedDoc) -> String {
    // Shell ("#") and C/C++/Rust ("//") share Style::Line; the per-comment
    // line_marker decides which marker to emit so we never convert "#" to "//"
    // or vice versa.
    let base = match doc.style {
        Style::Line => doc.line_marker.as_deref().unwrap_or("//"),
        Style::DocLine => doc.line_marker.as_deref().unwrap_or("///"),
        Style::Block | Style::DocBlock => doc.default_continuation_prefix.as_str(),
    };
    if matches!(doc.style, Style::Line | Style::DocLine) {
        format!("{}{} ", doc.indent, base)
    } else {
        format!("{}{}", doc.indent, base)
    }
}

fn emit_paragraphs(
    doc: &NormalizedDoc,
    prefix: &str,
    column_limit: usize,
    out: &mut Vec<String>,
    skip_first_emit_inline_opener: bool,
    tab_width: usize,
) {
    let prefix_width = display_width(prefix, tab_width);
    let effective = column_limit.saturating_sub(prefix_width);

    if doc.paragraphs.is_empty() && doc.lines.iter().any(|l| l.kind == LineKind::Blank) {
        out.push(blank_prefix(prefix).to_string());
        return;
    }

    for (idx, para) in doc.paragraphs.iter().enumerate() {
        // "preceded_by_blank" is the authoritative source-driven signal for
        // "insert a blank comment line before this paragraph". Earlier code
        // also gated on a "prev_was_blank" flag, but that flag never flipped to
        // true under the current paragraph grouping (Blank lines are skipped
        // during grouping, not emitted as paragraphs), so the gate was a no-op.
        if idx > 0 && para.preceded_by_blank {
            out.push(blank_prefix(prefix).to_string());
        }
        match para.kind {
            ParagraphKind::Preformatted => {
                // Replay the original raw source line so fences, art, tables,
                // blockquotes, reference links, and indented code pass through
                // intact. The one normalization applied even here: a block
                // comment's leading "**" star-run collapses to a single "*"
                // (strict single-star continuations); the content after the
                // marker is untouched, so alignment survives.
                let is_block = matches!(doc.style, Style::Block | Style::DocBlock);
                for &li in &para.line_indices {
                    let raw = &doc.lines[li].raw;

                    // A banner row is the one preformatted kind whose bytes are
                    // ordinary text, not layout: re-emit it behind the
                    // canonical prefix so a drifted "**" marker and a stripped
                    // decorative bookend land like every reflowed sibling. The
                    // body itself still goes out verbatim, unwrapped. Metadata
                    // keeps raw replay: a license block is not ours to retouch.
                    if matches!(doc.lines[li].kind, LineKind::LabelRow | LineKind::Banner)
                        && !(skip_first_emit_inline_opener && li == 0)
                    {
                        out.push(
                            format!("{prefix}{}", doc.lines[li].text)
                                .trim_end()
                                .to_string(),
                        );
                        continue;
                    }
                    if is_block {
                        if skip_first_emit_inline_opener && li == 0 {
                            let body = strip_leading_block_opener(raw, doc);
                            out.push(format!("{prefix}{body}").trim_end().to_string());
                            continue;
                        }
                        out.push(collapse_block_marker_stars(raw));
                    } else {
                        out.push(raw.clone());
                    }
                }
            }
            ParagraphKind::AtxHeader => {
                let li = para.line_indices[0];
                let mut full = prefix.to_string();
                full.push_str(doc.lines[li].text.trim());
                out.push(full);
            }
            ParagraphKind::Prose => {
                emit_prose_paragraph(doc, para, prefix, effective, out);
            }
        }
    }
}

fn strip_leading_block_opener<'a>(line: &'a str, doc: &NormalizedDoc) -> &'a str {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let marker = doc.block_opener_marker.as_deref().unwrap_or("/*");
    if let Some(rest) = trimmed.strip_prefix(marker) {
        rest.strip_prefix(' ').unwrap_or(rest)
    } else {
        trimmed
    }
}

/// Strip a known prefix sequence from an emitted continuation line, leaving
/// just the content. Tries the full emitted prefix first, then the
/// default continuation prefix as a fallback. This avoids the byte-leak
/// where a generic "*"-plus-space strip would only consume one star of a
/// "**"-style continuation, leaking the second star into content.
fn strip_known_prefix<'a>(line: &'a str, full_prefix: &str, cont_prefix: &str) -> &'a str {
    if let Some(rest) = line.strip_prefix(full_prefix) {
        return rest;
    }
    if let Some(rest) = line.strip_prefix(cont_prefix.trim_end()) {
        return rest.strip_prefix(' ').unwrap_or(rest);
    }
    let after_ws = line.trim_start_matches([' ', '\t']);
    let after_star = after_ws.trim_start_matches('*');
    after_star.strip_prefix(' ').unwrap_or(after_star)
}

fn blank_prefix(prefix: &str) -> &str {
    prefix.trim_end_matches(' ')
}

/// Collapse a block comment's leading star-run MARKER to a single "*",
/// preserving the leading whitespace and everything after the run. A run
/// counts as a marker when it is followed by a space/tab ("** body", at any
/// run length) or is a bare "**" at end of line. A longer bare run ("***"
/// alone) is left intact: it is more likely an intentional star divider than
/// an empty marker line. Single-star or non-marker lines pass through.
fn collapse_block_marker_stars(raw: &str) -> String {
    let ws_len = raw.len() - raw.trim_start_matches([' ', '\t']).len();
    let (ws, rest) = raw.split_at(ws_len);
    let star_len = rest.len() - rest.trim_start_matches('*').len();
    if star_len < 2 {
        return raw.to_string();
    }

    // Only collapse a genuine continuation marker: the star-run must be
    // followed by whitespace (" ** body") or be a bare "**" at end of line. A
    // run glued to content like "**ptr" is a token, not a marker: leave it.
    let after_stars = &rest[star_len..];
    let next_char = after_stars.chars().next();
    let is_marker = matches!(next_char, Some(' ' | '\t')) || (star_len == 2 && next_char.is_none());
    if is_marker {
        format!("{ws}*{after_stars}")
    } else {
        raw.to_string()
    }
}

fn emit_prose_paragraph(
    doc: &NormalizedDoc,
    para: &Paragraph,
    prefix: &str,
    effective: usize,
    out: &mut Vec<String>,
) {
    let mut body = String::new();
    for (n, &li) in para.line_indices.iter().enumerate() {
        let l = &doc.lines[li];
        if n > 0 && !body.ends_with(' ') {
            body.push(' ');
        }
        body.push_str(l.text.trim());
    }
    body = collapse_spaces(&body);

    let segments = split_at_return_boundary(&body);
    for (i, seg) in segments.iter().enumerate() {
        if i > 0 {
            out.push(blank_prefix(prefix).to_string());
        }
        let hang = doxygen_hanging_indent(seg, effective, doc.flavor);
        wrap_segment_aligned(seg, prefix, effective, hang, out);
    }
}

/// How far to indent the continuation lines of a paragraph that begins with a
/// Doxygen tag, so they align under the description column ("@param name" and
/// "@return" consume different amounts of the line). Zero means wrap flush
/// under the prefix, which is the answer for ordinary prose.
///
/// Gated on "flavor": only Doxygen-flavored comments align. A "@param" line in
/// a Rustdoc-flavored block is just prose.
fn doxygen_hanging_indent(body: &str, effective: usize, flavor: DocFlavor) -> usize {
    if !matches!(flavor, DocFlavor::Doxygen) {
        return 0;
    }

    // The kernel-doc form "convert_kernel_doc" emits ("@name : desc"). It is
    // not a tag-table keyword, so the lookup below answers None and its
    // continuations would wrap flush under the prefix while a "@param" entry's
    // align under the description column. Both forms sit in one file the moment
    // one comment converts and its neighbor aborts, so they have to agree.
    //
    // Safe only because the packer refuses to forge this shape mid-paragraph
    // (see "would_forge_kdoc_tag" below). A nonzero hang is what makes an
    // invented tag line visible: "classify_lines" has always split a paragraph
    // at one, but with a flush wrap the regrouping moved no bytes.
    //
    // The description column is whatever follows the colon and its space. A
    // name is "[A-Za-z0-9_]+", so everything up to the colon is ASCII and the
    // byte offset is the column.
    let trimmed = body.trim_start();
    if is_kernel_doc_tag(trimmed) {
        let after_colon = trimmed.find(':').map_or(0, |i| i + 1);
        let indent = after_colon + usize::from(trimmed[after_colon..].starts_with(' '));
        return if effective <= indent + MIN_WRAP_WIDTH {
            CONTINUATION_INDENT
        } else {
            indent
        };
    }

    // Both spellings of a tag, "@param" and "\param", align the same way: strip
    // whichever marker is there and look the bare keyword up in the one tag
    // table. A keyword the table doesn't know, or one not followed by a space,
    // is ordinary prose. A verbatim marker never carries a description to
    // align, and its region is emitted preformatted anyway.
    let Some(rest) = body.trim_start().strip_prefix(['@', '\\']) else {
        return 0;
    };
    let keyword = doxy_keyword(rest);
    let takes_name = match doxy_tag(keyword) {
        Some(TagShape::Named) => true,
        Some(TagShape::Plain) => false,
        Some(TagShape::VerbatimOpen | TagShape::VerbatimClose) | None => return 0,
    };
    let Some(after_tag) = rest[keyword.len()..].strip_prefix(' ') else {
        return 0;
    };

    // The description column: past the marker, the keyword, and (for a tag that
    // takes one) the name, each followed by its space. Table keywords are
    // ASCII, so their byte length is their column count.
    let mut indent = 1 + keyword.len() + 1;
    if takes_name {
        let name_chars = after_tag.chars().take_while(|c| !c.is_whitespace()).count();
        if name_chars == 0 {
            return 0;
        }
        indent += name_chars + 1;
    }

    // Aligning that far would leave too little room to wrap into. Fall back to
    // a fixed hanging indent rather than a near-zero (or negative) wrap width.
    if effective <= indent + MIN_WRAP_WIDTH {
        return CONTINUATION_INDENT;
    }
    indent
}

/// A word that would read as a Doxygen or kernel-doc tag if it opened a line.
///
/// "looks_like_email_or_path" is the guard "classify_lines" applies, reused
/// here so the two cannot disagree: "doxy_keyword" truncates "@file.txt" at the
/// dot and would otherwise read it as the "file" tag. Skipping such a word is
/// safe as well as tidier, since "kdoc_tag_of" matches the whole keyword and no
/// convertible spelling carries a dot or a slash.
///
/// Not gated on "DocFlavor::Doxygen" the way "classify_lines" is, and that
/// asymmetry is deliberate. The pass that eats a parked tag,
/// "normalize::convert_kernel_doc", keys off the source Language, while the
/// flavor of a plain "//" or "/* */" comment is "None" even in C. Gate this on
/// flavor and every non-doc C comment loses its protection.
fn is_tag_start(word: &str) -> bool {
    let Some(rest) = word.strip_prefix(['@', '\\']) else {
        return false;
    };
    (doxy_tag(doxy_keyword(rest)).is_some() && !looks_like_email_or_path(rest))
        || is_kernel_doc_tag(word)
}

/// A line holding nothing but "@name", the one shape a following ":"-led word
/// turns into the kernel-doc tag form. Every other way to end a line in "@name"
/// leaves an earlier word in front of it, and "is_kernel_doc_tag" reads the
/// line start, so those are already safe.
fn is_lone_kdoc_name(line: &str) -> bool {
    line.strip_prefix('@').is_some_and(|name| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Greedy line-packing of one paragraph. The first line goes out behind
/// "prefix" with the full "effective" width; every continuation line is
/// indented a further "hang" columns and wraps that much earlier.
fn wrap_segment_aligned(
    body: &str,
    prefix: &str,
    effective: usize,
    hang: usize,
    out: &mut Vec<String>,
) {
    let hanging_prefix = (hang > 0).then(|| format!("{prefix}{}", " ".repeat(hang)));
    let cont_prefix = hanging_prefix.as_deref().unwrap_or(prefix);
    let effective_cont = effective.saturating_sub(hang);

    // Track the current line's display width incrementally instead of
    // recomputing UnicodeWidthStr::width(current) for every appended word.
    // Words have no internal whitespace (we split on whitespace), so each word
    // contributes width = UnicodeWidthStr::width(w).
    let mut current = String::new();
    let mut current_width = 0usize;

    // Whether we are still filling the first line is derived, not tracked. This
    // paragraph owns every line it appends from here on, so "appended nothing
    // yet" and "still on line 1" are the same fact, and a flag for it is a
    // fourth assignment that can disagree with reality.
    let first_line_mark = out.len();

    // Where the last word on the line starts, so the tag rule below can peel it
    // off without rescanning "current" for the final space. "None" means the
    // line holds a single word, which cannot be peeled: nothing would be left
    // to emit in front of it.
    let mut last_word: Option<usize> = None;

    for w in body.split_whitespace() {
        let on_first_line = out.len() == first_line_mark;
        let avail = if on_first_line {
            effective
        } else {
            effective_cont
        };

        // Tracked state has one failure mode a rescan does not: it can fall out
        // of step with the string. This is the rescan, kept as a debug-only
        // cross-check so a desync fails a test run instead of silently parking
        // a tag at a line start. Compiled out of release, where the point is
        // not paying for it.
        debug_assert_eq!(
            last_word,
            current.rfind(' ').map(|i| i + 1),
            "last_word drifted from the actual last-word start"
        );
        let w_width = UnicodeWidthStr::width(w);
        if current.is_empty() {
            current.push_str(w);
            current_width = w_width;
            last_word = None;
            continue;
        }

        // Appending is never guarded, only finalizing is. A line that packing
        // turned into a bookend ("-------- x --------") cannot escape: either a
        // later word does not fit, and the emit guard below refuses to send it
        // out, or the paragraph ends on it and the borrow at the bottom does.
        // Checking here as well was tried and deleted; it failed no test that
        // those two do not already cover, and it cost a scan per word.
        //
        // The one exception to "appending is never guarded", and it decides the
        // append outright rather than voting on it: a lone "@name" about to
        // take a ":"-led word is the kernel-doc tag shape, and which line it
        // sits on is the whole question. Width does not get a say either way.
        //
        // On a CONTINUATION line the join FORGES a tag the source never had,
        // and "classify_lines" splits a paragraph at one: the next pass
        // regroups the text around it and the hanging indent moves with the
        // regrouping. Falling through emits "@name" on its own and opens the
        // next line with the ":" word, which loses nothing and breaks one word
        // early. Guarding the finalize path instead cannot work here the way it
        // does for bookends: "is_kernel_doc_tag" reads the line START, so once
        // a line holds "@name :" no further word clears it and "keep packing"
        // never terminates. The forging step is the only place to stand.
        //
        // On the paragraph's OPENING line the join is mandatory, because
        // "@name" and its ":" are one unit: a first line stopping at "@name" is
        // not a tag line on the next pass, so the paragraph merges into the one
        // above and the hang goes with it. Overflow instead, the same trade the
        // tag rule takes: an over-long line is recoverable, a regrouped one
        // settles a run late.
        let kdoc_colon_pending =
            last_word.is_none() && w.starts_with(':') && is_lone_kdoc_name(&current);
        let takes_word = if kdoc_colon_pending {
            on_first_line
        } else {
            current_width + 1 + w_width <= avail
        };
        if takes_word {
            last_word = Some(current.len() + 1);
            current.push(' ');
            current.push_str(w);
            current_width += 1 + w_width;
            continue;
        }

        // Past here the word does not fit, so a line is going out.

        // Never hand the next pass a line the bookend stripper would rewrite. A
        // rule token in prose ("a --- b") that a narrow wrap leaves alone on
        // its line reads as a bare rule on the next run and is deleted, so the
        // file settles only on the second one. Same trade as the tag rule
        // below: keep packing and overflow, because an over-long line is
        // recoverable and a deleted one is not.
        if bookend_match(&current).is_some() {
            last_word = Some(current.len() + 1);
            current.push(' ');
            current.push_str(w);
            current_width += 1 + w_width;
            continue;
        }

        let line_prefix = if on_first_line { prefix } else { cont_prefix };
        if is_tag_start(w) {
            // Keep the tag off the first column of a continuation line: the
            // next normalization pass would read it there as a kernel-doc
            // region and eat the tag word. Break one word earlier so the tag
            // lands second instead.
            //
            // That only works when the word moved down is not itself a tag;
            // otherwise the break just relocates the problem. With no such
            // word, let the line overflow. An over-long line is recoverable, a
            // deleted word is not. Peeling must not undo the bookend guard
            // above: breaking "--- x" in front of a tag emits "---" alone,
            // which is the very line that guard exists to prevent. Fall through
            // to the overflow arm.
            match last_word {
                Some(start)
                    if !is_tag_start(&current[start..])
                        && bookend_match(&current[..start - 1]).is_none() =>
                {
                    let last = current.split_off(start);
                    current.truncate(start - 1);
                    out.push(format!("{line_prefix}{current}"));
                    last_word = Some(last.len() + 1);
                    current = format!("{last} {w}");
                    current_width = UnicodeWidthStr::width(current.as_str());
                }
                _ => {
                    last_word = Some(current.len() + 1);
                    current.push(' ');
                    current.push_str(w);
                    current_width += 1 + w_width;
                }
            }
            continue;
        }
        out.push(format!("{line_prefix}{current}"));
        current.clear();
        current.push_str(w);
        current_width = w_width;
        last_word = None;
    }
    if current.is_empty() {
        return;
    }

    // The same rule at the paragraph's end, where there is no next word to pack
    // with. Borrow one from the line above so the rule is not alone, taking
    // care that neither resulting line is a bookend either: folding the rule
    // straight onto "-------- x y" would build the two-sided banner this is
    // avoiding. Only the paragraph's first line has nothing to borrow from, and
    // a paragraph that is one bare rule was the stripper's business long before
    // reflow saw it.
    //
    // The tests below take the line above as a BODY, not as the emitted line.
    // Every other bookend test in this file takes a body, and a " * "
    // continuation prefix would hide a leading rule run from the scan. Which
    // prefix that line went out behind is known exactly: only the paragraph's
    // first line uses "prefix".
    if bookend_match(&current).is_some() && out.len() > first_line_mark {
        let prev_prefix = if out.len() == first_line_mark + 1 {
            prefix
        } else {
            cont_prefix
        };
        let last = out.len() - 1;
        let prev_body = out[last]
            .strip_prefix(prev_prefix)
            .unwrap_or(&out[last])
            .to_string();

        // The borrowed text opens the paragraph's last line, and that line is
        // always a continuation, so the tag rule above applies to it just as it
        // does to a mid-paragraph break: a tag parked at the first column is
        // read by "classify_lines" as its own tag paragraph, the next pass
        // regroups around it, and the hanging indent moves with the regrouping.
        //
        // Every split point is tried, longest prefix first, so the borrow can
        // take two words or more. Borrowing exactly one is not enough on a line
        // that carries rule runs, and that is the line this arm sees most: the
        // one word above the rule is often a tag, which may not open a line,
        // while the prefix left behind by taking it is often a bare rule, which
        // may not stand alone. Both fire at once on "-------- \result @1buf:",
        // where the only safe cut is two words up. Taking the single-word split
        // as the whole search emitted the rule alone, and the next pass read
        // that as a bare rule and deleted it, losing a word.
        //
        // When nothing can be borrowed, or every borrow builds one of the
        // shapes above, fold the rule onto the line above whole. That arm used
        // to sit inside the "there is a word to borrow" case, so a line holding
        // ONE word (no space to split at) fell through and emitted the rule
        // alone anyway.
        let opens_with_tag = |seg: &str| {
            // "is_tag_start" reads one word; "is_kernel_doc_tag" reads a line
            // start, so it also catches the spaced "@name : desc" form, whose
            // first word alone ("@name") is not a tag.
            is_tag_start(seg.split_whitespace().next().unwrap_or("")) || is_kernel_doc_tag(seg)
        };

        // A split is usable when the text moved down may open a line and
        // neither resulting line is a bookend. The prefix left behind gets one
        // extra chance: a bare rule there may be folded onto the line above it,
        // which is how the rule-led case resolves. "-------- \result @1buf:"
        // has no usable split on its own, since the only cut that frees a
        // non-tag opener strands "--------"; folding that run one line further
        // up yields "@param.txt @1buf --------" over "\result @1buf: ***", two
        // lines the next pass rewrites neither of.
        let above = |i: usize| {
            let p = if i == first_line_mark {
                prefix
            } else {
                cont_prefix
            };
            (p, out[i].strip_prefix(p).unwrap_or(&out[i]).to_string())
        };
        let usable = prev_body
            .match_indices(' ')
            .map(|(space, _)| space)
            .rev()
            .find_map(|space| {
                let borrowed = &prev_body[space + 1..];
                if opens_with_tag(borrowed)
                    || bookend_match(&format!("{borrowed} {current}")).is_some()
                {
                    return None;
                }
                let head = &prev_body[..space];
                if bookend_match(head).is_none() {
                    return Some((space, false));
                }
                if last <= first_line_mark {
                    return None;
                }
                let (_, above_body) = above(last - 1);
                bookend_match(&format!("{above_body} {head}"))
                    .is_none()
                    .then_some((space, true))
            });
        if let Some((space, fold_head)) = usable {
            if fold_head {
                let (above_prefix, above_body) = above(last - 1);
                out[last - 1] = format!("{above_prefix}{above_body} {}", &prev_body[..space]);
                out.remove(last);
            } else {
                out[last] = format!("{prev_prefix}{}", &prev_body[..space]);
            }
            current = format!("{} {current}", &prev_body[space + 1..]);
        } else if bookend_match(&format!("{prev_body} {current}")).is_none() {
            out[last] = format!("{prev_prefix}{prev_body} {current}");
            return;
        }
    }
    let line_prefix = if out.len() == first_line_mark {
        prefix
    } else {
        cont_prefix
    };
    out.push(format!("{line_prefix}{current}"));
}

fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for c in s.chars() {
        if c == ' ' || c == '\t' {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out
}

fn join_lines(lines: &[String], doc: &NormalizedDoc) -> String {
    // Use the DOMINANT line ending in the source comment, not "any CRLF". A
    // stray CRLF in an otherwise-LF source no longer flips everything to CRLF.
    // Tie goes to LF (the spec rule).
    //
    // Exclude the last line: it is terminated by "*/", not a newline, so its
    // "had_crlf" is always false and carries no ending signal. Counting it
    // undercounts CRLF by one, which flips the tie toward LF as soon as a
    // comment shrinks to an even line count, making reflow non-idempotent
    // across a merge that changes the physical line count between passes.
    let counted = &doc.lines[..doc.lines.len().saturating_sub(1)];
    let crlf_count = counted.iter().filter(|l| l.had_crlf).count();
    let lf_count = counted.len() - crlf_count;

    // No interior line carries an ending signal: a single source line that
    // reflow is now wrapping. There is nothing to vote on, so fall back to the
    // spec hierarchy instead of the LF that a 0-0 tie would otherwise pick. For
    // line comments ("//", "#") tree-sitter captures the trailing "\r" inside
    // the node, so the lone line's own "had_crlf" is the truth; for block
    // comments the node ends at "*/" and carries no "\r", so the post-span
    // ending resolved at extract time ("fallback_ending") applies.
    let ending = if counted.is_empty() {
        if doc.lines.last().is_some_and(|l| l.had_crlf) {
            "\r\n"
        } else {
            doc.fallback_ending
        }
    } else if crlf_count > lf_count {
        "\r\n"
    } else {
        "\n"
    };
    let mut text = String::new();
    for (i, l) in lines.iter().enumerate() {
        text.push_str(l);
        if i + 1 < lines.len() {
            text.push_str(ending);
        }
    }
    text
}
