// Comments in this crate quote identifiers with "double quotes" rather than
// markdown code spans, so clippy's pedantic doc_markdown lint fires on every
// one of them. The convention is deliberate and the lint cannot be satisfied
// without reverting it, so silence that one lint and keep the rest of the
// pedantic group usable as a signal.
#![allow(clippy::doc_markdown)]

pub mod asm;
pub mod classify;
pub mod cli;
pub mod config;
pub mod convert;
pub mod diff;
pub mod linekind;
pub mod normalize;
pub mod parse;
pub mod reflow;
pub mod rewrite;

// Nothing outside the crate calls into this module; "parse" is the only caller.
// Keeping it crate-private says so, rather than publishing a module whose every
// item is "pub(crate)" anyway.
pub(crate) mod signature;
pub mod textline;

/// Byte range to delete when a comment is moved out of its spot (transforms 3
/// and 4). Three cases, none of which may cover a code byte:
/// - Alone on its line (only whitespace before it and after it to end-of-line):
///   drop the whole line, trailing newline included, so the line collapses.
/// - Shares its line with code, "backward" (an after-")" or trailing comment):
///   remove the comment and the whitespace gap *before* it, keeping the code.
/// - Shares its line with code, forward (a leading "/* c */ TYPE" comment):
///   remove the comment and the whitespace gap *after* it, up to the next token.
///
/// The whitespace walks stay within the comment's own line, so a comment that
/// ends a code line ("code, /* c */\n") can never reach back over the code.
fn comment_move_delete_span(source: &str, c: &parse::Comment, backward: bool) -> (usize, usize) {
    let bytes = source.as_bytes();
    let line_start = parse::line_start_before(source, c.start_byte);
    let before_ws = bytes[line_start..c.start_byte]
        .iter()
        .all(|&b| b == b' ' || b == b'\t');
    let mut after = c.end_byte;
    while after < source.len() && matches!(bytes[after], b' ' | b'\t') {
        after += 1;
    }
    let after_eol = after >= source.len() || matches!(bytes[after], b'\n' | b'\r');
    if before_ws && after_eol {
        let end = if source[after..].starts_with("\r\n") {
            after + 2
        } else if after < source.len() {
            after + 1
        } else {
            after
        };
        (line_start, end)
    } else if backward {
        let mut s = c.start_byte;
        while s > line_start && matches!(bytes[s - 1], b' ' | b'\t') {
            s -= 1;
        }
        (s, c.end_byte)
    } else {
        (c.start_byte, after)
    }
}

