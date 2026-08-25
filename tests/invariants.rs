// Comments here quote identifiers with "double quotes" rather than markdown
// code spans, matching the crate convention, so clippy's pedantic doc_markdown
// lint cannot be satisfied without reverting it.
#![allow(clippy::doc_markdown)]

// Correctness invariants: the contract this tool ships on. Every test in this
// file pins one documented behavior: style preservation, canonical block
// layout, the eight-spelling Return rule, preformatted pass-through (fences,
// art, tables, blockquotes, reference links, setext + ATX headers), Unicode
// width handling, line endings, replacement-range validation, and .clang-format
// discovery precedence.
//
// All tests use the in-process pipeline helper. Binary-level concerns (--check
// / --diff mtime, real .clang-format walks) get their own dedicated tests
// further down.

mod common;

use commentflow::parse::{Language, extract_comments};
use common::{detect, pipeline};
use std::path::PathBuf;

// style_preserved

#[test]
fn style_preserved_double_slash_stays_double_slash() {
    let src = "// a long enough comment that wraps when the column limit is short to force a rewrite\nint x = 0;\n";
    let out = pipeline(src, detect("foo.c"), 40);
    assert!(out.lines().all(|l| {
        let t = l.trim_start();
        t.is_empty() || !t.starts_with("/*") || t.contains("/*")
    }));
    for line in out.lines() {
        let t = line.trim_start();
        if t.starts_with('/') {
            assert!(
                t.starts_with("//"),
                "// must not become /* */, got: {line:?}"
            );
        }
    }
}

#[test]
fn style_preserved_block_stays_block() {
    let src = "/* a long enough block comment that will reflow when the column limit is short forcing splits */\nint x = 0;\n";
    let out = pipeline(src, detect("foo.c"), 40);
    assert!(!out.contains("//"), "/* */ must not become //, got:\n{out}");
    assert!(out.contains("/*"));
}

#[test]
fn style_preserved_outer_doc_line_stays_outer() {
    let src = "/// a long enough outer doc comment that will be reflowed when the column limit gets short enough\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 40);
    for l in out
        .lines()
        .filter(|l| l.contains("///") || l.contains("//!"))
    {
        assert!(
            l.trim_start().starts_with("///"),
            "outer doc must stay ///, got: {l:?}"
        );
    }
}

#[test]
fn style_preserved_inner_doc_line_stays_inner() {
    let src = "//! a long enough inner doc comment that will reflow once the column limit drops far enough below\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 40);
    for l in out.lines().filter(|l| l.trim_start().starts_with("//")) {
        assert!(
            l.trim_start().starts_with("//!"),
            "inner doc must stay //!, got: {l:?}"
        );
    }
}

#[test]
fn style_preserved_doxygen_block_stays_doxygen() {
    let src = "/** a long enough doxygen block doc comment that will be reflowed when the column limit is short */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 40);
    let first = out.lines().next().unwrap();
    assert!(
        first.starts_with("/**"),
        "/** opener must survive, got: {first:?}"
    );
}

#[test]
fn style_preserved_bang_block_stays_bang() {
    let src = "/*! a long enough bang block doc comment that will reflow at a short column limit boundary value */\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 40);
    let first = out.lines().next().unwrap();
    assert!(
        first.starts_with("/*!"),
        "/*! opener must survive, got: {first:?}"
    );
}

#[test]
fn style_no_conversion_line_to_block_or_block_to_line() {
    let src_a = "// just a normal line\n";
    let out_a = pipeline(src_a, detect("foo.c"), 80);
    assert_eq!(src_a, out_a);

    let src_b = "/* just a normal block */\n";
    let out_b = pipeline(src_b, detect("foo.c"), 80);
    assert_eq!(src_b, out_b);
}

// block_canonical_file_header

#[test]
fn block_canonical_file_header_already_matches_round_trips() {
    let src = "/*\n * This file header is already in canonical opener-alone form.\n */\nint main(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(src, out, "canonical file header must round-trip identical");
}

#[test]
fn block_canonical_bsd_file_header_round_trips() {
    let src = "/*-\n * SPDX-License-Identifier: BSD-3-Clause\n */\nint main(void) { return 0; }\n";
    let out = pipeline(src, detect("queue.h"), 80);
    assert_eq!(
        src, out,
        "BSD /*- file header must not gain an extra opener"
    );
}

#[test]
fn block_canonical_inline_spdx_file_header_does_not_leak_opener() {
    let src = "/* SPDX-License-Identifier: Apache-2.0\n * TLS accessors.\n */\nint main(void) { return 0; }\n";
    let out = pipeline(src, detect("tls.c"), 80);
    assert!(
        !out.contains("* /* SPDX"),
        "inline SPDX file header must not replay the raw opener:\n{out}"
    );
}

#[test]
fn block_canonical_file_header_bom_qualifies() {
    let bom = "\u{FEFF}";
    let src = format!(
        "{bom}/*\n * File header after a UTF-8 BOM still qualifies for the file-header form.\n */\nint main(void) {{ return 0; }}\n"
    );
    let out = pipeline(&src, detect("foo.c"), 80);
    assert_eq!(src, out, "BOM + canonical header must round-trip");
}

#[test]
fn block_canonical_inline_opener_at_file_start_becomes_file_header() {
    // Position-based canonical form: a block at file offset 0 uses opener-alone
    // form even if the author wrote inline. The opener becomes "/*" on its own
    // line and the first word moves to a " * " continuation.
    let src = "/* This inline-opener block sits at file offset 0 but the author chose inline form. */\nint main(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.starts_with("/*\n"),
        "offset-0 block must use opener-alone form, got:\n{out}"
    );
}

// block_canonical_interior_prefix_preserved

#[test]
fn block_canonical_interior_prefix_double_star_normalized() {
    // Strict single-star block continuations: a block whose source body lines
    // use "**" is normalized to canonical " * ". The "**" variant is no longer
    // preserved (reversed from the original spec).
    let src = "/*\n ** first body line that is long enough to keep the formatter from collapsing\n ** second body line which is similar in length to keep the layout meaningful\n */\nint main(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        !out.lines().any(|l| l.trim_start().starts_with("**")),
        "no body line may keep the ** marker, got:\n{out}"
    );
    let star_lines = out
        .lines()
        .filter(|l| l.trim_start().starts_with("* "))
        .count();
    assert!(
        star_lines >= 2,
        "body lines must use the canonical * marker, got:\n{out}"
    );
}

#[test]
fn block_double_star_normalized_with_art_block_preserved() {
    // A "**" block mixing prose and an aligned art/table block: prose reflows
    // to the column limit, "**" collapses to canonical " * " on every line
    // (prose AND the art lines), and the art content after the marker survives
    // byte-for-byte.
    let src = "/*\n ** Each log event contains an opcode field followed by a time field in seconds and a count and severity and msec field. Data follows... Count is in 32 bit words.\n **\n **          cccccccc   mmmmmm mmmmtsss   [c]ount,[m]sec,[t]ext,[s]everity\n ** mmmmmmmm mmmmMMMM MMMMMMMM MMMMMMMM [m]inor,[M]ajor SSSSSSSS SSSSSSSS\n ** SSSSSSSS SSSSSSSS [S]econds\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        !out.lines().any(|l| l.trim_start().starts_with("**")),
        "no line may keep the ** marker, got:\n{out}"
    );
    // Art rows survive with their alignment intact (content after the marker).
    assert!(
        out.contains(" *          cccccccc   mmmmmm mmmmtsss   [c]ount,[m]sec,[t]ext,[s]everity"),
        "aligned art row must be preserved under the * marker, got:\n{out}"
    );
    // Prose wrapped to the column limit.
    assert!(
        out.lines()
            .filter(|l| l.contains("Each log event") || l.contains("count and severity"))
            .count()
            >= 2,
        "prose must wrap to the column limit, got:\n{out}"
    );
}

#[test]
fn block_double_star_mid_file_fuses_opener() {
    // The same "**" block placed mid-file (not at offset 0): the opener-alone
    // "/*" fuses with the first word into inline form "/* Each ...", "**"
    // collapses to "*", and the art block survives. This is the exact shape
    // from the feature request.
    let src = "int before;\n/*\n ** Each log event contains an opcode field followed by a time field in seconds and a count and severity and msec field. Data follows... Count is in 32 bit words.\n **\n **          cccccccc   mmmmmm mmmmtsss   [c]ount,[m]sec,[t]ext,[s]everity\n ** SSSSSSSS SSSSSSSS [S]econds\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("/* Each log event"),
        "mid-file opener must fuse to inline form, got:\n{out}"
    );
    assert!(
        !out.lines().any(|l| l.trim_start().starts_with("**")),
        "no line may keep the ** marker, got:\n{out}"
    );
    assert!(
        out.contains(" *          cccccccc   mmmmmm mmmmtsss   [c]ount,[m]sec,[t]ext,[s]everity"),
        "art row must survive under the * marker, got:\n{out}"
    );
}

#[test]
fn block_double_star_bold_and_banner_preserved() {
    // Check that markdown bold spans like **bold** and ASCII star-banners like
    // ********************** are not corrupted/collapsed inside block comments.
    let src = "/*\n ** This is **bold** text inside a double-star comment.\n ** **********************\n ** *   Decorative   *\n ** **********************\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("**bold**"),
        "must preserve **bold** markdown, got:\n{out}"
    );
    assert!(
        out.contains(" **********************"),
        "must preserve ********************** banner, got:\n{out}"
    );
}

#[test]
fn block_is_table_row_no_false_positives() {
    // Check that bullet lists starting with - or + followed by multiple spaces,
    // and prose starting with s (no definition keywords) are correctly reflowed
    // rather than being pinned as preformatted TableRows.
    let src = "/*\n * -  First bullet point in our comment which has a couple of spaces and is very long so it should wrap.\n * -  Second bullet point which is also very long and should wrap to the column limit.\n * s  stands for seconds but since this is ordinary prose and does not contain any of our legend markers it should also wrap.\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 50);
    // Verified that wrapping occurred:
    assert!(
        out.lines().count() > 6,
        "prose and bullet points must wrap, got:\n{out}"
    );
}

// indentation_preserved

#[test]
fn indentation_preserved_tabs_stay_tabs() {
    let src = "void f(void) {\n\t// this comment is preceded by a real tab character not spaces and must remain that way\n\treturn;\n}\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.lines().any(|l| l.starts_with('\t') && l.contains("//")),
        "tab-indented comment must keep tab indentation, got:\n{out}"
    );
}

#[test]
fn indentation_preserved_byte_identical_first_line() {
    let src = "        // an indented comment whose exact eight-space leading indentation must be preserved\nint x = 0;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let first = out.lines().next().unwrap();
    assert!(
        first.starts_with("        //"),
        "eight-space indent must survive, got: {first:?}"
    );
}

// return_on_own_line: all 8 spellings

fn assert_token_at_prefix_column(out: &str, token: &str, prefix_pat: &str) {
    let line = out
        .lines()
        .find(|l| l.trim_start().contains(token))
        .unwrap_or_else(|| panic!("expected line containing {token:?} in:\n{out}"));
    let trimmed = line.trim_start();
    assert!(
        trimmed.starts_with(prefix_pat),
        "{token} must sit at first non-prefix column under {prefix_pat:?}, got: {line:?}"
    );
}

// A converted C/C++ function comment must carry the kernel-doc "Return ..."
// line (blank-separated, own line) and no surviving Doxygen return tag.
fn assert_kernel_doc_return(out: &str) {
    assert_token_at_prefix_column(out, "Return the result", "* Return the result");
    assert!(
        !out.contains("@return") && !out.contains("\\return"),
        "Doxygen return tag must be converted to kernel-doc, got:\n{out}"
    );
}

#[test]
fn return_on_own_line_prose_capital() {
    let src = "#include <stdio.h>\n/* Does something useful with the inputs supplied. Returns the result on success. */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_token_at_prefix_column(&out, "Returns", "* Returns");
}