/// A genuinely multi-line, own-line comment reads better with a blank line
/// separating it from the code above: the common AI-generated shape drops the
/// comment flush against the statement it follows. When one needs that blank
/// line, return the extended replacement start (the comment's line start) and
/// the text to prepend there: a newline plus the comment's own indentation, so
/// the splice replaces "[line_start..end_byte]" with a blank line then the
/// unchanged comment. "None" leaves the comment where it is.
///
/// Skipped when the comment is the first thing inside a block, which is the
/// previous line ending in "{" (the Xlib first-statement-in-body shape), in
/// ":" (a label, a "case", a C++ access specifier), or opening a preprocessor
/// conditional. Skipped too when the comment is wedged directly before a
/// body-opening "{" (a comment between a function signature and its body, the
/// unrelocated manual-page position, belongs with the function, not split off
/// above), when a blank line is already there, when the comment sits at file
/// start, when it sits directly under the file's "#!" shebang (the header
/// belongs with the preamble, and the rule would otherwise fire on every shell
/// script there is), and when the previous line is itself a comment (don't
/// fracture a stacked comment). Doc comments are never touched at all: a blank
/// line detaches a Rust or Doxygen doc comment from the item it documents.
/// "multiline" says the comment is still multi-line after reflow; one that the
/// source split needlessly collapses to a single line no longer qualifies.
///
/// Ceiling: a multi-line comment wedged mid-expression, between the lines of a
/// call's argument list, also gets a blank line, which reads oddly. The
/// previous line ends with a comma or an open paren rather than "{", so the
/// block guard cannot see it. Rare for genuine explanatory comments; refine
/// the guard if it bites.
fn blank_line_before(
    source: &str,
    c: &parse::Comment,
    style: classify::Style,
    multiline: bool,
    prev_comment: Option<&parse::Comment>,
    lang: parse::Language,
) -> Option<(usize, String)> {
    if c.is_trailing || !multiline {
        return None;
    }
    if !matches!(style, classify::Style::Line | classify::Style::Block) {
        return None;
    }
    let line_start = parse::line_start_before(source, c.start_byte);
    if line_start == 0 {
        return None;
    }
    let prev_start = parse::line_start_before(source, line_start - 1);
    let mut directive_start = prev_start;
    let mut directive_end = line_start - 1;
    while directive_start > 0 {
        let before_end = directive_start - 1;
        let before_start = parse::line_start_before(source, before_end);
        if !parse::ends_with_line_splice(&source[before_start..before_end]) {
            break;
        }
        directive_start = before_start;
        directive_end = before_end;
    }
    let prev = source[prev_start..line_start].trim();
    if prev.is_empty() || prev.ends_with('{') || prev.ends_with(':') {
        return None;
    }

    // One parser for the two "#" lines that mean "this comment is not
    // explaining me". Splitting them was how the BOM strip below ended up on
    // only one of the pair, which left a BOM'd file whose first line is
    // "#ifndef GUARD" failing a test its BOM-free twin passes. U+FEFF is not
    // Unicode White_Space, so "trim" leaves it glued to the "#".
    //
    // - "#!" on line 1 of a SHELL file is the file's preamble. The header
    //   below it belongs flush against it, and without this the rule fires on
    //   essentially every shell script in existence, detaching its header from
    //   line 1 and splitting a "#! nix-shell -i bash" run off the shebang it
    //   belongs to, which some interpreters require. Gated on "prev_start == 0"
    //   because a "#!" anywhere else is an ordinary comment, per exec(2).
    // - "#if"/"#ifdef"/"#ifndef"/"#else"/"#elif" open a conditional block, so
    //   the comment is the first thing inside it: the "{" case above in its
    //   "#"-directive form. "#endif"/"#define"/"#include" open no scope and are
    //   left alone.
    if let Some(rest) = source[directive_start..directive_end]
        .trim()
        .trim_start_matches('\u{feff}')
        .trim_start()
        .strip_prefix('#')
    {
        // "#!" takes "rest" untrimmed: a shebang is the two bytes "#!" with
        // nothing between them, so "# !x" is an ordinary comment. The
        // directives take the trimmed form, because "# if" is valid cpp.
        //
        // Shell only, and that gate is load-bearing rather than tidiness:
        // "#![no_std]" is a Rust inner attribute in exactly the same position,
        // and reading it as a shebang suppressed the blank line under it. The
        // extensionless-shebang carve-out lands on Shell too, so the scripts
        // that need this still get it.
        let d = rest.trim_start();
        if (lang == parse::Language::Shell && prev_start == 0 && rest.starts_with('!'))
            || d.starts_with("if")
            || d.starts_with("else")
            || d.starts_with("elif")
        {
            return None;
        }
    }

    // Don't fracture a stacked comment. Comments are sorted and disjoint, so
    // only the immediately preceding one can overlap the previous line.
    // Rescanning the whole slice here made "plan" quadratic in comment count.
    if prev_comment.is_some_and(|o| o.start_byte < line_start && o.end_byte > prev_start) {
        return None;
    }

    // Assembly's comment list is deliberately incomplete: the scanner claims
    // "/* */" and nothing else, so a target-specific line comment above this
    // one is invisible to the check above. Fall back to reading the previous
    // line, or transform 5 splits a stacked ".s" header in two.
    if lang == parse::Language::Asm && crate::asm::opens_line_comment_at(prev) {
        return None;
    }

    // Comment wedged directly before a body-opening "{": it documents the
    // function whose signature sits above, so keep it there rather than
    // splitting the signature from it.
    if source[c.end_byte..].trim_start().starts_with('{') {
        return None;
    }
    let nl = if source[..line_start].ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    Some((
        line_start,
        format!("{nl}{}", &source[line_start..c.start_byte]),
    ))
}