#[test]
fn return_on_own_line_doxygen_at_return() {
    // C/C++ Doxygen return tags convert to kernel-doc "Return ..." on their own
    // line, blank-separated. All four spellings (@return, @returns, \return, \returns)
    // converge to the same output.
    let src = "#include <stdio.h>\n/**\n * Computes a thing.\n * @return the result of the computation\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_kernel_doc_return(&out);
}

#[test]
fn return_on_own_line_doxygen_at_returns() {
    let src = "#include <stdio.h>\n/**\n * Computes a thing.\n * @returns the result of the computation\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_kernel_doc_return(&out);
}

#[test]
fn return_on_own_line_doxygen_backslash_return() {
    let src = "#include <stdio.h>\n/**\n * Computes a thing.\n * \\return the result of the computation\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_kernel_doc_return(&out);
}

#[test]
fn return_on_own_line_doxygen_backslash_returns() {
    let src = "#include <stdio.h>\n/**\n * Computes a thing.\n * \\returns the result of the computation\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_kernel_doc_return(&out);
}

#[test]
fn return_on_own_line_rustdoc_hash_returns() {
    let src = "/// Computes a value.\n///\n/// # Returns\n///\n/// The computed value.\nfn f() -> i32 { 0 }\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert!(
        out.contains("/// # Returns"),
        "# Returns header must be preserved exactly, got:\n{out}"
    );
}

#[test]
fn return_on_own_line_rustdoc_hash_return() {
    let src = "/// Computes a value.\n///\n/// # Return\n///\n/// The computed value.\nfn f() -> i32 { 0 }\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert!(
        out.contains("/// # Return"),
        "# Return header must be preserved exactly, got:\n{out}"
    );
}

#[test]
fn return_on_own_line_prose_return_singular() {
    let src = "#include <stdio.h>\n/* Does something useful with the inputs supplied. Return the result on success. */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_token_at_prefix_column(&out, "Return", "* Return");
}

// return_case_sensitivity (lowercase doesn't trigger)

#[test]
fn return_case_sensitivity_lowercase_returns_prose() {
    let src = "#include <stdio.h>\n/* The function calls foo and then returns to the caller as quickly as possible. */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        !out.contains("\n * returns"),
        "lowercase 'returns' must NOT trigger Return rule, got:\n{out}"
    );
}

#[test]
fn return_case_sensitivity_lowercase_rust_doc_header() {
    // "# returns" with lowercase r is still a valid ATX header: header
    // detection is by "#" + space + text. The Return rule does NOT fire on
    // lowercase, so the header text survives exactly as written.
    let src =
        "/// short description here for context.\n/// # returns lowercase header form\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert!(
        out.contains("/// # returns lowercase header form"),
        "lowercase '# returns' header must be preserved verbatim, got:\n{out}"
    );
}

// return_trailing_punctuation

#[test]
fn return_trailing_punctuation_colon() {
    let src = "#include <stdio.h>\n/* Does something. Returns: the value on success. */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("Returns:"),
        "trailing colon must be preserved, got:\n{out}"
    );
}

#[test]
fn return_trailing_punctuation_period() {
    // "Returns. Done.": the first sentence ends in "Returns" with a trailing
    // period; the period must survive on output and the Returns token must
    // still sit at the first non-prefix column.
    let src = "#include <stdio.h>\n/* Does something for some reason explained at length. Returns. Done with the operation now. */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("Returns."),
        "trailing period must be preserved on Returns, got:\n{out}"
    );
    let returns_line = out
        .lines()
        .find(|l| l.contains("Returns"))
        .expect("expected Returns line");
    assert!(
        returns_line.trim_start().starts_with("* Returns"),
        "Returns must split into its own paragraph even with trailing period, got: {returns_line:?}"
    );
}

// return_not_split_from_neighbor_tag

#[test]
fn return_not_split_from_neighbor_tag() {
    // A "@param" tag followed by a prose "Returns ..." line converts to a
    // kernel-doc param plus a separate "Return ..." line: the prose return is
    // never glued onto the param description.
    let src = "#include <stdio.h>\n/**\n * @param x first parameter\n * Returns the sum on success.\n */\nint f(int x) { return x; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("* @x : first parameter"),
        "param must convert to kernel-doc, got:\n{out}"
    );
    assert_token_at_prefix_column(&out, "Return the sum", "* Return the sum");
    assert!(
        !out.contains("@param") && !out.contains("Returns"),
        "Doxygen param and prose Returns must be converted, got:\n{out}"
    );
}

// at_tag_identifier_rules

#[test]
fn at_tag_identifier_rule_email_not_a_tag() {
    let src = "// contact me at foo@example.com for any details about the configuration options\nint x = 0;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(src, out, "email must round-trip unchanged, got:\n{out}");
}

#[test]
fn at_tag_identifier_rule_handle_not_a_tag() {
    let src =
        "// see @user/repo for upstream commits affecting this comment reflow tool\nint x = 0;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(src, out, "handle must round-trip unchanged, got:\n{out}");
}

#[test]
fn at_param_converts_and_wraps_within_limit() {
    // A "@param name desc" long enough to wrap converts to kernel-doc "@name :
    // desc" and its continuation wraps within the column limit.
    let src = "#include <stdio.h>\n/**\n * @param widget the operand for the operation that needs to be wide enough to wrap onto another line\n */\nint f(int widget) { return widget; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("* @widget : the operand") && !out.contains("@param"),
        "@param must convert to kernel-doc @name form, got:\n{out}"
    );
    assert!(
        out.lines()
            .filter(|l| l.trim_start().starts_with('*'))
            .all(|l| l.chars().count() <= 80),
        "converted param must wrap within the column limit, got:\n{out}"
    );
}

#[test]
fn doxygen_alignment_preserved_for_unconverted_tag() {
    // A comment carrying a tag we don't convert (@throws) bails from kernel-doc
    // conversion, so the Doxygen description-column alignment path stays live
    // and its continuation must remain indented past the tag+name, not flush.
    let src = "#include <stdio.h>\n/**\n * @throws SomeException when the operand exceeds the maximum width allowed for one single line here\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("@throws SomeException"),
        "unconverted Doxygen tag must survive verbatim, got:\n{out}"
    );
    let cont = out
        .lines()
        .find(|l| !l.contains("@throws") && l.trim_start().starts_with('*') && !l.contains("*/"))
        .expect("expected a wrapped continuation line");
    let after_star = cont.trim_start().strip_prefix("* ").unwrap_or("");
    assert!(
        after_star.starts_with("  "),
        "Doxygen tag continuation must stay description-column aligned, got: {cont:?}"
    );
}

// mid_paragraph_split_six_spellings_only

#[test]
fn mid_paragraph_split_prose_returns() {
    // Force reflow by making the comment exceed the column limit so the
    // single-line fast-path doesn't skip normalization.
    let src = "#include <stdio.h>\n/* Does the thing with the inputs supplied to it. Returns the value on success or an error code on failure. */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let returns_line = out
        .lines()
        .find(|l| l.contains("Returns"))
        .expect("expected Returns line");
    assert!(
        returns_line.trim_start().starts_with("* Returns"),
        "mid-paragraph 'Returns' must split, got: {returns_line:?}"
    );
}

#[test]
fn mid_paragraph_split_prose_return_singular() {
    let src = "#include <stdio.h>\n/* Does the thing with the inputs supplied to it. Return the value on success or an error code on failure. */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let line = out
        .lines()
        .find(|l| l.contains("Return"))
        .expect("expected Return line");
    assert!(
        line.trim_start().starts_with("* Return"),
        "mid-paragraph 'Return' must split, got: {line:?}"
    );
}

/// A mid-line Doxygen return tag splits onto its own paragraph AND converts to
/// kernel-doc form (Scope transform 2) in the same run.
///
/// These four used to assert that the tag spelling survived the split, which
/// pinned a pass-1-only state: the split hoists the tag to a line start, which
/// is exactly where the converter fires, so the next run changed the file
/// again. The split now happens before the conversion instead of after it.
fn check_doxygen_return_split(tag: &str) {
    let src = format!(
        "#include <stdio.h>\n/* Does the thing with the inputs supplied to it. {tag} the value on success or an error code on failure. */\nint f(void) {{ return 0; }}\n"
    );
    let out = pipeline(&src, detect("foo.c"), 80);
    assert!(
        out.contains("\n *\n * Return the value on success or an error code on failure.\n"),
        "mid-paragraph {tag:?} must split into its own kernel-doc Return paragraph, got:\n{out}"
    );
    assert!(
        !out.contains(tag),
        "{tag:?} must be converted, not preserved, got:\n{out}"
    );
}

#[test]
fn mid_paragraph_split_doxygen_at_return() {
    check_doxygen_return_split("@return");
}

#[test]
fn mid_paragraph_split_doxygen_at_returns() {
    check_doxygen_return_split("@returns");
}

#[test]
fn mid_paragraph_split_doxygen_backslash_return() {
    check_doxygen_return_split("\\return");
}

#[test]
fn mid_paragraph_split_doxygen_backslash_returns() {
    check_doxygen_return_split("\\returns");
}

#[test]
fn mid_paragraph_split_rustdoc_hash_returns_not_split() {
    // ATX-header forms are out of scope for the mid-paragraph split. A mid-line
    // "# Returns" sequence in source prose is just text.
    let src = "/// Does the thing. # Returns the value on success.\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    let lines: Vec<&str> = out.lines().collect();
    // # Returns should NOT appear on its own line; it sits mid-paragraph.
    let on_own_line = lines
        .iter()
        .any(|l| l.trim_start().starts_with("/// # Returns"));
    assert!(
        !on_own_line,
        "mid-line '# Returns' must NOT be promoted to a header, got:\n{out}"
    );
}

// kernel_doc_long_param

#[test]
fn kernel_doc_long_param_wraps_idempotent() {
    // A long "@param name desc" at a tight ColumnLimit converts to kernel-doc
    // and its description wraps within the limit without corrupting the name
    // line, and is stable across two runs.
    let src = "#include <stdio.h>\n/**\n * @param very_long_identifier_name description with several words that wrap reasonably onto further lines\n */\nint f(int very_long_identifier_name) { return very_long_identifier_name; }\n";
    let out = pipeline(src, detect("foo.c"), 50);
    assert!(
        out.contains("* @very_long_identifier_name :") && !out.contains("@param"),
        "long param must convert to kernel-doc, got:\n{out}"
    );
    assert!(
        out.lines()
            .filter(|l| l.trim_start().starts_with('*'))
            .all(|l| l.chars().count() <= 50),
        "converted param must wrap within ColumnLimit 50, got:\n{out}"
    );
}

// markdown_list_boundary

#[test]
fn markdown_list_boundary_dash_list_not_collapsed() {
    let src = "/*\n * Intro paragraph with several words to set context for the list following.\n * - first item\n * - second item\n * - third item\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let dash_lines = out
        .lines()
        .filter(|l| l.trim_start().starts_with("* -"))
        .count();
    assert!(
        dash_lines >= 3,
        "dash list items must stay on separate lines, got:\n{out}"
    );
}

#[test]
fn markdown_list_boundary_numbered_list_not_collapsed() {
    let src = "/*\n * Intro paragraph here with enough words to be a real paragraph in the comment.\n * 1. first item\n * 2. second item\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("1.") && out.contains("2."),
        "numbered list items must survive, got:\n{out}"
    );
}

// preformatted_borderline

#[test]
fn preformatted_borderline_operator_prose_reflows() {
    // "when a > b && c < d, return early" must reflow normally; it is not ASCII
    // art (the word-length guard fires on "when", "early", "return").
    let src = "/* when a is greater than b and the condition c is less than d then we return early before the work and avoid wasting cycles */\nint f(int a, int b, int c, int d) { (void)a; (void)b; (void)c; (void)d; return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    // The output should have been reflowed (wrapped onto multiple lines).
    let comment_lines = out
        .lines()
        .filter(|l| l.contains("greater") || l.contains("less") || l.contains("early"))
        .count();
    assert!(
        comment_lines >= 2,
        "operator-heavy prose must reflow, not pass through, got:\n{out}"
    );
}

#[test]
fn preformatted_borderline_pipe_table_passes_through() {
    let src = "/*\n * | col1 | col2 | col3 |\n * | val1 | val2 | val3 |\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(out.contains("| col1 | col2 |"), "table row must survive");
}

#[test]
fn preformatted_tab_aligned_table_passes_through() {
    let src = "/*\n *  +  means available\n *  -  means unavailable\n *  s  means slow\n *\n *\t\tSLIST\tLIST\tSTAILQ\tTAILQ\n * _HEAD\t+\t+\t+\t+\n * _PREV\t-\t+\t-\t+\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(out, src, "tab-aligned table must pass through");
}

#[test]
fn preformatted_borderline_fenced_code_passes_through() {
    let src = "/// Examples.\n///\n/// ```\n/// let x = 1;\n/// ```\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert_eq!(src, out, "fenced code must pass through, got:\n{out}");
}

// preformatted_fence_state

#[test]
fn preformatted_fence_state_prose_fence_prose() {
    // A Prose paragraph followed by a fenced code block followed by more prose
    // produces three separate paragraphs: the prose halves are reflowed
    // independently and the fence content passes through.
    let src = "/// Intro prose with enough words to count as a real paragraph in the test corpus.\n///\n/// ```\n/// let x = 1;\n/// let y = 2;\n/// ```\n///\n/// Outro prose with enough words to count as a second real paragraph here also.\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    // Fence content must be byte-identical to source.
    assert!(out.contains("/// let x = 1;\n"));
    assert!(out.contains("/// let y = 2;\n"));
    assert!(out.contains("/// ```"));
}

// fence_variants

#[test]
fn fence_variants_backtick_three_and_four() {
    let src_three = "/// before.\n///\n/// ```\n/// code\n/// ```\nfn f() {}\n";
    assert_eq!(pipeline(src_three, detect("foo.rs"), 80), src_three);
    let src_four = "/// before.\n///\n/// ````\n/// code with ``` backticks\n/// ````\nfn f() {}\n";
    assert_eq!(pipeline(src_four, detect("foo.rs"), 80), src_four);
}

#[test]
fn fence_variants_tilde_three_and_four() {
    let src_three = "/// before.\n///\n/// ~~~\n/// code\n/// ~~~\nfn f() {}\n";
    assert_eq!(pipeline(src_three, detect("foo.rs"), 80), src_three);
    let src_four = "/// before.\n///\n/// ~~~~\n/// code\n/// ~~~~\nfn f() {}\n";
    assert_eq!(pipeline(src_four, detect("foo.rs"), 80), src_four);
}

#[test]
fn fence_variants_info_strings_preserved() {
    let src = "/// before.\n///\n/// ```rust,no_run\n/// let x = 1;\n/// ```\nfn f() {}\n";
    assert_eq!(pipeline(src, detect("foo.rs"), 80), src);
}

#[test]
fn fence_variants_mismatched_marker_does_not_close() {
    // Backtick opener with tilde "closer": the tilde line is NOT a closer, so
    // the fence remains open through end-of-comment and absorbs everything as
    // FenceContent. The "~~~" line and "still content" line both must pass
    // through verbatim (not be reflowed or merged into surrounding prose).
    let src = "/// before.\n///\n/// ```\n/// content\n/// ~~~\n/// still content\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert!(
        out.contains("/// ~~~\n"),
        "tilde line inside open backtick fence must survive verbatim, got:\n{out}"
    );
    assert!(
        out.contains("/// still content"),
        "post-tilde content must remain inside the fence, got:\n{out}"
    );
}

#[test]
fn block_comment_unclosed_tilde_fence_does_not_duplicate_closer() {
    // ASCII art line starts with "~~~" and contains no matching pure-"~~~"
    // closer, so fence_marker_run picks it up as a tilde fence that runs to
    // end-of-comment. The fence's preformatted paragraph then captures the
    // block-comment closer line "*/" as raw content. Before the fix, reflow
    // appended a *second* "*/" after the body, so each pass duplicated the
    // closer and corrupted the file's trailing whitespace.
    let src = "int main(void) {\n    /*\n       Art:\n       ~~~------+-+-+<--+  +->+-+-+--------------------+      +-+-+---------~~~\n       |     |  +------------------------------------+\n       ~~~--------+     +-------+\n     */\n    return 0;\n}\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(
        out.matches("*/").count(),
        1,
        "comment closer must appear exactly once, got:\n{out}"
    );
}

// doctest_hidden_lines

#[test]
fn doctest_hidden_lines_not_treated_as_headers() {
    let src = "/// Examples.\n///\n/// ```rust\n/// # use foo::bar;\n/// # let x = 1;\n/// bar(x);\n/// ```\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert_eq!(src, out, "doctest-hidden lines must survive, got:\n{out}");
}

// reference_style_links

#[test]
fn reference_style_links_preserved() {
    let src = "/// See [foo] for context on this code.\n///\n/// [foo]: https://example.com/very/long/url/that/must/not/wrap\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert!(
        out.contains("[foo]: https://example.com/very/long/url/that/must/not/wrap"),
        "reference link must not wrap, got:\n{out}"
    );
}

// setext_headers

#[test]
fn setext_headers_eq_underline_preserved() {
    let src = "/// Title\n/// =====\n///\n/// Body paragraph below the setext title.\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert!(out.contains("=====") || out.contains("/// ====="));
}

#[test]
fn setext_headers_dash_underline_preserved() {
    let src = "/// Subtitle\n/// --------\n///\n/// Body paragraph below the setext subtitle.\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert!(out.contains("--------"));
}

// atx_header_edge_cases

#[test]
fn atx_header_edge_case_hash_no_space_is_prose() {
    // "#Returns" (no space) is NOT a header; it's prose. The Return rule also
    // must NOT fire because "#" is part of the prose token.
    let src = "/// short intro line.\n/// #Returns is just prose content here that should not be promoted.\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert!(
        out.contains("#Returns is just prose"),
        "#Returns must stay as prose, not be promoted to header, got:\n{out}"
    );

    // A real ATX-header line would have been split out and preceded by a blank
    // "///" line; verify that did NOT happen.
    let header_emitted = out
        .lines()
        .any(|l| l.trim() == "/// # Returns" || l.trim() == "///# Returns");
    assert!(
        !header_emitted,
        "no ATX header should be emitted, got:\n{out}"
    );
}

#[test]
fn atx_header_edge_case_multi_level_all_headers() {
    for level in 1..=6 {
        let prefix = "#".repeat(level);
        let src = format!("/// Intro.\n///\n/// {prefix} Returns\n///\n/// Body.\nfn f() {{}}\n");
        let out = pipeline(&src, detect("foo.rs"), 80);
        let header_match = format!("/// {prefix} Returns");
        assert!(
            out.contains(&header_match),
            "level-{level} ATX Returns header must be preserved, got:\n{out}"
        );
    }
}

// atx_header_language_independent

#[test]
fn atx_header_language_independent_in_doxygen_block() {
    // ATX header inside a Doxygen DocBlock should still act as a paragraph
    // boundary: the header line is preserved verbatim and not merged into the
    // surrounding prose.
    let src = "#include <stdio.h>\n/**\n * Intro paragraph.\n *\n * # Section\n *\n * Body.\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains(" * # Section"),
        "ATX header in Doxygen DocBlock must be preserved, got:\n{out}"
    );

    // The "# Section" line must be on a line of its own, not glued to "Intro
    // paragraph." or "Body.".
    let mixed = out
        .lines()
        .any(|l| l.contains("# Section") && (l.contains("Intro") || l.contains("Body")));
    assert!(!mixed, "# Section must not merge into prose, got:\n{out}");
}

#[test]
fn atx_header_language_independent_rust_no_doxygen_align() {
    // A Rust DocBlock with "@param" should NOT get Doxygen description-column
    // alignment: @param is just prose under Rustdoc flavor. If it wrapped with
    // description-column indent, continuation lines would be deeply indented
    // past the "@param widget" header. Make the description long enough to wrap
    // and verify the continuation sits flush-left under the " * " prefix, not
    // column-aligned under the tag.
    let src = "/**\n * @param widget the widget that needs careful operation in any state including edge cases here that wrap\n */\nfn f(widget: i32) -> i32 { widget }\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    let cont = out
        .lines()
        .find(|l| !l.contains("@param") && l.trim_start().starts_with("* ") && !l.contains("*/"))
        .expect("expected a wrapped continuation line");
    let after_star = cont.trim_start().strip_prefix("* ").unwrap_or("");

    // Under Rustdoc, the continuation must NOT start with multiple spaces
    // (which would indicate Doxygen description-column alignment kicked in).
    assert!(
        !after_star.starts_with("  "),
        "@param must NOT get description-column alignment under Rustdoc flavor, got: {cont:?}"
    );
}