/// A rewritten block comment must still be a block comment: an opener, a
/// closer that does not overlap it, and no interior "*/" that would end the
/// comment early and turn the remainder into code.
///
/// This is a postcondition, not a transform. The emitter replays preformatted
/// body lines verbatim, and for the comment's *last* source line that raw text
/// still carries the original "*/"; when the body is degenerate enough that the
/// opener and that closer overlap ("/*" + "/" + "*/" collapsing to "/*/"), the
/// result is an unterminated comment that swallows the rest of the file. Rather
/// than special-case each degenerate body, refuse to emit anything that is not
/// a well-formed comment and leave the source untouched: "when in doubt, do
/// not reflow".
fn block_comment_well_formed(text: &str) -> bool {
    let t = text.trim_end();
    if t.len() < 4 || !t.starts_with("/*") || !t.ends_with("*/") {
        return false;
    }
    !t[2..t.len() - 2].contains("*/")
}

/// Run the comment pipeline (parse → classify → normalize → reflow → rewrite)
/// and return the validated replacements, sorted-safe for "rewrite::apply".
/// The single source of truth for the pipeline, so reflow rules cannot drift
/// between callers. "--to-blocks" runs "convert" ahead of this and then comes
/// through here like everything else.
pub fn plan(
    source: &str,
    lang: parse::Language,
    column_limit: usize,
    indent: config::IndentConfig,
    pool: &mut parse::ParserPool,
) -> anyhow::Result<Vec<rewrite::Replacement>> {
    let comments = parse::extract_comments_with(source, lang, pool)?;
    let mut out = Vec::new();
    for (i, c) in comments.iter().enumerate() {
        // Drifted parameter comment: move it verbatim to trail the parameter it
        // describes ("Comment::param_shift"), leaving the drifted spot's code
        // intact. A leading comment ("/* c */ TYPE name") is deleted with its
        // trailing gap up to the type; the after-")" comment is deleted with
        // the gap before it. The verbatim text is re-inserted with one leading
        // space at the target (the described parameter's declarator end).
        if let Some(shift) = c.param_shift {
            let (del_start, del_end) = comment_move_delete_span(source, c, shift.after_paren);
            out.push(rewrite::Replacement {
                start: del_start,
                end: del_end,
                text: String::new(),
            });

            // Re-insert verbatim, except collapse a multi-line block to one
            // line (its interior whitespace to single spaces) so the moved
            // comment never becomes a glued trailing block the closer-split
            // would reflow on the next run, keeping the transform idempotent.
            let moved = if c.text.contains(['\n', '\r']) {
                c.text.split_whitespace().collect::<Vec<_>>().join(" ")
            } else {
                c.text.clone()
            };
            out.push(rewrite::Replacement {
                start: shift.insert_at,
                end: shift.insert_at,
                text: format!(" {moved}"),
            });
            continue;
        }
        let kind = classify::classify(&c.text, lang);
        let Some(doc) = normalize::normalize(c, &kind, lang, column_limit) else {
            // Two kinds of block comment skip reflow but still get a glued
            // closing "*/" split onto its own line: trailing blocks, and ACSL
            // annotations. An ACSL body is parser-visible syntax that must stay
            // byte-identical (see "is_acsl_annotation"), but the closer's line
            // is pure layout: Frama-C reads whitespace as whitespace, and the
            // multi-line form is spelled with "*/" alone on the last line. The
            // closer lands under the opener's "*" rather than under the clause
            // column, which is where the last line's own indent would put it.
            // "acsl_closer_split_allowed" holds the guards: every OTHER reason
            // a comment can be pinned as passthrough still vetoes the split.
            let closer_indent = if parse::acsl_closer_split_allowed(source, c, lang) {
                Some(format!("{} ", c.line_indent_bytes))
            } else if !c.force_passthrough && c.is_trailing {
                None
            } else {
                continue;
            };
            if matches!(
                kind.style,
                classify::Style::Block | classify::Style::DocBlock
            ) && let Some(text) =
                normalize::split_trailing_block_closer(&c.text, closer_indent.as_deref())
                && let Some(replacement) = rewrite::make_replacement(c, text, source)
            {
                out.push(replacement);
            }
            continue;
        };
        let rewritten = reflow::reflow(&doc, column_limit, indent);

        // Postconditions. "Comment style is preserved" and "a block comment
        // stays a block comment" are invariants, so check them on the way out
        // rather than trusting every emitter path to uphold them. The raw
        // replay of a preformatted body line is the known offender: a body line
        // that merely looks like an opener ("/**" on the second source line of
        // a plain "/*" block) gets emitted as the comment's opener, silently
        // promoting it to a doc block. On any violation, leave the source
        // untouched.
        if !rewritten.is_empty()
            && (classify::classify_style(&rewritten) != kind.style
                || (matches!(
                    kind.style,
                    classify::Style::Block | classify::Style::DocBlock
                ) && !block_comment_well_formed(&rewritten)))
        {
            continue;
        }
        if doc.manpage
            && let Some(insert_at) = c.relocate_before
            && !rewritten.is_empty()
        {
            // Manual-page block between signature and body: delete it in place
            // and re-insert the cleaned comment ahead of the function.
            // "insert_at" is the function's column-0 line start (see
            // "Comment::relocate_before"); the insert is a zero-width splice
            // there, separated from the function by a blank line.
            //
            // Own-line block: remove the whole line, trailing newline included,
            // so the vacated line collapses. Trailing block (glued after ")" on
            // a code line): remove only the comment and the whitespace gap
            // before it, leaving the code and the line's newline so the "{"
            // stays on its own line.
            let (del_start, del_end) = comment_move_delete_span(source, c, c.is_trailing);
            out.push(rewrite::Replacement {
                start: del_start,
                end: del_end,
                text: String::new(),
            });
            let nl = c.fallback_ending;

            // Keep a blank line above the relocated comment unless one is
            // already there (or it lands at file start). Without it, a comment
            // sitting one newline above the function (a copyright banner, say)
            // would abut the relocated block and "merge_comment_groups" would
            // fuse the two on the next run.
            let before = &source[..insert_at];
            let lead =
                if before.is_empty() || before.ends_with("\n\n") || before.ends_with("\r\n\r\n") {
                    ""
                } else {
                    nl
                };
            out.push(rewrite::Replacement {
                start: insert_at,
                end: insert_at,
                text: format!("{lead}{rewritten}{nl}{nl}"),
            });
            continue;
        }
        let multiline = rewritten.contains('\n');
        let prev_comment = i.checked_sub(1).map(|p| &comments[p]);
        let blank = blank_line_before(source, c, kind.style, multiline, prev_comment, lang);
        match rewrite::make_replacement(c, rewritten, source) {
            Some(mut replacement) => {
                if let Some((start, lead)) = blank {
                    replacement.start = start;
                    replacement.text.insert_str(0, &lead);
                }
                out.push(replacement);
            }

            // Comment text unchanged, but a genuinely multi-line comment may
            // still need a blank line above it. Prepend it to the verbatim
            // comment bytes so nothing else about the comment shifts.
            None => {
                if let Some((start, lead)) = blank {
                    out.push(rewrite::Replacement {
                        start,
                        end: c.end_byte,
                        text: format!("{lead}{}", &source[c.start_byte..c.end_byte]),
                    });
                }
            }
        }
    }
    rewrite::validate(&out, source, &comments)?;
    Ok(out)
}