// blockquote_passthrough

#[test]
fn blockquote_passthrough_simple() {
    let src = "/// Intro.\n///\n/// > quoted line one\n/// > quoted line two\n///\n/// Outro.\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert!(out.contains("/// > quoted line one"));
    assert!(out.contains("/// > quoted line two"));
}

// unicode_art_passthrough

#[test]
fn unicode_art_box_drawing_passthrough() {
    let src = "/*\n * ┌─────┐\n * │ box │\n * └─────┘\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(out.contains("┌─────┐"));
    assert!(out.contains("│ box │"));
    assert!(out.contains("└─────┘"));
}

#[test]
fn unicode_art_block_elements_passthrough() {
    let src = "/*\n * █ ▓ ▒ ░\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(out.contains("█ ▓ ▒ ░"));
}

#[test]
fn unicode_art_arrows_passthrough() {
    let src = "/*\n * ← ↑ → ↓ ⇒\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(out.contains("← ↑ → ↓ ⇒"));
}

#[test]
fn unicode_art_mixed_ascii_unicode_passthrough() {
    // A box drawn with "|" sides and "─" tops still passes through.
    let src = "/*\n * |───|\n * |   |\n * |───|\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(out.contains(" * |───|"));
    assert!(out.contains(" * |   |"));
}

// unicode_art_negative

#[test]
fn unicode_art_negative_single_arrow_in_prose() {
    // Single arrow in prose should NOT classify as art: the density threshold
    // protects against it. The word-length guard also protects ("symbol",
    // "means", "yields" all qualify).
    let src = "// the symbol → means yields in the formal grammar that we use here\nint x = 0;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(
        src, out,
        "single arrow in prose must not be classified as art, got:\n{out}"
    );
}

// unicode_art_math_negative

#[test]
fn unicode_art_math_negative_single_symbols_in_prose() {
    let src =
        "// when A means B and the consequent C means D then we get the property\nint x = 0;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(
        src, out,
        "prose with no art chars must not reflow, got:\n{out}"
    );
}

#[test]
fn unicode_art_math_negative_check_mark_in_prose() {
    let src =
        "// see the check mark for items already covered in the design document\nint x = 0;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(
        src, out,
        "check-mark prose must not classify as art, got:\n{out}"
    );
}

// line_ending_split_lines

/// Interior line breaks of the rewritten comment span only. "head_marker"
/// is the first byte sequence of the trailing code line, used to cut the
/// comment off the rest of the output so out-of-span CRLFs never inflate the
/// count (the false-pass this assertion exists to avoid).
fn comment_interior_breaks(out: &str, head_marker: &str) -> (usize, usize) {
    let comment = out.split(head_marker).next().unwrap();

    // Drop the single separator between the comment and the code line: it is
    // out-of-span and would let a comment that never wrapped false-pass the
    // "crlf >= 1" check.
    let comment = comment
        .strip_suffix("\r\n")
        .or_else(|| comment.strip_suffix('\n'))
        .unwrap_or(comment);
    let crlf = comment.matches("\r\n").count();
    let lf = comment.matches('\n').count();
    (crlf, lf)
}

#[test]
fn line_ending_split_lines_uses_dominant_crlf() {
    // CRLF-dominant source: when reflow splits a single comment into several
    // lines, the new intermediate lines must use CRLF.
    let src = "/* this is a long block comment that should definitely wrap onto multiple lines when the column limit is short enough to force the wrap to happen reliably */\r\nint f(void) { return 0; }\r\n";
    let out = pipeline(src, detect("foo.c"), 60);

    // Count CRLF on the comment's interior lines ONLY; counting the whole
    // output false-passes on the untouched out-of-span code-line CRLFs even
    // when the interior breaks reflow emits are LF.
    let (crlf, lf) = comment_interior_breaks(&out, "int f");
    assert!(
        crlf >= 2,
        "comment must wrap into CRLF lines, got:\n{out:?}"
    );
    assert_eq!(
        lf, crlf,
        "every interior break in the comment must be CRLF, got:\n{out:?}"
    );
}

// Task 1: a single-SOURCE-line comment that wraps must emit CRLF interior
// breaks, not LF: there is no interior line to vote on, so the ending falls
// back to the post-span byte. One fixture per marker style.

#[test]
fn line_ending_single_source_line_crlf_block_wraps_crlf() {
    let src = "/* one physical source line block comment that is long enough to wrap when the limit is short */\r\nint f(void) { return 0; }\r\n";
    let out = pipeline(src, detect("foo.c"), 50);
    let (crlf, lf) = comment_interior_breaks(&out, "int f");
    assert!(crlf >= 1, "block must wrap, got:\n{out:?}");
    assert_eq!(lf, crlf, "interior breaks must all be CRLF, got:\n{out:?}");
}

#[test]
fn line_ending_single_source_line_crlf_slash_wraps_crlf() {
    let src = "// one physical source line slash comment that is long enough to wrap when the limit is short\r\nint x = 0;\r\n";
    let out = pipeline(src, detect("foo.c"), 50);
    let (crlf, lf) = comment_interior_breaks(&out, "int x");
    assert!(crlf >= 1, "// must wrap, got:\n{out:?}");
    assert_eq!(lf, crlf, "interior breaks must all be CRLF, got:\n{out:?}");

    // The out-of-span code line must be byte-identical: only comment bytes may
    // change.
    assert!(
        out.ends_with("\r\nint x = 0;\r\n"),
        "non-comment bytes must be untouched, got:\n{out:?}"
    );
}

#[test]
fn line_ending_single_source_line_crlf_hash_wraps_crlf() {
    let src = "# one physical source line shell comment that is long enough to wrap when the limit is short\r\nx=0\r\n";
    let out = pipeline(src, detect("foo.sh"), 50);
    let (crlf, lf) = comment_interior_breaks(&out, "x=0");
    assert!(crlf >= 1, "# must wrap, got:\n{out:?}");
    assert_eq!(lf, crlf, "interior breaks must all be CRLF, got:\n{out:?}");
}

#[test]
fn line_ending_single_source_line_crlf_idempotent() {
    // Run-twice: a wrapped single-source-line CRLF comment must be stable; the
    // second pass sees genuine interior CRLF breaks and must not flip them. One
    // case per marker style: each takes a distinct path (// via last-line
    // had_crlf, /* */ via fallback_ending, # via the trailing-\r re-append).
    let cases = [
        (
            "// one physical source line slash comment that is long enough to wrap when the limit is short\r\nint x = 0;\r\n",
            "foo.c",
        ),
        (
            "/* one physical source line block comment that is long enough to wrap when the limit is short */\r\nint f(void) { return 0; }\r\n",
            "foo.c",
        ),
        (
            "# one physical source line shell comment that is long enough to wrap when the limit is short\r\nx=0\r\n",
            "foo.sh",
        ),
    ];
    for (src, path) in cases {
        // The convergence assertion inside the helper is the whole point here.
        pipeline(src, detect(path), 50);
    }
}

#[test]
fn line_ending_split_lines_uses_dominant_lf() {
    // Mostly LF, one CRLF: dominant is LF, so new lines use LF.
    let src = "/* this is a long block comment that should definitely wrap onto multiple lines when the column limit is short enough to force wrapping */\nint f(void) { return 0; }\nint g(void) { return 1; }\n";
    let out = pipeline(src, detect("foo.c"), 60);
    // Output must contain LF-terminated lines, not stray CRLF.
    assert!(
        !out.contains('\r'),
        "LF-dominant must not invent CRLF, got:\n{out:?}"
    );
}

#[test]
fn line_ending_no_trailing_newline_preserved() {
    // Source comment has no trailing newline at end of file. After reflow, the
    // final line must still have no trailing newline.
    let src = "/* this is a long block comment that wraps when the column limit is short forcing multiple lines */";
    let out = pipeline(src, detect("foo.c"), 60);
    assert!(
        !out.ends_with('\n'),
        "no trailing newline must be preserved, got:\n{out:?}"
    );
}

// trailing_whitespace_policy

#[test]
fn trailing_whitespace_policy_blank_comment_line_no_space() {
    let src = "/**\n * First paragraph here, long enough to be a real paragraph.\n *\n * Second paragraph here, also long enough to be a real paragraph.\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    // The blank-prefix line must be " *" with NO trailing space.
    for line in out.lines() {
        if line.trim_end() == " *" {
            assert_eq!(line, " *", "blank line must have no trailing whitespace");
        }
    }
}

#[test]
fn trailing_whitespace_policy_no_emitted_line_ends_in_space() {
    let src = "/** A long enough doxygen comment that will reflow and wrap to ensure we cover lots of emitted lines with no trailing spaces anywhere in the output */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 60);
    for line in out.lines() {
        // Preformatted regions can keep trailing space; this comment has none.
        assert!(
            !line.ends_with(' ') && !line.ends_with('\t'),
            "no emitted line may end in whitespace, got: {line:?}"
        );
    }
}

// rust_grammar_selection

#[test]
fn rust_grammar_selection_extensions() {
    use commentflow::parse::detect_language;
    assert_eq!(
        detect_language(&PathBuf::from("a.rs")).unwrap(),
        Language::Rust
    );
    for c_ext in &["c", "h"] {
        assert_eq!(
            detect_language(&PathBuf::from(format!("a.{c_ext}"))).unwrap(),
            Language::C
        );
    }
    for cpp_ext in &["cc", "cpp", "cxx", "c++", "hh", "hpp", "hxx", "h++"] {
        assert_eq!(
            detect_language(&PathBuf::from(format!("a.{cpp_ext}"))).unwrap(),
            Language::Cpp
        );
    }
    assert!(detect_language(&PathBuf::from("a.java")).is_err());
    assert!(detect_language(&PathBuf::from("a.py")).is_err());
}

#[test]
fn rust_grammar_handles_all_comment_kinds() {
    let src = "// plain line comment that is short\n/// outer doc comment that is short\n//! inner doc comment that is short\n/* block comment that is short */\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert_eq!(src, out);
}

// rust_doc_attribute_untouched

#[test]
fn rust_doc_attribute_string_literal_untouched() {
    // #[doc = "..."] is a string literal, not a comment, so it passes through.
    let src = "#[doc = \"a very long doc string literal that would normally exceed the column limit but it is a string not a comment\"]\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert_eq!(src, out);
}

// rustdoc_section_headers

#[test]
fn rustdoc_section_headers_all_six_preserved() {
    let src = "/// Computes a value.\n///\n/// # Arguments\n///\n/// * `x` - input\n///\n/// # Returns\n///\n/// The result.\n///\n/// # Errors\n///\n/// Returns an error when foo.\n///\n/// # Panics\n///\n/// Panics when bar.\n///\n/// # Safety\n///\n/// Safe to call.\n///\n/// # Examples\n///\n/// ```\n/// let y = compute(1);\n/// ```\nfn compute(x: i32) -> Result<i32, ()> { Ok(x) }\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert_eq!(src, out, "rustdoc full section structure must round-trip");
}

// rustdoc_doc_flavor_isolated

#[test]
fn rustdoc_doc_flavor_cpp_uses_doxygen() {
    // /// in a .cpp file is Doxygen: its @param converts to kernel-doc (C/C++
    // only). The .rs sibling below leaves @param as prose, so the two languages
    // diverge on the same source text.
    let src = "/// Computes a thing.\n/// @param widget the widget object that needs careful operation in any state including edge cases here\nint f(int widget) { return widget; }\n";
    let out = pipeline(src, detect("foo.cpp"), 80);
    assert!(
        out.contains("/// @widget : the widget object") && !out.contains("@param"),
        ".cpp @param must convert to kernel-doc, got:\n{out}"
    );
}

#[test]
fn rustdoc_doc_flavor_rs_treats_at_param_as_prose() {
    // Same "///" + "@param ..." text in a .rs file. Under Rustdoc
    // flavor, @param is just prose: continuation is flush-left under "/// ",
    // NOT description-column aligned.
    let src = "/// Computes a thing.\n/// @param widget the widget object that needs careful operation here in this rust file using prose semantics\nfn f(widget: i32) -> i32 { widget }\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    let cont = out
        .lines()
        .find(|l| l.starts_with("///") && !l.contains("@param") && !l.contains("Computes"))
        .expect("expected wrapped continuation");
    let after_prefix = cont.strip_prefix("/// ").unwrap_or(cont);
    assert!(
        !after_prefix.starts_with("  "),
        "Rustdoc flavor must NOT align @param as a Doxygen tag, got: {cont:?}"
    );
}

// rust_nested_block_force_passthrough

#[test]
fn rust_nested_block_force_passthrough_block() {
    let src = "fn f() {} /* outer /* inner */ outer */\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert_eq!(src, out);
}

#[test]
fn rust_nested_block_force_passthrough_docblock() {
    let src = "/** outer doc /* inner */ outer doc */\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert_eq!(src, out);
}

// rust_inner_doc_grouping

#[test]
fn rust_inner_doc_grouping_runs_merge() {
    let src = "//! one line of inner doc that needs to be merged with the following line\n//! second line of inner doc that joins into the same logical paragraph for reflow\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);

    // The two source lines should reflow into one packed line (≥75 chars)
    // because they merge as a single LineGroup.
    let max_inner_line_len = out
        .lines()
        .filter(|l| l.trim_start().starts_with("//!"))
        .map(str::len)
        .max()
        .unwrap_or(0);
    assert!(
        max_inner_line_len >= 75,
        "inner-doc lines must merge into a packed paragraph, got max line {max_inner_line_len}:\n{out}"
    );
}

#[test]
fn rust_inner_doc_grouping_outer_inner_do_not_merge() {
    let src = "/// outer doc line\n//! inner doc line\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert_eq!(src, out, "outer/inner must not merge, got:\n{out}");
}

// column_limit_discovery

/// Make a unique tmp directory UNDER the cargo target dir so resolved files
/// canonicalize as descendants of the project cwd. That puts discovery on
/// the "file under cwd" code path, which walks upward from the file
/// directory, needed so a test-owned ".clang-format" wins regardless of
/// any ".clang-format" sitting above the project dir.
fn fresh_tmp_under_cwd(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    // CARGO_TARGET_TMPDIR points at the package's target-tmp dir during test
    // runs. It's already under the project cwd.
    let base = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let dir = base.join(format!("commentflow-{tag}-{}-{nanos}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn column_limit_discovery_nested_clang_format() {
    use commentflow::config::resolve;
    use std::fs;
    let tmp = fresh_tmp_under_cwd("cl-nested");
    let nested = tmp.join("a/b/c");
    fs::create_dir_all(&nested).unwrap();
    fs::write(tmp.join(".clang-format"), "ColumnLimit: 80\n").unwrap();
    fs::write(tmp.join("a/.clang-format"), "ColumnLimit: 100\n").unwrap();
    let file = nested.join("src.c");
    fs::write(&file, "int x = 0;\n").unwrap();
    let n = resolve(&file, None).unwrap().column_limit;
    assert_eq!(n, 100, "nearest-ancestor wins");
    let n2 = resolve(&file, Some(50)).unwrap().column_limit;
    assert_eq!(n2, 50);
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn column_limit_discovery_ignores_other_keys() {
    use commentflow::config::resolve;
    use std::fs;
    let tmp = fresh_tmp_under_cwd("cl-keys");
    fs::write(
        tmp.join(".clang-format"),
        "Language: Cpp\nBasedOnStyle: Google\nColumnLimit: 90 # project default\nIndentWidth: 4\n",
    )
    .unwrap();
    let file = tmp.join("src.c");
    fs::write(&file, "int x = 0;\n").unwrap();
    let n = resolve(&file, None).unwrap().column_limit;
    assert_eq!(n, 90);
    let _ = fs::remove_dir_all(&tmp);
}

// clang_format_precedence_total_order

#[test]
fn clang_format_precedence_cli_override_wins() {
    use commentflow::config::resolve;
    use std::fs;
    let tmp = fresh_tmp_under_cwd("cl-cli");
    fs::write(tmp.join(".clang-format"), "ColumnLimit: 80\n").unwrap();
    let file = tmp.join("src.c");
    fs::write(&file, "int x = 0;\n").unwrap();
    let n = resolve(&file, Some(50)).unwrap().column_limit;
    assert_eq!(n, 50, "--column-limit always wins");
    let _ = fs::remove_dir_all(&tmp);
}

// check_and_diff_no_writes

#[test]
fn check_does_not_write() {
    use std::fs;
    use std::process::Command;
    let tmp = fresh_tmp_under_cwd("check-nowrite");
    let path = tmp.join("input.c");
    let src = "#include <stdio.h>\n/* a long comment. Returns the value on success or an error code. */\nint f(void) { return 0; }\n";
    fs::write(&path, src).unwrap();
    let mtime_before = fs::metadata(&path).unwrap().modified().unwrap();
    // Sleep briefly to ensure mtime tick would be visible if a write occurred.
    std::thread::sleep(std::time::Duration::from_millis(10));
    let bin = env!("CARGO_BIN_EXE_commentflow");
    let status = Command::new(bin)
        .arg("--check")
        .arg(&path)
        .status()
        .expect("run binary");
    // Either exit 0 (no change) or 1 (would change); both must not write.
    assert!(status.code() == Some(0) || status.code() == Some(1));
    let mtime_after = fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(mtime_before, mtime_after, "--check must not write to disk");
    let after = fs::read_to_string(&path).unwrap();
    assert_eq!(src, after, "file content must be unchanged");
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn diff_does_not_write() {
    use std::fs;
    use std::process::Command;
    let tmp = fresh_tmp_under_cwd("diff-nowrite");
    let path = tmp.join("input.c");
    let src = "#include <stdio.h>\n/* a long comment. Returns the value on success or an error code. */\nint f(void) { return 0; }\n";
    fs::write(&path, src).unwrap();
    let mtime_before = fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    let bin = env!("CARGO_BIN_EXE_commentflow");
    let status = Command::new(bin)
        .arg("--diff")
        .arg(&path)
        .status()
        .expect("run binary");
    assert_eq!(status.code(), Some(0), "--diff must exit 0");
    let mtime_after = fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(mtime_before, mtime_after, "--diff must not write to disk");
    let after = fs::read_to_string(&path).unwrap();
    assert_eq!(src, after);
    let _ = fs::remove_dir_all(&tmp);
}

// working_dir_clang_format

#[test]
fn working_dir_clang_format_under_cwd_uses_file_anchored() {
    use commentflow::config::resolve;
    use std::fs;

    // File under the test tmp dir (which is under cwd, so file-anchored walk
    // applies). The file's anchored ".clang-format" wins.
    let tmp = fresh_tmp_under_cwd("wdcl-under");
    let project = tmp.join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join(".clang-format"), "ColumnLimit: 120\n").unwrap();
    let file = project.join("src.c");
    fs::write(&file, "int x = 0;\n").unwrap();
    let n = resolve(&file, None).unwrap().column_limit;
    assert_eq!(n, 120);
    let _ = fs::remove_dir_all(&tmp);
}

// replacement_range_invariants

#[test]
fn replacement_range_invariants_overlapping_rejected() {
    use commentflow::parse::Comment;
    use commentflow::rewrite::{Replacement, validate};
    let source = "// hello\nint x;\n";
    let reps = vec![
        Replacement {
            start: 0,
            end: 5,
            text: "// HEL".to_string(),
        },
        Replacement {
            start: 3,
            end: 8,
            text: "lll".to_string(),
        },
    ];
    let comments = vec![Comment {
        start_byte: 0,
        end_byte: 8,
        start_col_byte: 0,
        text: "// hello".to_string(),
        force_passthrough: false,
        is_trailing: false,
        line_indent_bytes: String::new(),
        fallback_ending: "\n",
        at_file_start: false,
        relocate_before: None,
        param_shift: None,
    }];
    assert!(validate(&reps, source, &comments).is_err());
}

#[test]
fn replacement_range_invariants_outside_comment_rejected() {
    use commentflow::parse::Comment;
    use commentflow::rewrite::{Replacement, validate};
    let source = "int x = 1;\n// hello\n";

    // Range straddles the "int x = 1;" portion which is non-whitespace
    // non-comment bytes: must be rejected.
    let reps = vec![Replacement {
        start: 0,
        end: 19,
        text: "FOO".to_string(),
    }];
    let comments = vec![Comment {
        start_byte: 11,
        end_byte: 19,
        start_col_byte: 0,
        text: "// hello".to_string(),
        force_passthrough: false,
        is_trailing: false,
        line_indent_bytes: String::new(),
        fallback_ending: "\n",
        at_file_start: false,
        relocate_before: None,
        param_shift: None,
    }];
    assert!(validate(&reps, source, &comments).is_err());
}

#[test]
fn replacement_range_invariants_valid_range_passes() {
    use commentflow::parse::Comment;
    use commentflow::rewrite::{Replacement, validate};
    let source = "// hello\n";
    let reps = vec![Replacement {
        start: 0,
        end: 8,
        text: "// HELLO".to_string(),
    }];
    let comments = vec![Comment {
        start_byte: 0,
        end_byte: 8,
        start_col_byte: 0,
        text: "// hello".to_string(),
        force_passthrough: false,
        is_trailing: false,
        line_indent_bytes: String::new(),
        fallback_ending: "\n",
        at_file_start: false,
        relocate_before: None,
        param_shift: None,
    }];
    assert!(validate(&reps, source, &comments).is_ok());
}

#[test]
fn zero_width_insert_at_relocation_target_passes() {
    use commentflow::parse::Comment;
    use commentflow::rewrite::{Replacement, validate};

    // Manual-page relocation shape: delete the comment line, insert the cleaned
    // copy at the function start (offset 0, a recorded relocate_before).
    let source = "int f(void)\n/* c */\n{}\n";
    let reps = vec![
        Replacement {
            start: 0,
            end: 0,
            text: "/* c */\n\n".to_string(),
        },
        Replacement {
            start: 12,
            end: 20,
            text: String::new(),
        },
    ];
    let comments = vec![Comment {
        start_byte: 12,
        end_byte: 19,
        start_col_byte: 0,
        text: "/* c */".to_string(),
        force_passthrough: false,
        is_trailing: false,
        line_indent_bytes: String::new(),
        fallback_ending: "\n",
        at_file_start: false,
        relocate_before: Some(0),

        param_shift: None,
    }];
    assert!(validate(&reps, source, &comments).is_ok());
}

#[test]
fn zero_width_insert_off_target_rejected() {
    use commentflow::parse::Comment;
    use commentflow::rewrite::{Replacement, validate};

    // A non-empty zero-width insert whose offset is NOT any comment's
    // relocate_before would splice text into arbitrary code: reject it.
    let source = "int f(void)\n/* c */\n{}\n";
    let reps = vec![Replacement {
        start: 5,
        end: 5,
        text: "/* injected */".to_string(),
    }];
    let comments = vec![Comment {
        start_byte: 12,
        end_byte: 19,
        start_col_byte: 0,
        text: "/* c */".to_string(),
        force_passthrough: false,
        is_trailing: false,
        line_indent_bytes: String::new(),
        fallback_ending: "\n",
        at_file_start: false,
        relocate_before: Some(0), // target is 0, not 5
        param_shift: None,
    }];
    assert!(validate(&reps, source, &comments).is_err());
}

#[test]
fn zero_width_insert_sharing_a_start_is_not_a_false_overlap() {
    use commentflow::parse::Comment;
    use commentflow::rewrite::{Replacement, validate};

    // A zero-width insert at offset 5 alongside an in-place comment rewrite
    // that also starts at 5. The insert is a boundary, not an overlap; the
    // (start, end) sort must order it first so "validate" accepts both
    // regardless of the order they were pushed.
    let source = "12345// HI\n/* c */\n";
    let reps = vec![
        // Pushed range-first on purpose, the order that used to false-bail.
        Replacement {
            start: 5,
            end: 10,
            text: "// HELLO".to_string(),
        },
        Replacement {
            start: 5,
            end: 5,
            text: "/* c */\n\n".to_string(),
        },
    ];
    let comments = vec![
        Comment {
            start_byte: 5,
            end_byte: 10,
            start_col_byte: 5,
            text: "// HI".to_string(),
            force_passthrough: false,
            is_trailing: true,
            line_indent_bytes: String::new(),
            fallback_ending: "\n",
            at_file_start: false,
            relocate_before: None,
            param_shift: None,
        },
        Comment {
            start_byte: 11,
            end_byte: 18,
            start_col_byte: 0,
            text: "/* c */".to_string(),
            force_passthrough: false,
            is_trailing: false,
            line_indent_bytes: String::new(),
            fallback_ending: "\n",
            at_file_start: false,
            relocate_before: Some(5), // authorizes the insert at offset 5
            param_shift: None,
        },
    ];
    assert!(
        validate(&reps, source, &comments).is_ok(),
        "boundary insert must not read as an overlap"
    );
}

#[test]
fn empty_zero_width_span_always_ok() {
    use commentflow::parse::Comment;
    use commentflow::rewrite::{Replacement, validate};
    // A zero-width span with empty text is a no-op and needs no authorization.
    let source = "int x;\n";
    let reps = vec![Replacement {
        start: 3,
        end: 3,
        text: String::new(),
    }];
    let comments: Vec<Comment> = vec![];
    assert!(validate(&reps, source, &comments).is_ok());
}

// trailing_comment_conservative

#[test]
fn trailing_comment_long_passes_through() {
    let src = "int x = 42; // a very long trailing comment that absolutely exceeds the column limit we set for the test\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(src, out, "trailing comment must pass through, got:\n{out}");
}

#[test]
fn trailing_comment_short_passes_through() {
    let src = "int x = 42; // short\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(src, out);
}

// block_prefix_line_count_change

#[test]
fn block_prefix_line_count_change_all_double_star() {
    // Original continuation lines use "**" → all body lines normalize to the
    // canonical " * " and the result round-trips stably.
    let src = "/*\n ** first long body line designed to be wide enough that the formatter will keep it as a wrap\n ** second long body line that is also wide enough to ensure stable round-tripping happens here\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        !out.lines().any(|l| l.trim_start().starts_with("**")),
        "** must normalize to *, got:\n{out}"
    );
}

#[test]
fn block_prefix_line_count_change_single_line_splits_to_canonical() {
    // Single-line block that reflow now splits → newly introduced lines use the
    // canonical " * ".
    let src = "#include <stdio.h>\n/* this is a single source-line block comment that must be split into multiple lines when the formatter is given a tight column budget */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 60);
    // The continuation lines should use " * " prefix.
    let star_lines = out
        .lines()
        .filter(|l| l.trim_start().starts_with("* "))
        .count();
    assert!(
        star_lines >= 1,
        "newly introduced continuation lines must use ' * ', got:\n{out}"
    );
}

// Return rule negatives (boundary checks)

#[test]
fn return_rule_negative_returning_does_not_split() {
    // "Returning" has a "Return" prefix but the next char is alphabetic so the
    // word-boundary check must fail. No paragraph split, no forced blank line.
    let src = "#include <stdio.h>\n/* Some intro text long enough to force reflow at the column limit. Returning early avoids the work that follows in this function. */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);

    // If the Return rule fired, "Returning early..." would appear on a fresh
    // line with " * Returning" as the first non-prefix content. Check that it
    // stays glued to the prior sentence.
    let bad_split = out
        .lines()
        .any(|l| l.trim_start().starts_with("* Returning"));
    assert!(
        !bad_split,
        "'Returning' must NOT trigger the Return rule, got:\n{out}"
    );
}

#[test]
fn return_rule_negative_returns_with_digit_suffix() {
    // "Returns123": the same word-boundary defense. Alphanumeric after the
    // token name disqualifies the match.
    let src = "#include <stdio.h>\n/* Some intro text long enough to force reflow at the column limit. Returns123 is a fictional symbol referenced in the documentation here. */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let bad_split = out
        .lines()
        .any(|l| l.trim_start().starts_with("* Returns123"));
    assert!(
        !bad_split,
        "'Returns123' must NOT trigger the Return rule, got:\n{out}"
    );
}

#[test]
fn return_rule_negative_doxygen_returning_tag_unknown() {
    // "@returning" is not a recognized Doxygen tag. The mid-paragraph split
    // must not fire (it only fires on "@return" / "@returns" exact match
    // followed by word boundary).
    let src = "#include <stdio.h>\n/* Some intro text long enough to force reflow at the column limit. @returning is not a known tag and must stay inline. */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let bad_split = out
        .lines()
        .any(|l| l.trim_start().starts_with("* @returning"));
    assert!(
        !bad_split,
        "unknown '@returning' must NOT trigger the Return rule, got:\n{out}"
    );
}

// replacement_range_invariants extra coverage

#[test]
fn replacement_range_invariants_end_past_source_rejected() {
    use commentflow::parse::Comment;
    use commentflow::rewrite::{Replacement, validate};
    let source = "// hi\n";
    let comments = vec![Comment {
        start_byte: 0,
        end_byte: 5,
        start_col_byte: 0,
        text: "// hi".to_string(),
        force_passthrough: false,
        is_trailing: false,
        line_indent_bytes: String::new(),
        fallback_ending: "\n",
        at_file_start: false,
        relocate_before: None,
        param_shift: None,
    }];
    let reps = vec![Replacement {
        start: 0,
        end: 999,
        text: "x".to_string(),
    }];
    assert!(validate(&reps, source, &comments).is_err());
}

#[test]
fn replacement_range_invariants_inverted_range_rejected() {
    use commentflow::parse::Comment;
    use commentflow::rewrite::{Replacement, validate};
    let source = "// hello\n";
    let comments = vec![Comment {
        start_byte: 0,
        end_byte: 8,
        start_col_byte: 0,
        text: "// hello".to_string(),
        force_passthrough: false,
        is_trailing: false,
        line_indent_bytes: String::new(),
        fallback_ending: "\n",
        at_file_start: false,
        relocate_before: None,
        param_shift: None,
    }];
    let reps = vec![Replacement {
        start: 5,
        end: 2,
        text: "x".to_string(),
    }];
    assert!(validate(&reps, source, &comments).is_err());
}

#[test]
fn replacement_range_invariants_non_utf8_boundary_rejected() {
    use commentflow::parse::Comment;
    use commentflow::rewrite::{Replacement, validate};

    // "你好" is two 3-byte chars. Bytes 0..3 = "你", 3..6 = "好". A range
    // starting at 1 sits in the middle of "你", not a char boundary.
    let source = "// 你好\n";
    let comments = vec![Comment {
        start_byte: 0,
        end_byte: source.len() - 1,
        start_col_byte: 0,
        text: source[..source.len() - 1].to_string(),
        force_passthrough: false,
        is_trailing: false,
        line_indent_bytes: String::new(),
        fallback_ending: "\n",
        at_file_start: false,
        relocate_before: None,
        param_shift: None,
    }];
    let reps = vec![Replacement {
        start: 1,
        end: 4,
        text: "x".to_string(),
    }];
    assert!(validate(&reps, source, &comments).is_err());
}

#[test]
fn replacement_range_invariants_group_span_whitespace_accepted() {
    use commentflow::parse::Comment;
    use commentflow::rewrite::{Replacement, validate};

    // Two consecutive line comments with one newline between them. A
    // replacement covering the whole grouped span is valid because the
    // inter-comment bytes are whitespace only.
    let source = "// one\n// two\n";
    let comments = vec![
        Comment {
            start_byte: 0,
            end_byte: 6,
            start_col_byte: 0,
            text: "// one".to_string(),
            force_passthrough: false,
            is_trailing: false,
            line_indent_bytes: String::new(),
            fallback_ending: "\n",
            at_file_start: false,
            relocate_before: None,
            param_shift: None,
        },
        Comment {
            start_byte: 7,
            end_byte: 13,
            start_col_byte: 0,
            text: "// two".to_string(),
            force_passthrough: false,
            is_trailing: false,
            line_indent_bytes: String::new(),
            fallback_ending: "\n",
            at_file_start: false,
            relocate_before: None,
            param_shift: None,
        },
    ];
    let reps = vec![Replacement {
        start: 0,
        end: 13,
        text: "// merged\n".to_string(),
    }];
    assert!(
        validate(&reps, source, &comments).is_ok(),
        "grouped-span replacement with whitespace-only inter-comment bytes must validate"
    );
}

// shell_shebang_passthrough
#[test]
fn shell_shebang_is_not_extracted_as_comment() {
    let src = "#!/usr/bin/env bash\n# regular\necho hi\n";
    let comments = extract_comments(src, detect("foo.sh")).unwrap();
    assert_eq!(
        comments.len(),
        1,
        "expected exactly 1 comment (the shebang must be filtered); got {} comments",
        comments.len()
    );
    assert!(
        comments[0].text.starts_with("# regular"),
        "wrong comment extracted: {:?}",
        comments[0].text
    );
}

// shell_shebang_mid_file_is_a_comment
#[test]
fn shell_hashbang_mid_file_is_a_regular_comment() {
    // A "#!" sequence that is NOT at file start is a comment per the kernel
    // exec(2) contract, so it must enter the reflow stream.
    let src = "echo a\n#! looks like a shebang but isn't\n";
    let comments = extract_comments(src, detect("foo.sh")).unwrap();
    assert_eq!(comments.len(), 1);
    assert!(comments[0].text.starts_with("#!"));
}

// shell_style_preserved
#[test]
fn shell_hash_comment_stays_hash() {
    let src = "# the quick brown fox jumps over the lazy dog and keeps on running far past column eighty\necho hi\n";
    let out = pipeline(src, detect("foo.sh"), 60);
    assert!(
        !out.contains("// "),
        "shell `#` comment must not become `//` in output: {out}"
    );
    assert!(
        out.contains("# the quick"),
        "leading `# ` prefix lost: {out}"
    );
}

// shell_shebang_unchanged_in_pipeline
#[test]
fn shell_full_pipeline_leaves_shebang_byte_identical() {
    let src = "#!/usr/bin/env bash\n# a short note\necho hi\n";
    let out = pipeline(src, detect("foo.sh"), 80);
    assert!(
        out.starts_with("#!/usr/bin/env bash\n"),
        "shebang corrupted: {out}"
    );
}

// shell_double_hash_preserved
#[test]
fn shell_double_hash_marker_not_corrupted() {
    // Regression: the first cut stripped only one "#", leaving the second in
    // the body: "## bar ..." emitted as "# # bar ...".
    let src = "## bar is a section heading and it is long enough to need reflow at column sixty\necho hi\n";
    let out = pipeline(src, detect("foo.sh"), 60);
    assert!(
        !out.contains("# # "),
        "double-hash marker collapsed to `# # `: {out}"
    );
    assert!(
        out.contains("## "),
        "double-hash marker lost entirely: {out}"
    );
}

// shell_triple_hash_preserved
#[test]
fn shell_triple_hash_marker_not_corrupted() {
    let src =
        "### deeply nested heading text that runs past the column limit on purpose\necho hi\n";
    let out = pipeline(src, detect("foo.sh"), 60);
    assert!(
        !out.contains("# ## ") && !out.contains("## # "),
        "triple-hash marker corrupted: {out}"
    );
    assert!(out.contains("### "), "triple-hash marker lost: {out}");
}

// shell_long_mid_file_hashbang_stays_shell
#[test]
fn shell_long_mid_file_hashbang_keeps_marker_through_reflow() {
    // Regression for a Codex finding: when a mid-file "#!" comment was long
    // enough to skip the single-line fast path, strip_one_line refused to
    // assign it any marker, so line_marker stayed None and reflow's Style::Line
    // emitted "//" (Rust/C default) instead of shell's "#!", which silently
    // converting a shell comment to C-style.
    let src = "echo a\n#! this is not a real shebang but it does run past the column limit on purpose\necho b\n";
    let out = pipeline(src, detect("foo.sh"), 60);
    assert!(
        !out.contains("// "),
        "mid-file `#!` was converted to `//`: {out}"
    );
    assert!(out.contains("#!"), "mid-file `#!` marker lost: {out}");
}

// shell_compact_hashbang_canonicalizes_space
#[test]
fn shell_compact_mid_file_hashbang_gains_canonical_space() {
    // A mid-file "#!foo" (no space) normalizes to "#! foo": same
    // canonicalization the tool already applies to "//foo" and "#foo". Pinned
    // to lock the decision; revisit only if shell users complain.
    let src = "echo a\n#!foo this is a long mid-file hashbang line that exceeds the column budget\necho b\n";
    let out = pipeline(src, detect("foo.sh"), 60);
    assert!(
        out.contains("#! "),
        "expected `#! ` canonical prefix: {out}"
    );
    assert!(
        !out.contains("// "),
        "mid-file `#!foo` was converted to `//`: {out}"
    );
}

// shell_bom_shebang_filtered
#[test]
fn shell_bom_prefixed_shebang_is_filtered_at_extraction() {
    // Lock in the BOM behavior: tree-sitter-bash starts the comment node AFTER
    // the BOM (at byte 3), so "is_shell_shebang" with "bom_offset = 3"
    // correctly matches. If tree-sitter ever changes to include BOM in the node
    // range, this test fires and the filter needs revisiting.
    let mut src_bytes = Vec::new();
    src_bytes.extend_from_slice(b"\xef\xbb\xbf");
    src_bytes.extend_from_slice(b"#!/usr/bin/env bash\n# real comment\necho hi\n");
    let src = std::str::from_utf8(&src_bytes).unwrap();
    let comments = extract_comments(src, detect("foo.sh")).unwrap();
    assert_eq!(
        comments.len(),
        1,
        "expected exactly 1 comment (BOM + shebang filtered, real comment kept); got {}",
        comments.len()
    );
    assert!(
        comments[0].text.starts_with("# real"),
        "wrong comment extracted: {:?}",
        comments[0].text
    );
}

// shell_mixed_hash_runs_do_not_merge
#[test]
fn shell_mixed_hash_runs_stay_in_separate_paragraphs() {
    // Run-length matters: "# foo" and "## bar" must NOT merge into one
    // paragraph because shell convention treats run length as a visual heading
    // level.
    let src = "# regular comment line\n## section heading line\necho hi\n";
    let out = pipeline(src, detect("foo.sh"), 80);
    assert!(out.contains("# regular"), "first marker dropped: {out}");
    assert!(
        out.contains("## section"),
        "second marker dropped or merged: {out}"
    );
}

// bookend_strip_single_line_labeled_c
#[test]
fn bookend_strip_labeled_dash_run_c() {
    let src = "int main(void) {\n// -------- section foo --------\nreturn 0;\n}\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("// section foo\n"),
        "labeled bookend not collapsed: {out}"
    );
    assert!(
        !out.contains("--------"),
        "dash bookend bytes survived: {out}"
    );
}

// bookend_strip_single_line_labeled_rust_doc
#[test]
fn bookend_strip_labeled_equals_run_rustdoc() {
    let src = "/// === Section title ===\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert!(
        out.contains("/// Section title\n"),
        "labeled equals bookend not collapsed in Rustdoc: {out}"
    );
}

// bookend_strip_shell_label
#[test]
fn bookend_strip_labeled_shell() {
    let src = "# ----- setup -----\necho hi\n";
    let out = pipeline(src, detect("foo.sh"), 80);
    assert!(
        out.contains("# setup\n"),
        "shell labeled bookend not collapsed: {out}"
    );
}

// bookend_strip_no_label_singleline_preserved
#[test]
fn bookend_strip_skips_bare_singleline_setext() {
    // A standalone "/// =====" line MUST NOT be stripped: at the single-comment
    // scope it is indistinguishable from a setext underline, and we already pin
    // setext-header preservation elsewhere in this file.
    let src = "/// Title\n/// =====\n///\n/// Body\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert!(
        out.contains("/// =====\n"),
        "bare bookend on standalone line wrongly collapsed: {out}"
    );
}

#[test]
fn bookend_strip_bare_singleline_c() {
    let src = "int main(void) {\n// --------\nreturn 0;\n}\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("//\n"),
        "bare C bookend not collapsed to blank comment line: {out}"
    );
    assert!(
        !out.contains("--------"),
        "dash bookend bytes survived: {out}"
    );
}

#[test]
fn bookend_strip_bare_singleline_shell() {
    let src = "# =====\necho hi\n";
    let out = pipeline(src, detect("foo.sh"), 80);
    assert!(
        out.starts_with("#\n"),
        "bare shell bookend not collapsed to blank comment line: {out}"
    );
    assert!(
        !out.contains("====="),
        "equals bookend bytes survived: {out}"
    );
}

// bookend_strip_inside_block_comment
#[test]
fn bookend_strip_inside_block_comment_label_collapses() {
    // The label survives; the dash runs do not. Whether the stripped label
    // paragraphs back into the following prose or stays on its own line is up
    // to the paragraph grouper, v0.1 prefers compaction.
    let src = "/*\n * -------- section --------\n * body content here\n */\nint x = 0;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(out.contains("section"), "label lost: {out}");
    assert!(
        !out.contains("--------"),
        "dash bookend bytes survived inside block: {out}"
    );
}

// bookend_strip_protected_by_art_adjacency
#[test]
fn bookend_strip_cancelled_when_adjacent_to_ascii_art() {
    // Box border around prose: the dash run is structural, not
    // decorative. Adjacency to "|  ...  |" cancels the strip.
    let src = "/*\n * ----------\n * | hello  |\n * ----------\n */\nint x = 0;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("----------"),
        "dash border around art wrongly collapsed: {out}"
    );
    assert!(out.contains("| hello  |"), "art body lost: {out}");
}

#[test]
fn bookend_strip_labeled_art_adjacent_passes_verbatim() {
    let src = "/*\n * ----------   diagram   ----------\n * |      |\n */\nint x = 0;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("* ----------   diagram   ----------"),
        "art-adjacent labeled bookend was normalized: {out}"
    );
    assert!(out.contains("* |      |"), "adjacent art line lost: {out}");
}

// star_banner_box_collapses_to_content
#[test]
fn star_banner_box_collapses_to_single_line() {
    // The classic AI/legacy banner: a full-asterisk frame above and below a
    // single short prose line. The "/****/" rows are decoration (Doxygen also
    // treats "/***" as a non-doc comment), so they merge and collapse, leaving
    // just the content as a one-line block comment.
    let src = "void f(void) {}\n/*****************************************/\n/* Output messages to the system logger. */\n/*****************************************/\nint x;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("/* Output messages to the system logger. */"),
        "content not collapsed to single-line block: {out}"
    );
    assert!(!out.contains("****"), "star banner frame survived: {out}");
}

// A banner whose rule run sits on ONE side is not collapsed (a banner must be
// bracketed on both sides, or "--- a/kernel/sched.c" loses its dashes), which
// used to leave it as prose for reflow to wrap. The wrap stranded the rule run
// on its own line, the next pass read that line as a bare rule and deleted it,
// and the file settled only on its second run. ICU's utf8.h, at the default
// column limit. The line is frozen now: nothing here wants rewrapping.

#[test]
fn one_sided_banner_single_line_block_is_left_alone() {
    let src = "/* single-code point definitions -------------------------------------------- */\nint f(void);\n";
    let out = pipeline(src, detect("foo.c"), 60);
    assert_eq!(src, out, "one-sided banner was rewritten");
}

#[test]
fn one_sided_banner_at_default_limit_is_left_alone() {
    let src = "/* definitions with backward iteration and a somewhat longer label ------------------- */\nint f(void);\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(
        src, out,
        "one-sided banner was rewritten at the default limit"
    );
}

#[test]
fn one_sided_banner_inside_a_block_is_frozen() {
    let src = "/*\n * single-code point definitions --------------------------------------------\n *\n * some prose here\n */\nint f(void);\n";
    let out = pipeline(src, detect("foo.c"), 60);
    assert_eq!(src, out, "banner line inside a block was rewrapped");
}

#[test]
fn one_sided_banner_with_the_run_leading_is_frozen() {
    let src = "/*\n * ------------------------------- label here\n */\nint f(void);\n";
    let out = pipeline(src, detect("foo.c"), 30);
    assert_eq!(src, out, "leading-run banner was rewrapped");
}

#[test]
fn one_sided_banner_in_a_line_comment_is_left_alone() {
    let src = "// single-code point definitions --------------------------------------------\nint f(void);\n";
    let out = pipeline(src, detect("foo.c"), 60);
    assert_eq!(src, out, "one-sided banner in a line comment was rewritten");
}

/// The freeze is gated on standing alone in a paragraph, because a line ending
/// in "---" inside one is overwhelmingly a sentence wrapped right after an
/// em-dash written as three hyphens (apr_pools.h). Freezing that would strand
/// the rest of the sentence, which is the exact damage this tool repairs.
#[test]
fn em_dash_inside_a_paragraph_still_reflows() {
    let src = "/*\n * a strategy that is fundamentally unsound and quite long indeed ---\n * continues here with more words to wrap around\n */\nint f(void);\n";
    let out = pipeline(src, detect("foo.c"), 50);
    assert_eq!(
        out,
        "/*\n * a strategy that is fundamentally unsound and\n * quite long indeed --- continues here with more\n * words to wrap around\n */\nint f(void);\n"
    );
}

// star_box_around_framed_label_preserved
#[test]
fn star_box_around_framed_label_preserved() {
    // A star rule bracketing a SIDE-FRAMED label ("* Title *") is a drawn box,
    // not a banner-over-prose: the adjacency check must keep it verbatim. The
    // rule rows must survive (a side-framed label cancels their strip);
    // interior label padding may still reflow, matching the existing
    // double-star banner contract.
    let src =
        "int q;\n/*\n * ****************\n * *    Title    *\n * ****************\n */\nint r;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("****************"),
        "star box rule lost: {out}"
    );
    assert!(out.contains("Title"), "boxed label text lost: {out}");
}

// star_bare_rule_collapses_like_dash_equals
#[test]
fn star_bare_rule_collapses_in_plain_block() {
    // A standalone "***" rule inside a plain/Doxygen block is a decorative
    // horizontal rule with zero information; it collapses just like a bare
    // "---" or "===" already does. Deliberate: the whole point of the feature.
    let src = "/*\n * Paragraph one.\n * ***\n * Paragraph two.\n */\nint x = 0;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(!out.contains("***"), "bare star rule survived: {out}");
    assert!(out.contains("Paragraph one."), "prose lost: {out}");
    assert!(out.contains("Paragraph two."), "prose lost: {out}");
}

// star_emphasis_does_not_cancel_adjacent_rule
#[test]
fn star_emphasis_does_not_cancel_adjacent_bookend() {
    // Markdown emphasis ("*emphasis*") hugs the marker with no inner space, so
    // it is NOT a box wall and must not protect an adjacent "***" rule from
    // collapsing.
    let src = "/*\n * ***\n * *emphasis*\n */\nint x = 0;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(out.contains("*emphasis*"), "emphasis text lost: {out}");
    assert!(
        !out.lines().any(|l| l.trim() == "* ***"),
        "bare star rule wrongly preserved next to emphasis: {out}"
    );
}

// single_line_labeled_star_banner_collapses
#[test]
fn single_line_labeled_star_banner_collapses() {
    // A one-line labeled star banner collapses to its label, like the dash form
    // already did. The "/*" delimiter eats one star each side, so 4+ total
    // stars are needed to clear the >=3 bookend run; exactly-3 ("/*** x ***/")
    // is the irreducible floor and passes through.
    let src = "int a;\n/**** install hooks ****/\nint b;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("/* install hooks */"),
        "labeled star banner not collapsed: {out}"
    );
    assert!(!out.contains("****"), "star frame survived: {out}");

    // Doc comments are NOT banners, must pass through untouched.
    let doc = "int a;\n/** brief note stays */\nint b;\n";
    assert_eq!(pipeline(doc, detect("foo.c"), 80), doc);
}

// stacked_solid_star_rules_are_art_not_banner
#[test]
fn stacked_solid_star_rules_preserved_as_art() {
    // A bar chart / thick border drawn from consecutive solid-star rows is a
    // drawing, not a banner: two adjacent rules protect each other, so no row
    // is dropped. Regression for the multi-row-art corruption.
    let src = "/*\n * *****\n * ***\n * *\n */\nint x = 0;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(out.contains("*****"), "top bar row dropped: {out}");
    assert!(
        out.matches('*').count() >= src.matches('*').count(),
        "a star row was lost: {out}"
    );
}

fn planned(src: &str, path: &str, col: usize) -> String {
    pipeline(src, detect(path), col)
}

#[test]
fn degenerate_block_body_is_left_alone_not_mangled() {
    // Body is a lone "/", so the emitted opener and the raw-replayed closer
    // overlap into "/*/", an unterminated comment that swallows the rest of the
    // file. The well-formedness postcondition must refuse it.
    let src = "int y;\n/*\n/*/\nint x;\n";
    let out = planned(src, "foo.c", 80);
    assert_eq!(out, src, "degenerate body must pass through untouched");
    assert_ne!(
        out, "int y;\n/*/\nint x;\n",
        "collapsed the opener and closer into an unterminated comment"
    );
}

#[test]
fn plain_block_never_promoted_to_doc_block() {
    // "/**" on the second source line of a plain "/*" block is body content,
    // not the opener. Replaying it raw would turn the comment into a doc block
    // and violate style preservation.
    let src = "int y;\n/*\n/**\n\nbody\n*/\nint x;\n";
    let out = planned(src, "foo.c", 80);
    assert_eq!(out, src, "style must be preserved");
    assert_eq!(planned(&out, "foo.c", 80), out, "must be idempotent");
}

#[test]
fn trailing_only_rule_is_content_not_decoration() {
    // A banner is bracketed on both sides. A line merely ENDING in a rule is
    // almost always a sentence wrapped after an em-dash written "---", and
    // collapsing it deletes a token the author wrote. Real instances live in
    // apr_pools.h and httpd.h.
    let src = "/*\n * a fundamentally unsound strategy ---\n * particularly in the presence of die().\n */\nint x;\n";
    let out = planned(src, "foo.c", 80);
    assert!(out.contains("strategy ---"), "em-dash deleted:\n{out}");
}

#[test]
fn labeled_bookend_over_a_bare_rule_converges_in_one_pass() {
    // The art-adjacency guard spares a labeled bookend whose neighbor is a bare
    // rule, but that rule is blanked in the same pass. If the label then
    // collapses on the next pass, "format && --check" fails forever.
    let src = "/*\n * Section one ----------\n * ----------------------\n */\nint x;\n";
    let out = planned(src, "foo.c", 80);
    assert_eq!(
        planned(&out, "foo.c", 80),
        out,
        "did not converge in one pass"
    );
}

#[test]
fn emacs_mode_line_survives_the_two_sided_rule() {
    // "-*-" is the Emacs file-variables marker, not a banner: it is bookend
    // characters but not a uniform run, so both sides must be uniform before a
    // labeled bookend collapses.
    let src = "/* -*- Mode: C; c-basic-offset: 4; indent-tabs-mode: nil -*- */\nint x;\n";
    let out = planned(src, "foo.c", 80);
    assert!(out.contains("-*-"), "Emacs mode line eaten:\n{out}");
}

#[test]
fn leading_only_rule_is_content_not_decoration() {
    // "--- a/foo.c" is a diff header, not a banner. A banner is bracketed on
    // both sides; collapsing a one-sided run destroys quoted patch text.
    let src = "/*\n * Applied upstream as:\n * --- a/kernel/sched.c\n * +++ b/kernel/sched.c\n */\nint x;\n";
    let out = planned(src, "foo.c", 80);
    assert!(
        out.contains("--- a/kernel/sched.c"),
        "diff header was eaten:\n{out}"
    );
}

#[test]
fn mixed_one_sided_run_is_content_not_decoration() {
    // "-*-" is an Emacs mode marker, not a rule. Only a uniform run counts as
    // one-sided decoration.
    let src =
        "/* A tutorial for i386 systems, by someone who cared about it. -*- asm -*- */\nint x;\n";
    let out = planned(src, "foo.c", 80);
    assert!(out.contains("-*- asm -*-"), "mode marker eaten: {out}");
}

#[test]
fn spdx_tag_stays_on_the_first_line() {
    // Merging the tag's comment into the header below it makes line 1 a bare
    // "/*", which reads as an unlicensed file to the kernel checker and reuse.
    let src = "/* SPDX-License-Identifier: GPL-2.0 */\n/*\n * Low-level entry\n * points here.\n */\nint x;\n";
    let out = planned(src, "foo.c", 80);
    assert!(
        out.starts_with("/* SPDX-License-Identifier: GPL-2.0 */\n"),
        "SPDX tag moved off line 1:\n{out}"
    );
    assert_eq!(planned(&out, "foo.c", 80), out, "must be idempotent");
}

#[test]
fn header_block_below_an_spdx_tag_keeps_file_header_form() {
    // Blocking the SPDX merge must not also demote the block below it out of
    // file-start handling: that loses the opener-alone layout and glues the
    // first body line onto "/*" with a dangling "*/" underneath.
    let src = "/* SPDX-License-Identifier: GPL-2.0 */\n/*\n * Copyright (c) 2020 Someone. All rights reserved. This is a long line indeed.\n */\nint x;\n";
    let out = planned(src, "foo.c", 80);
    assert!(
        out.contains("*/\n/*\n * Copyright"),
        "header block lost its opener-alone form:\n{out}"
    );
    assert_eq!(planned(&out, "foo.c", 80), out, "must be idempotent");
}
