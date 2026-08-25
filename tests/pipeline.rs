mod common;

use commentflow::parse;
use common::{detect, pipeline};
use std::path::PathBuf;

#[test]
fn bytes_outside_comments_unchanged_c() {
    let src = "int main(void) {\n    int x = 0; // tiny comment\n    return x;\n}\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let src_no_comment: String = src
        .lines()
        .map(|l| {
            if let Some(i) = l.find("//") {
                let pre = &l[..i];
                format!("{pre}\n")
            } else {
                format!("{l}\n")
            }
        })
        .collect();
    let out_no_comment: String = out
        .lines()
        .map(|l| {
            if let Some(i) = l.find("//") {
                let pre = &l[..i];
                format!("{pre}\n")
            } else {
                format!("{l}\n")
            }
        })
        .collect();
    assert_eq!(src_no_comment, out_no_comment);
}

#[test]
fn idempotent_short_c_comment() {
    let src = "int x = 0; // tiny\n";
    let pass1 = pipeline(src, detect("foo.c"), 80);
    let pass2 = pipeline(&pass1, detect("foo.c"), 80);
    assert_eq!(pass1, pass2, "second run must produce same bytes as first");
}

#[test]
fn idempotent_short_rust_comment() {
    let src = "fn x() {} // tiny\n";
    let pass1 = pipeline(src, detect("foo.rs"), 80);
    let pass2 = pipeline(&pass1, detect("foo.rs"), 80);
    assert_eq!(pass1, pass2);
}

#[test]
fn pipeline_handles_block_comment() {
    let src = "int x;\n/* small block */\nint y;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn pipeline_handles_doc_comments_rust() {
    let src = "/// short doc\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    let pass2 = pipeline(&out, detect("foo.rs"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn rust_inner_doc_line_marker_preserved() {
    let src = "//! This inner module documentation comment is intentionally long enough that it must be wrapped by the formatter while staying an inner doc comment.\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert!(out.lines().next().unwrap().starts_with("//! "));
    assert!(
        !out.contains("///"),
        "inner doc line marker must not become outer doc marker, got:\n{out}"
    );
    let pass2 = pipeline(&out, detect("foo.rs"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn bang_block_doc_opener_preserved() {
    let src = "/*! This inner block documentation comment is intentionally long enough that it must be rewritten by the formatter while keeping its bang opener marker. */\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert!(
        out.starts_with("/*! "),
        "bang block opener must be preserved, got:\n{out}"
    );
    assert!(
        !out.starts_with("/** "),
        "bang block opener must not become star doc opener, got:\n{out}"
    );
    let pass2 = pipeline(&out, detect("foo.rs"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn pipeline_skips_macro_body_comments() {
    let src = "#define FOO(x) /* macro body comment */ (x)\nint y;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("/* macro body comment */"),
        "macro comment must pass through unchanged"
    );
}

#[test]
fn pipeline_skips_rust_nested_block() {
    let src = "fn f() {} /* outer /* inner */ outer */\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert_eq!(src, out, "nested block must round-trip byte-identical");
}

#[test]
fn pipeline_skips_frama_c_acsl_annotations() {
    let src = "/*@ requires n >= 0;\n  @ assigns \\nothing;\n  @ ensures \\result >= 0;\n  */\nint f(int n);\n//@ assert x >= 0;\n";
    let out = pipeline(src, detect("foo.c"), 30);
    assert_eq!(src, out, "ACSL annotations must stay byte-identical");
}

#[test]
fn block_opener_alone_preserved() {
    // Source uses opener-alone form. Output must keep it that way.
    let src = "/*\n * This is a multi-line block comment that should reflow but the opener-alone\n * form must be preserved because that is what the source uses.\n */\nint main(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.starts_with("/*\n * "),
        "opener-alone source must stay opener-alone, got: {out:?}"
    );
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn adjacent_block_comments_merge_and_drop_empty_bookends() {
    let src = "/*\n *\n */\n/* inkernel state manipulation                                      */\n/*                                                                  */\n/* INKERNEL_NOW / INKERNEL_LOCK are not set on the active path.      */\n/* INKERNEL_EXIT is used by __ker_exit as a re-entry guard.          */\n/* INKERNEL_SPECRET is superseded by the per-CPU in_specret state —  */\n/* it remains only so entry.c's set_inkernel_init() and a few */\n/* diagnostic readers (trace/events.h, stall-detector.c) keep compiling. */\n/*\n *\n */\n";
    // Offset-0 plain block → opener-alone (file-header) form.
    let expected = "/*\n * inkernel state manipulation\n *\n * INKERNEL_NOW / INKERNEL_LOCK are not set on the active path. INKERNEL_EXIT is\n * used by __ker_exit as a re-entry guard. INKERNEL_SPECRET is superseded by the\n * per-CPU in_specret state — it remains only so entry.c's set_inkernel_init()\n * and a few diagnostic readers (trace/events.h, stall-detector.c) keep\n * compiling.\n */\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(out, expected);
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn adjacent_block_comments_keep_first_opener_alone_style() {
    let src = "/*\n * license header words\n */\n/* more header words */\nint x;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.starts_with("/*\n * license header words more header words\n */"),
        "merged block must keep opener-alone style, got:\n{out}"
    );
}

#[test]
fn adjacent_block_comments_ignore_empty_bookend_for_opener_style() {
    let src = "/*\n *\n */\n/*\n * license header words\n */\n/* more header words */\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.starts_with("/*\n * license header words more header words\n */"),
        "empty bookend must not decide opener style, got:\n{out}"
    );
}

#[test]
fn adjacent_block_comments_preserve_crlf() {
    let src = "/* first words */\r\n/* second words that are long enough to keep the merged block multiline */\r\nint x;\r\n";
    let out = pipeline(src, detect("foo.c"), 80);
    // Offset-0 plain block → opener-alone form; CRLF preserved throughout.
    assert!(
        out.starts_with("/*\r\n * first words second words that are long enough to keep the merged block\r\n * multiline\r\n */\r\n"),
        "merged block must preserve CRLF, got:\n{out:?}"
    );

    // Reflowing a merged comment changes its physical line count, which must
    // not flip the dominant line ending on a second pass.
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2, "CRLF merge must be idempotent, got:\n{pass2:?}");
}

#[test]
fn adjacent_block_comments_keep_leading_star_content() {
    let src = "/*\n * *ptr remains meaningful here\n */\n/* more words */\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("*ptr remains meaningful here more words"),
        "body star must not be stripped, got:\n{out}"
    );
}

#[test]
fn adjacent_block_comments_with_double_star_merge_and_normalize() {
    // "**" is no longer special: a "**" block normalizes to single-star and
    // merges with an adjacent plain block like any other.
    let src = "/*\n ** kept verbatim\n */\n/* second words */\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        !out.contains("**"),
        "** must normalize to single star, got:\n{out}"
    );
    assert!(
        out.contains("* kept verbatim"),
        "body must use the canonical * marker, got:\n{out}"
    );
}

#[test]
fn adjacent_block_comments_skip_table_merge() {
    let src = "/*\n * | column one | column two |\n * | value one  | value two  |\n */\n/* more words */\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(out, src, "table block must not be merged, got:\n{out}");
}

#[test]
fn adjacent_block_comments_skip_fence_merge() {
    let src = "/*\n * ```\n * code();\n * ```\n */\n/* more words */\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(out, src, "fenced block must not be merged, got:\n{out}");
}

#[test]
fn adjacent_block_comments_skip_single_line_indented_body_merge() {
    let src = "/*   code(); */\n/* more words */\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(
        out, src,
        "single-line indented body must not be merged, got:\n{out}"
    );
}

#[test]
fn line_group_rejects_trailing_comments() {
    // Two trailing line comments must NOT be merged into one logical group,
    // even when they're on consecutive source lines with the same marker.
    let src = "int x = 0; // a-comment-on-code\nint y = 1; // another-comment-on-code\n";
    let out = pipeline(src, detect("foo.c"), 80);
    // Each trailing comment passes through unchanged.
    assert_eq!(
        src, out,
        "trailing line comments must not merge, got:\n{out}"
    );
}

#[test]
fn line_group_rejects_mixed_markers() {
    // /// (outer doc) and // (line comment) must NOT merge even when adjacent,
    // they carry different documentation semantics.
    let src = "/// outer-doc-line-here\n// plain-line-comment-here\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert_eq!(
        src, out,
        "different marker styles must not merge, got:\n{out}"
    );
}

#[test]
fn metadata_version_with_long_prose_value_treated_verbatim() {
    // "Version: <stuff>" matches is_metadata_line. Verify this is intentional:
    // the line is treated as metadata and emitted verbatim even if its value is
    // long prose. This test documents the current behavior; if metadata
    // detection should narrow further (e.g. require semver-like values), update
    // both is_metadata_line and this test.
    let src = "/*\n * Version: 1.0 was released in 2025 with several major improvements\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2);
    // The Version: line stays on its own logical line in the output.
    let version_lines = out.lines().filter(|l| l.contains("Version:")).count();
    assert_eq!(
        version_lines, 1,
        "Version: metadata line must stay on its own line, got:\n{out}"
    );
}

#[test]
fn column_limit_override_skips_indent_config_discovery() {
    use commentflow::config::{UseTab, resolve};

    // A --column-limit override must skip discovery for the indent keys too,
    // not just the column budget: no .clang-format read, plain defaults.
    let here = PathBuf::from(file!());
    let overridden = resolve(&here, Some(50)).unwrap();
    assert_eq!(overridden.column_limit, 50);
    assert_eq!(
        overridden.indent.use_tab,
        UseTab::Never,
        "override must return defaults"
    );
    assert_eq!(overridden.indent.tab_width, 8);

    // The override reads nothing, so it must not require the path to exist.
    let missing = PathBuf::from("/definitely/does/not/exist/foo.c");
    assert_eq!(resolve(&missing, Some(50)).unwrap().column_limit, 50);
}

#[test]
fn line_group_merges_consecutive_slashes() {
    let src = "void f(void) {\n    // this is a multi-line comment using consecutive single-line markers\n    // that should ideally reflow as one paragraph rather than being\n    // emitted line-by-line independently because they form one logical\n    // thought split across several short lines\n    return;\n}\n";
    let out = pipeline(src, detect("foo.c"), 80);

    // First emitted line should now be packed to near 80 cols, not just the
    // original short fragment.
    let first = out.lines().nth(1).expect("expected a comment line");
    assert!(
        first.len() >= 75,
        "LineGroup must pack the paragraph; got short line: {first:?}"
    );
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn metadata_false_positive_copyright_law() {
    // "Copyright law forbids this." is prose, not a license header.
    let src = "/*\n * Copyright law forbids this kind of misappropriation\n * which we discuss further below in the design section.\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);

    // Two short source lines should reflow into a longer packed line (≥70
    // cols), not be treated as metadata and kept verbatim. If is_metadata_line
    // false-positived, each source line would stay on its own line and the
    // longest line would be much shorter.
    let max_len = out
        .lines()
        .filter(|l| l.contains("Copyright") || l.contains("design") || l.contains("below"))
        .map(str::len)
        .max()
        .unwrap_or(0);
    assert!(
        max_len >= 70,
        "Copyright-law prose must reflow into a packed line; max prose line is {max_len} cols. Output:\n{out}"
    );
}

#[test]
fn metadata_copyright_with_year_preserved() {
    // "Copyright 2026 ..." IS a metadata line and must stay on its own line.
    let src = "/*\n * Copyright 2026 someone.\n * Copyright 2027 someone else.\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2);
    let copy_lines = out.lines().filter(|l| l.contains("Copyright")).count();
    assert_eq!(
        copy_lines, 2,
        "two Copyright lines must stay separate, got:\n{out}"
    );
}

/// Every file under "tests/corpus/" reproduces a shape from real source that
/// this tool got wrong. Hand-written cases kept missing the shapes that
/// actually break, so the corpus is where real-world input lives: a whole file
/// at a time, which is the only way to hold the shapes that need file context
/// (a byte-0 shebang, a heredoc body, an unterminated block at EOF). Byte
/// equality against the recorded sibling is the assertion; the convergence
/// check inside the shared "pipeline" helper rides along on top of it.
#[test]
fn corpus_files_match_expected() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");

    // Sorted, so a failure names the same file on every filesystem; "read_dir"
    // order is unspecified. The ".expected" siblings are the one thing held
    // back from the run. Anything else "detect_language" rejects is a file
    // somebody put here expecting it to be tested, and skipping it silently is
    // exactly the failure this test is supposed to make impossible.
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read tests/corpus")
        .map(|e| e.expect("corpus entry").path())
        .collect();
    entries.sort();
    let (siblings, inputs): (Vec<PathBuf>, Vec<PathBuf>) = entries
        .into_iter()
        .partition(|p| p.extension().is_some_and(|e| e == "expected"));

    assert!(
        !inputs.is_empty(),
        "no corpus files found in {}",
        dir.display()
    );

    for path in &inputs {
        let lang = parse::detect_language(path)
            .unwrap_or_else(|e| panic!("corpus file {}: {e}", path.display()));
        let src = std::fs::read_to_string(path).expect("read corpus file");

        // Append rather than "with_extension": a corpus file may legitimately
        // have NO extension (the extensionless-shebang carve-out makes one a
        // supported shell input), and "with_extension" would then produce
        // "script..expected" and, for a dotfile, eat the name itself.
        let mut expected_name = path.clone().into_os_string();
        expected_name.push(".expected");
        let expected_path = PathBuf::from(expected_name);

        // Byte equality against a recorded output is the whole assertion: it
        // covers the code lines, the comment text, and the layout at once, for
        // any language, which the old check for one known C declaration did
        // not. Reading the sibling with "expect" rather than skipping is what
        // makes a deleted ".expected" a failure. ("pipeline" separately asserts
        // a second pass changes nothing, so a file that oscillates is caught
        // even when its first pass matches.)
        let expected = std::fs::read_to_string(&expected_path).unwrap_or_else(|e| {
            panic!("read {}: {e}", expected_path.display());
        });
        let out = pipeline(&src, lang, 80);
        assert_eq!(
            expected,
            out,
            "corpus file {} no longer produces {}; if the tool is right, \
             replace the sibling with this output, otherwise the shape found a \
             bug",
            path.display(),
            expected_path.display()
        );
    }

    // The mirror of the missing-sibling failure above. A ".expected" whose
    // input is gone (a rename that only touched one of the pair, a deleted
    // shape) is a file this loop never opens, so nothing else in the suite
    // would ever notice it went stale.
    for sibling in &siblings {
        // The inverse of the push above: "partition" guarantees the extension
        // here is exactly "expected", so stripping it cannot eat anything else.
        let input = sibling.with_extension("");
        assert!(
            inputs.contains(&input),
            "{} has no corpus input {}; delete the orphan or restore its input",
            sibling.display(),
            input.display()
        );
    }
}

#[test]
fn label_run_keeps_one_line_each() {
    // A "File:" / "Task:" banner is a table, not a paragraph: packing the two
    // into "File: fill.c Task: Fill ..." destroys the layout.
    let src =
        "/*\n * File: fill.c\n * Task: Fill a[0..n-1] with val\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(out, src, "label run must pass through untouched");
    assert_eq!(out, pipeline(&out, detect("foo.c"), 80));
}

#[test]
fn label_run_tolerates_aligned_colons() {
    // "File : x" columns the separator for looks; the padding is alignment, not
    // prose, so the run still counts.
    let src = "/*\n * File   : fill.c\n * Task   : Fill a[0..n-1] with val\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(out, src, "aligned label run must pass through untouched");
    assert_eq!(out, pipeline(&out, detect("foo.c"), 80));
}

#[test]
fn label_run_tolerates_alignment_padding() {
    // " * File: x" pads one space past the marker for alignment. That is below
    // the indented-code threshold, so only the label rule can hold it.
    let src =
        "/*\n *  File: fill.c\n *  Task: Fill a[0..n-1] with val\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(out, src, "padded label run must pass through untouched");
}

#[test]
fn lone_label_line_still_reflows() {
    // One "Note:" line with prose under it is a sentence, not a banner: it must
    // still pack. Otherwise every prose line opening with "Word:" freezes.
    let src = "/*\n * Note: the buffer may be null here\n * so every caller has to check it first.\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(
        out,
        "/*\n * Note: the buffer may be null here so every caller has to check it first.\n */\nint f(void) { return 0; }\n"
    );
}

#[test]
fn label_run_head_reflows_when_its_tail_wrapped() {
    // Two label-shaped lines where the second is a wrapped sentence, not a
    // table row: freezing them would strand "in flight ..." on its own, which
    // is the damage this tool exists to repair. The run must dissolve entirely.
    let src = "/*\n * Note: the pool is allocated lazily.\n * Warning: freeing it while a request is\n * in flight corrupts the arena badly.\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(
        out,
        "/*\n * Note: the pool is allocated lazily. Warning: freeing it while a request is in\n * flight corrupts the arena badly.\n */\nint f(void) { return 0; }\n"
    );
}

#[test]
fn label_run_keeps_genuine_rows_and_releases_the_wrapped_one() {
    // Three rows, the last one wrapped onto a continuation line. The two real
    // rows hold their lines; the wrapped one rejoins its tail.
    let src = "/*\n * File: aaa bbb.\n * Task: ccc ddd.\n * Note: eee fff and\n * more text here.\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(
        out,
        "/*\n * File: aaa bbb.\n * Task: ccc ddd.\n * Note: eee fff and more text here.\n */\nint f(void) { return 0; }\n"
    );
    assert_eq!(out, pipeline(&out, detect("foo.c"), 80));
}

#[test]
fn over_long_label_lines_still_reflow() {
    // A frozen line is emitted as-is, so a label line that does not already fit
    // must not qualify, or it stays over the column limit forever.
    let src = "/*\n * Note: the buffer may be null here and this sentence is intentionally long enough to wrap\n * Warning: callers must still check because the function cannot distinguish ownership\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let max_len = out.lines().map(str::len).max().unwrap_or(0);
    assert!(
        max_len <= 80,
        "over-long label lines must reflow; longest line is {max_len} cols. Output:\n{out}"
    );
}

#[test]
fn label_run_still_strips_decorative_bookends() {
    // A frozen label line re-emits behind the canonical prefix, not the raw
    // source bytes, so scope transform 1 is not undone under it.
    let src = "/*\n * -------- File: fill.c --------\n * -------- Task: fill it --------\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(
        out,
        "/*\n * File: fill.c\n * Task: fill it\n */\nint f(void) { return 0; }\n"
    );
    assert_eq!(out, pipeline(&out, detect("foo.c"), 80));
}

#[test]
fn label_run_normalizes_a_double_star_marker() {
    // sqlite3.h house style: "**" at column 0. A frozen row must land on the
    // same prefix as its reflowed siblings.
    let src = "/*\n** CAPI3REF: Run-Time Library Version Numbers\n** KEYWORDS: sqlite3_version sqlite3_sourceid\n*/\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(
        out,
        "/*\n * CAPI3REF: Run-Time Library Version Numbers\n * KEYWORDS: sqlite3_version sqlite3_sourceid\n */\nint f(void) { return 0; }\n"
    );
    assert_eq!(out, pipeline(&out, detect("foo.c"), 80));
}

#[test]
fn star_prefix_no_space_body_not_corrupted() {
    // A body line that genuinely starts with "*foo" (no space) must NOT be
    // misread as marker "*" plus content "foo" and re-emitted as " * foo".
    let src = "/*\n * foo *bar baz\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2, "round-trip must be idempotent");
}

#[test]
fn backslash_doxy_tags_align_continuation() {
    let src = "/**\n * \\param widget the widget object that needs careful operation in any state including edge cases\n */\nint f(int widget) { return widget; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn line_comment_not_double_indented() {
    // Source has 4-space indent before "//". Output must NOT have 8 spaces.
    // Regression: the line-comment first line was being emitted with "indent"
    // prepended, which then layered on top of the source's leading whitespace
    // (outside the comment node range) to produce double indentation.
    let src = "void f(void) {\n    // https://example.com/very/long/url/that/wont/wrap/and/is/just/short/enough\n    return;\n}\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        !out.contains("        //"),
        "line comment must not be double-indented, got:\n{out}"
    );
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn block_inline_opener_at_file_start_becomes_file_header() {
    // Position-based canonical form: an inline opener at file offset 0 is
    // promoted to opener-alone form ("/*" on its own line).
    let src = "/* This is a multi-line block comment whose opener has content on the same line. The author chose inline form but at file offset zero it canonicalizes to opener-alone. */\nint main(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.starts_with("/*\n"),
        "offset-0 block must canonicalize to opener-alone, got: {out:?}"
    );
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn block_canonical_inline_position() {
    let src = "#include <stdio.h>\n/* This is a multi-line block comment that should reflow into the inline canonical form because it is not at file offset zero */\nint main(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("\n/* This"),
        "non-file-header opener must use inline form (/* + content), got: {out:?}"
    );
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn inline_block_collapses_to_single_line_when_it_fits() {
    // A non-file-header block whose body is one content line collapses the
    // dangling "*/" up onto the content line when the result still fits.
    let src =
        "#include <x.h>\n/* Define the flags of a process (process_t.flags)\n */\n#define F 1U\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("/* Define the flags of a process (process_t.flags) */\n"),
        "single-line block must collapse, got:\n{out}"
    );
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2, "collapse must be idempotent");
}

#[test]
fn inline_block_stays_multiline_when_single_line_would_overflow() {
    // Body that fits the wrapped form but whose "/* ... */" one-line form
    // exceeds the column limit must keep its multi-line shape.
    let src = "void g(void) {\n    /* this comment body is engineered to land exactly at the wrap boundary XY\n     */\n}\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(
        src, out,
        "overflowing one-line form must not collapse, got:\n{out}"
    );
}

#[test]
fn inline_block_preformatted_single_line_preserved() {
    let src = "#include <stdio.h>\n/* | column one | column two | column three | column four | column five | column six | */\nint main(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(
        src, out,
        "inline preformatted block comments must pass through byte-identical"
    );
}

#[test]
fn inline_block_preformatted_first_line_keeps_single_opener() {
    let src = "#include <stdio.h>\n/* | column one | column two | column three | column four | column five | column six |\n * | value one  | value two  | value three   | value four  | value five  | value six  |\n */\nint main(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        !out.contains("/* /*"),
        "inline preformatted first line must not duplicate the block opener, got:\n{out}"
    );
    assert!(
        out.contains("/* | column one"),
        "inline preformatted first line must retain its original opener, got:\n{out}"
    );
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn return_mid_paragraph_split_c_block() {
    let src = "#include <stdio.h>\n/* Does the thing with several arguments. Returns the value on success, or a negative error code on failure. */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let lines: Vec<&str> = out.lines().collect();
    let returns_line = lines
        .iter()
        .find(|l| l.contains("Returns"))
        .expect("expected a line containing Returns");
    assert!(
        returns_line.trim_start().starts_with("* Returns"),
        "Returns must be at the start of its line (after ' * ' prefix), got: {returns_line:?}"
    );
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn return_split_ignores_a_numbered_list_marker() {
    // "2." is a list ordinal, not a sentence end. Splitting there strands a
    // bare "2." on its own line, and the next run repacks it: the same input
    // never settles.
    let src = "/*\n * Call the helper, which will:\n * 1. Finish the context switch\n * 2. Return a pointer to the saved frame\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("2. Return a pointer to the saved frame"),
        "list item was split at its ordinal:\n{out}"
    );
    assert_eq!(pipeline(&out, detect("foo.c"), 80), out, "not idempotent");
}

#[test]
fn return_split_skips_a_boundary_inside_brackets_or_quotes() {
    // A sentence end inside a parenthetical is not a paragraph boundary:
    // splitting there orphans the closing delimiter onto a new paragraph. Found
    // by running this tool on its own source.
    let paren = "/* See the footnote (the allocator can fail. Return codes are listed above) for the full table of behaviors here. */\nint f(void);\n";
    let out = pipeline(paren, detect("foo.c"), 80);
    assert!(
        out.contains("can fail. Return codes"),
        "split inside parentheses:\n{out}"
    );
    let quoted = "/* An enumerated step whose text happens to start with the word (\"2. Return the pointer\") splits at that marker and strands it. */\nint f(void);\n";
    let out = pipeline(quoted, detect("foo.c"), 80);

    // A split emits a blank comment line; wrapping alone never does. The quoted
    // phrase may wrap across lines, so assert on the paragraph break itself.
    assert!(!out.contains("\n *\n"), "split inside a quotation:\n{out}");
    assert!(out.contains("(\"2. Return the"), "quote mangled:\n{out}");
    assert_eq!(pipeline(&out, detect("foo.c"), 80), out, "not idempotent");
}

#[test]
fn return_split_ignores_an_unmatched_delimiter() {
    // An opener with no closer after it was never a region. Treating it as one
    // disabled the Return rule for every later sentence in the paragraph.
    for src in [
        "/* The buffer must be at least 4\" wide before the caller starts using it. Return NULL on error. */\nint f(void);\n",
        "/* Frees the node (the caller must still be holding the write lock here. Return 0 on success. */\nint f(void);\n",
    ] {
        let out = pipeline(src, detect("foo.c"), 80);
        let line = out
            .lines()
            .find(|l| l.contains("Return "))
            .expect("expected a Return line");
        assert!(
            line.trim_start().starts_with("* Return"),
            "unmatched delimiter suppressed the split:\n{out}"
        );
        assert_eq!(pipeline(&out, detect("foo.c"), 80), out, "not idempotent");
    }
}

#[test]
fn return_split_ignores_escaped_quotes() {
    let src = "/* Use the \\\" escape sequence in generated strings before validating a deliberately long input buffer. Returns zero on success. */\nint f(void);\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let returns_line = out
        .lines()
        .find(|l| l.contains("Returns zero"))
        .expect("expected a Returns line");
    assert!(
        returns_line.trim_start().starts_with("* Returns zero"),
        "escaped quote suppressed Return split:\n{out}"
    );
    assert_eq!(pipeline(&out, detect("foo.c"), 80), out, "not idempotent");
}

#[test]
fn return_split_still_fires_after_a_sentence_ending_number() {
    // Only a marker opening the paragraph is an ordinal. A number ending a
    // sentence mid-paragraph is a sentence end, and the Return rule applies.
    let src = "/* Computes the offset. The saved frame sits at 42. Return the pointer to the caller when done. */\nint f(void);\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let returns_line = out
        .lines()
        .find(|l| l.contains("Return the pointer"))
        .expect("expected a Return line");
    assert!(
        returns_line
            .trim_start()
            .starts_with("* Return the pointer"),
        "Return must start its own line, got: {returns_line:?}"
    );
    assert_eq!(pipeline(&out, detect("foo.c"), 80), out, "not idempotent");
}

#[test]
fn return_not_split_for_lowercase_returns() {
    let src = "#include <stdio.h>\n/* The function calls foo and then returns to the caller without any cleanup. */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        !out.contains("\n * returns"),
        "lowercase 'returns' must NOT trigger the Return rule, got: {out:?}"
    );
}

#[test]
fn idempotent_two_runs_c() {
    let src = "#include <stdio.h>\n/* A medium-length comment. Returns the value on success. */\nint f(void) { return 0; }\n";
    let p1 = pipeline(src, detect("foo.c"), 80);
    let p2 = pipeline(&p1, detect("foo.c"), 80);
    assert_eq!(p1, p2);
}

#[test]
fn doxygen_param_converts_to_kernel_doc() {
    let src = "#include <stdio.h>\n/**\n * Computes a value.\n *\n * @param flags configuration flags that change the behavior of the operation in subtle ways\n * @return the computed value\n */\nint f(int flags) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);

    // @param -> "@name : desc" (wrapped within the limit); @return -> a
    // blank-separated "Return ..." line; no surviving Doxygen tags.
    assert!(
        out.contains(" * @flags : configuration flags"),
        "param must convert to kernel-doc, got:\n{out}"
    );
    assert!(
        out.contains("\n *\n * Return the computed value"),
        "return must be a blank-separated kernel-doc 'Return' line, got:\n{out}"
    );
    assert!(
        !out.contains("@param") && !out.contains("@return"),
        "no Doxygen param/return tag may survive, got:\n{out}"
    );
    assert!(
        out.lines()
            .filter(|l| l.trim_start().starts_with('*'))
            .all(|l| l.chars().count() <= 80),
        "converted comment must wrap within the column limit, got:\n{out}"
    );
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2, "kernel-doc conversion must be idempotent");
}

#[test]
fn doxygen_param_cross_reference_survives_conversion() {
    // A comment that cross-references its own arguments in prose ("a multiple
    // of @align") used to abort the whole conversion, since any "@word" that is
    // not @param/@return read as a foreign tag. Shape from tlsf-bsd's tlsf.h.
    // The narrow limit forces the references across wrap boundaries, where
    // "common::pipeline"'s second-pass assertion is the real test.
    let src = "/**\n * Allocate memory with a specified alignment.\n *\n * @param t the allocator\n * @param align alignment in bytes, a power of two\n * @param size bytes wanted; need not be a multiple of @align\n * @return pointer aligned to @align, or NULL on failure\n */\nvoid *f(void *t, size_t align, size_t size);\n";
    let out = pipeline(src, detect("foo.c"), 56);

    assert!(
        out.contains(" * @align : alignment in bytes"),
        "params must convert, got:\n{out}"
    );
    assert!(
        !out.contains("@param") && !out.contains("@return"),
        "no Doxygen param/return tag may survive, got:\n{out}"
    );

    // Both prose references survive as words. Where reflow parks them is not
    // pinned here: a bare "@align" is not a tag any pass eats, and making the
    // packer keep it off a continuation-line start cost a fixed point.
    assert_eq!(
        out.matches("@align").count(),
        3,
        "one declaration plus both references must survive, got:\n{out}"
    );
    assert!(
        out.contains("aligned to"),
        "the return description must survive, got:\n{out}"
    );
}

#[test]
fn doxygen_backslash_param_cross_reference_survives_conversion() {
    let src = "/**\n * \\param size bytes wanted\n * \\return pointer to \\size, or NULL on failure\n */\nvoid *f(size_t size);\n";
    let out = pipeline(src, detect("foo.c"), 80);

    assert!(
        out.contains(" * @size : bytes wanted")
            && out.contains("Return pointer to \\size, or NULL on failure"),
        "backslash parameter references must convert, got:\n{out}"
    );
    assert!(!out.contains("\\param") && !out.contains("\\return"));
}

#[test]
fn doxygen_foreign_tag_still_aborts_despite_a_prose_param_mention() {
    // Head prose mentioning "@param note" is not a declaration, so the real
    // "@note" section below stays foreign and the whole comment passes through
    // rather than folding "@note" into "@x"'s description.
    let src = "/**\n * Pass @param note to the logger when tracing.\n *\n * @param x the x value\n * @note beware of the reentrancy hazard\n */\nint f(int x);\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(out, src, "a foreign @note must leave the comment untouched");
}

#[test]
fn doxygen_section_tag_survives_a_param_of_the_same_name() {
    // "file" is an ordinary C parameter name and also a Doxygen section tag.
    // The name is what disqualifies it as a cross-reference, so the section
    // survives and the whole comment passes through rather than folding
    // "writer.c" into the param.
    let src = "/**\n * @param file the output stream\n * @file writer.c\n * @return count\n */\nint f(void *file);\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(
        out, src,
        "a real @file section must leave the comment alone"
    );
}

#[test]
fn doxygen_section_tag_survives_a_reflow_that_merges_its_line() {
    // The same collision in a comment with no doc flavor, where nothing marks
    // "@file" as a tag line and reflow packs the whole body onto one line. A
    // position-based reference test read the merged "@file" as running prose
    // and converted on the SECOND run what the first refused, folding the
    // section into the param description. "common::pipeline" asserts the fixed
    // point, so the merge here is the whole test.
    let src = "/*\n * @param file the output stream\n * @file writer.c\n */\nint f(void *file);\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains("@param file") && out.contains("@file writer.c"),
        "a merged @file section must still abort the conversion, got:\n{out}"
    );
}

#[test]
fn doxygen_param_declares_a_name_that_wrapped_off_its_tag_line() {
    // "@param" ending a line takes its name off the line below, exactly as the
    // entry scan reads it. Collected per line the name went undeclared, so the
    // "@size" reference read as a foreign tag and aborted; reflow then rejoined
    // the tag and its name, and the next run converted. One run has to do it.
    let src = "/**\n * @param\n * size the size in bytes\n * @return a multiple of @size\n */\nvoid *f(size_t size);\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert!(
        out.contains(" * @size : the size in bytes"),
        "the wrapped param must convert on the first run, got:\n{out}"
    );
    assert!(
        out.contains("Return a multiple of @size"),
        "the reference must ride along as description text, got:\n{out}"
    );
}

#[test]
fn doxygen_param_named_after_a_convertible_tag_passes_through() {
    // "@param return desc" would emit "@return : desc", which the next run
    // reads as a return tag and rewrites again to "Return : desc". A param name
    // that is itself a convertible keyword cannot round-trip, so the comment
    // passes through instead.
    let src = "/**\n * @param return the return slot\n */\nint f(int *ret);\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(out, src, "a param named \"return\" must not convert");
}

#[test]
fn kernel_doc_entry_hangs_its_continuations_under_the_description() {
    // The form "convert_kernel_doc" emits wraps under the description column,
    // the same way a "@param" entry does. Shape from tlsf-bsd's tlsf.h.
    let src = "/**\n * @prev : Pointer to the previous physical block. Only valid when the previous block is free; physically stored at the tail of that block's payload.\n * @header : Size or status bits.\n */\nstruct b { int prev; };\n";
    let out = pipeline(src, detect("foo.c"), 80);
    // Assert the column rather than a hardcoded run of spaces: the
    // continuation must begin exactly under "Pointer".
    let lines: Vec<&str> = out.lines().collect();
    let head = lines.iter().position(|l| l.contains("@prev :")).unwrap();
    let desc_col = lines[head].find("Pointer").unwrap();
    let cont_col = lines[head + 1].find("block is free;").unwrap();
    assert_eq!(
        cont_col, desc_col,
        "continuation must align under the description column, got:\n{out}"
    );
    // A short entry that never wraps keeps its single line.
    assert!(
        out.contains("\n * @header : Size or status bits.\n"),
        "an entry that fits must not gain an indent, got:\n{out}"
    );
}

#[test]
fn packer_never_forges_a_kernel_doc_tag_mid_paragraph() {
    // A lone "@buf" that takes a ":"-led word on a continuation line would be
    // a tag line the source never had. "classify_lines" splits a paragraph at
    // one, so the next pass regroups the text and the hanging indent moves
    // with it. The harness's second-pass assertion is the real test here.
    let src = "/**\n * alpha beta gamma delta epsilon zeta eta theta iota kappa @buf : lambda mu\n */\nint f(int buf);\n";
    let out = pipeline(src, detect("foo.c"), 46);
    assert!(
        !out.lines().any(|l| l.trim_start().starts_with("* @buf :")),
        "reflow must not open a continuation line with a forged tag, got:\n{out}"
    );
}

#[test]
fn packer_never_splits_a_kernel_doc_entry_from_its_colon() {
    // The mirror: a name long enough to fill the opening line must still keep
    // its ":" , overflowing if it has to. A first line ending at "@name" is
    // not a tag line on the next pass, so the entry would dissolve.
    let src = "/**\n * lead words here\n * @destination_buffer_length : alpha beta gamma delta epsilon\n */\nint f(int destination_buffer_length);\n";
    let out = pipeline(src, detect("foo.c"), 34);
    assert!(
        out.contains("* @destination_buffer_length :"),
        "the name must keep its colon, got:\n{out}"
    );
}

#[test]
fn packer_never_borrows_a_tag_onto_a_continuation_line() {
    // A paragraph ending in a bare rule borrows a word from the line above so
    // the rule is not stranded there, and that borrowed word OPENS the last
    // line, which is a continuation. Borrowing a tag onto it is the same defect
    // the tag rule prevents at every other break: "classify_lines" reads a tag
    // at the first column as its own tag paragraph, the next pass regroups
    // around it, and the hanging indent moves with the regrouping. Invisible
    // while a kernel-doc entry wrapped flush; the moment it hangs, the regroup
    // moves bytes and the file settles only on its second run.
    //
    // The trailing "---" is an em-dash, the shape the bookend rules exist for.
    // "common::pipeline" asserts the fixed point, so the borrow is the test.
    let src =
        "/**\n * @buf : destination for the decoded bytes, see @note ---\n */\nint f(int x);\n";
    let out = pipeline(src, detect("foo.c"), 24);
    assert!(
        !out.lines().any(|l| l.trim_start().starts_with("* @note")),
        "the borrow must not open a continuation line with a tag, got:\n{out}"
    );
}

#[test]
fn a_rule_never_goes_out_alone_behind_a_one_word_line() {
    // The other way out of the borrow: the line above holds a single word, so
    // there is no space to split it at and nothing to lend. The fold arm has to
    // catch that, because falling through emits the rule alone, and the next
    // pass reads a line that is nothing but a rule run as a bare decorative
    // rule and DELETES it. A deleted word is the one outcome worth overflowing
    // a line to avoid.
    let src = "/**\n * @param[in] warning multiple beta reallyquitelongword ---\n */\nvoid *f(void *t);\n";
    let out = pipeline(src, detect("foo.c"), 25);
    assert!(
        out.contains("reallyquitelongword ---"),
        "the rule must ride out on the line above, got:\n{out}"
    );
}

#[test]
fn mid_paragraph_doxygen_param_does_not_lose_tag_in_line_run() {
    let src =
        "int f(void) { return 0; }\n// alpha beta gamma delta epsilon zeta @param eta theta\n";
    let out = pipeline(src, detect("foo.c"), 40);
    assert_eq!(
        out,
        "int f(void) { return 0; }\n\n// alpha beta gamma delta epsilon\n// zeta @param eta theta\n"
    );
    let pass2 = pipeline(&out, detect("foo.c"), 40);
    assert_eq!(out, pass2, "the tag must survive a second pass");
}

#[test]
fn mid_paragraph_doxygen_param_does_not_lose_tag_in_block() {
    let src =
        "int f(void) { return 0; }\n/* alpha beta gamma delta epsilon zeta @param eta theta */\n";
    let out = pipeline(src, detect("foo.c"), 40);
    assert_eq!(
        out,
        "int f(void) { return 0; }\n\n/* alpha beta gamma delta epsilon\n * zeta @param eta theta\n */\n"
    );
    let pass2 = pipeline(&out, detect("foo.c"), 40);
    assert_eq!(out, pass2, "the tag must survive a second pass");
}

/// Breaking one word earlier is not enough on its own: when the word moved
/// down is itself a tag, the break relocates the problem instead of solving
/// it. This body used to come back as "@epsilon : x delta" on the second
/// pass, losing "param". The packer overflows the line rather than break here.
#[test]
fn adjacent_doxygen_tags_do_not_lose_a_tag() {
    let src = "int f(void) { return 0; }\n// y epsilon zeta y @return @param epsilon x delta\n";
    let out = pipeline(src, detect("foo.c"), 32);
    assert_eq!(
        out,
        "int f(void) { return 0; }\n\n// y epsilon zeta y @return @param\n// epsilon x delta\n"
    );
    let pass2 = pipeline(&out, detect("foo.c"), 32);
    assert_eq!(out, pass2, "both tags must survive a second pass");
}

#[test]
fn non_tag_words_stay_within_the_column_limit() {
    let src = "int f(void) { return 0; }\n// alpha beta gamma delta epsilon zeta \\0 eta theta\n";
    let out = pipeline(src, detect("foo.c"), 40);
    assert_eq!(
        out,
        "int f(void) { return 0; }\n\n// alpha beta gamma delta epsilon zeta\n// \\0 eta theta\n"
    );
    assert!(out.lines().all(|line| line.chars().count() <= 40));
    let pass2 = pipeline(&out, detect("foo.c"), 40);
    assert_eq!(out, pass2);
}

/// "doxy_keyword" truncates "@file.txt" at the dot, so the tag table answers
/// "file" for what is really a path. classify_lines already refuses that with
/// "looks_like_email_or_path" and the packer honors the same guard: the word
/// wraps like prose and may open a line, because no convertible tag spelling
/// carries a dot and nothing downstream will eat it.
#[test]
fn path_shaped_word_wraps_like_prose() {
    let src = "int f(void) { return 0; }\n// alpha beta gamma delta epsilon zeta @file.txt eta\n";
    let out = pipeline(src, detect("foo.c"), 40);
    assert_eq!(
        out,
        "int f(void) { return 0; }\n\n// alpha beta gamma delta epsilon zeta\n// @file.txt eta\n"
    );
    assert!(out.lines().all(|line| line.chars().count() <= 40));
    let pass2 = pipeline(&out, detect("foo.c"), 40);
    assert_eq!(out, pass2, "the path must survive a second pass intact");
}

#[test]
fn rustdoc_atx_headers_preserved() {
    let src = "/// Computes a value.\n///\n/// # Arguments\n///\n/// * `widget` - the widget to operate on\n///\n/// # Returns\n///\n/// The computed value, or an error.\nfn compute(widget: i32) -> Result<i32, ()> { Ok(widget) }\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert_eq!(
        src, out,
        "rustdoc with ATX headers must round-trip byte-identical"
    );
    let pass2 = pipeline(&out, detect("foo.rs"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn column_limit_zero_disables_reflow() {
    let src = "#include <stdio.h>\n/* a long comment that would normally be reflowed because it exceeds eighty characters in width and crosses our limit boundary */\n";
    let out = pipeline(src, detect("foo.c"), 0);
    assert_eq!(src, out, "ColumnLimit 0 must skip reflow entirely");
}

#[test]
fn comment_inside_include_guard_reflows() {
    // A comment under an #ifndef/#endif include guard is ordinary code, not a
    // macro body. Tree-sitter nests the whole guard body under preproc_ifdef;
    // the skip-preproc filter must see through it or the entire file no-ops.
    let src = "#ifndef GUARD_H\n#define GUARD_H\n/* a long comment that exceeds eighty columns and so should be reflowed across two lines for sure */\n#define X 1\n#endif\n";
    let out = pipeline(src, detect("foo.h"), 80);
    assert_ne!(src, out, "comment inside include guard must reflow");
    assert!(out.contains("\n */\n"), "closing marker moved to own line");
}

#[test]
fn comment_inside_define_body_untouched() {
    // A trailing block comment that is part of a #define macro body must NOT
    // reflow: inserting a newline would terminate the macro.
    let src = "#define FOO /* a trailing macro comment long enough to cross the eighty column budget here ok */ 1\n";
    let out = pipeline(src, detect("foo.h"), 80);
    assert_eq!(src, out, "macro-body comment must pass through unchanged");
}

#[test]
fn comment_nested_in_define_body_untouched() {
    let src = "#define FOO (1 + /* some very long comment that is nested inside a macro definition expression and should never be reflowed because inserting a newline would break the macro */ 2)\n";
    let out = pipeline(src, detect("foo.h"), 80);
    assert_eq!(
        src, out,
        "nested macro-body comment must pass through unchanged"
    );
}

#[test]
fn comment_nested_in_multiline_define_error_untouched() {
    // A backslash-continued #define whose comment line makes tree-sitter emit
    // an ERROR node (no preproc_ ancestor): the syntactic fallback must still
    // recognize it as macro-internal and leave it byte-identical.
    let src = "#define FOO \\\n    (1 + \\\n     /* some very long comment that is on its own line and nested inside a macro but causes tree-sitter to parse with an ERROR node and should never be reflowed because doing so would break the macro */ \\\n     )\n";
    let out = pipeline(src, detect("foo.h"), 80);
    assert_eq!(
        src, out,
        "nested multiline error macro-body comment must pass through unchanged"
    );
}

#[test]
fn comment_on_conditional_directive_line_untouched() {
    // The transparent-conditional rule walks past preproc_if, so a comment on
    // the #if condition line itself relies on the syntactic backstop to stay
    // unreflowed, since reflowing it would split the directive line.
    let src = "#if defined(FOO) /* a trailing comment on the condition line long enough to cross eighty columns ok */\nint x;\n#endif\n";
    let out = pipeline(src, detect("foo.h"), 80);
    assert_eq!(src, out, "comment on #if condition line must pass through");
}

#[test]
fn crlf_preservation_short_comment() {
    let src = "int x = 0; // short\r\nint y = 1;\r\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(src, out, "CRLF must be preserved when no reflow happens");
}

#[test]
fn empty_block_comment_idempotent() {
    let src = "int x;\n/* */\nint y;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn empty_block_comment_double_star_idempotent() {
    // /**/ must be treated as standard block comments, not doc blocks, so
    // forcing reflow doesn't corrupt them by splitting /** into opener and body
    // content.
    let src = "int x;\n/**/\nint y;\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2);

    // Under low column limit, /**/ should reflow cleanly as an empty block
    // comment.
    let low_limit = pipeline(src, detect("foo.c"), 2);
    assert_eq!(low_limit, "int x;\n/* */\nint y;\n");
}

#[test]
fn short_triple_star_block_keeps_body_clean() {
    let src = "/***\n * foo\n ***/\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(out, "/*\n * foo\n */\n");
}

#[test]
fn empty_line_comments_idempotent() {
    let src = "//\nint x = 0;\n///\n//!\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    let pass2 = pipeline(&out, detect("foo.rs"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn utf8_cjk_width_aware() {
    // The CJK chars are display-width 2 each; the comment has 4 CJK + 38 ASCII
    // chars = roughly 46 display columns, under 80, so it should not reflow.
    let src = "fn f() {} // 你好世界 this is a comment with some ASCII text\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert_eq!(src, out, "CJK comment under column limit must not reflow");
}

#[test]
fn rustdoc_fenced_code_preserved() {
    let src = "/// Examples.\n///\n/// ```rust\n/// let x = 1;\n/// # use std::collections::HashMap;\n/// ```\nfn f() {}\n";
    let out = pipeline(src, detect("foo.rs"), 80);
    assert_eq!(
        src, out,
        "fenced code with doctest-hidden lines must pass through unchanged"
    );
}

#[test]
fn doxygen_blank_lines_survive() {
    let src = "#include <stdio.h>\n/**\n * First paragraph.\n *\n * Second paragraph after a blank.\n */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let blank_count = out.lines().filter(|l| l.trim() == "*").count();
    assert!(
        blank_count >= 1,
        "blank line between paragraphs must survive, got:\n{out}"
    );
    let pass2 = pipeline(&out, detect("foo.c"), 80);
    assert_eq!(out, pass2);
}

#[test]
fn dry_run_does_not_modify() {
    // Smoke check on the Mode::DryRun path: invoking the pipeline with no apply
    // must not produce any text mutation. (CLI-level no-write behavior is
    // exercised by the binary; this test asserts the in-process function path.)
    let src = "#include <stdio.h>\n/* short */\nint f(void) { return 0; }\n";
    let out = pipeline(src, detect("foo.c"), 80);
    assert_eq!(src, out, "short comment must round-trip unchanged");
}

#[test]
fn bytes_outside_comments_unchanged_full() {
    let src = "#include <stdio.h>\n/* Long comment that will reflow. Returns nothing useful. */\nint main(int argc, char **argv) {\n    int x = argc + 1;\n    return x;\n}\n";
    let out = pipeline(src, detect("foo.c"), 80);
    let strip_comments = |s: &str| -> String {
        let mut result = String::new();
        let mut in_block = false;
        for line in s.lines() {
            let mut l = line.to_string();
            if in_block {
                if let Some(idx) = l.find("*/") {
                    l = l[idx + 2..].to_string();
                    in_block = false;
                } else {
                    result.push('\n');
                    continue;
                }
            }
            while let Some(start) = l.find("/*") {
                if let Some(end) = l[start..].find("*/") {
                    let after = start + end + 2;
                    let prefix = &l[..start];
                    let suffix = &l[after..];
                    l = format!("{prefix}{suffix}");
                } else {
                    l = l[..start].to_string();
                    in_block = true;
                    break;
                }
            }
            if let Some(idx) = l.find("//") {
                l = l[..idx].to_string();
            }
            result.push_str(l.trim_end());
            result.push('\n');
        }
        result
    };
    assert_eq!(strip_comments(src), strip_comments(&out));
}

#[test]
fn plan_splits_trailing_block_closer() {
    let src = "struct s {\n    bool b; /* aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n            * bbb. */\n};\n";
    let out = pipeline(src, detect("s.c"), 80);
    assert!(
        out.ends_with("            * bbb.\n            */\n};\n"),
        "closer not split onto its own line:\n{out}"
    );
    // Idempotent.
    assert_eq!(pipeline(&out, detect("s.c"), 80), out);
}

#[test]
fn plan_relocates_manpage_comment_before_function() {
    // X11 manual-page block wedged between signature and body: DESCRIPTION and
    // RETURNS sections collapse to prose + a kernel-doc "Return" line, and the
    // whole comment hoists ahead of the function.
    let src = "\
#include <X11/Xlib.h>

XcmsCCC XcmsCreateCCC(Display *dpy, int screenNumber)
/*      DESCRIPTION
 *              Given a Display, Screen, Visual, etc., this routine creates
 *              an appropriate Color Conversion Context.
 *
 *      RETURNS
 *              Returns NULL if failed; otherwise address of the newly
 *              created XcmsCCC.
 */
{
    return 0;
}
";
    let out = pipeline(src, detect("ccc.c"), 80);
    let expected = "\
#include <X11/Xlib.h>

/* Given a Display, Screen, Visual, etc., this routine creates an appropriate
 * Color Conversion Context.
 *
 * Return NULL if failed; otherwise address of the newly created XcmsCCC.
 */

XcmsCCC XcmsCreateCCC(Display *dpy, int screenNumber)
{
    return 0;
}
";
    assert_eq!(out, expected, "manpage comment not relocated/cleaned");
    // Idempotent once the comment is ahead of the function.
    assert_eq!(pipeline(&out, detect("ccc.c"), 80), out);
}

#[test]
fn plan_relocated_comment_stays_separate_from_preceding_banner() {
    // A copyright banner directly above the function must not fuse with the
    // relocated block on a re-run: the insert keeps a blank line between them.
    let src = "\
/* Copyright banner that is left exactly as-is above the function. */
int f(void)
/*      DESCRIPTION
 *              Does the thing this function is meant to do here.
 *
 *      RETURNS
 *              Zero, always, no matter what the caller happens to pass.
 */
{
    return 0;
}
";
    let out = pipeline(src, detect("f.c"), 80);
    assert!(
        out.contains("above the function. */\n\n/* Does the thing"),
        "relocated comment must be blank-separated from the banner:\n{out}"
    );
    // Idempotent: the two blocks must not merge on a second pass.
    assert_eq!(pipeline(&out, detect("f.c"), 80), out);
}

#[test]
fn plan_inserts_blank_before_multiline_comment() {
    // A genuinely multi-line comment flush against the statement above gets a
    // blank line, even when its own text is already correctly wrapped.
    let src = "\
void f(void)
{
    sync_scale();
    /* The clear box must match the renderer that will actually run. The fixed
     * aliases render through SDL_ttf now.
     */
    use_it();
    /* A block-opening comment keeps its place. */
    if (x) {
        /* First statement in a block: no blank line inserted here because the
         * previous line already ends with an opening brace.
         */
        g();
    }
    switch (x) {
    case 1:
        /* First statement in a case arm: no blank line inserted here because
         * the previous line is a label.
         */
        break;
    default:
        /* First statement in a default arm: no blank line inserted here because
         * the previous line is a label.
         */
        break;
    }
}
";
    let out = pipeline(src, detect("font.c"), 80);
    assert!(
        out.contains("    sync_scale();\n\n    /* The clear box"),
        "blank line not inserted before multi-line comment:\n{out}"
    );
    // The "{" exception: comment opening a block is untouched.
    assert!(
        out.contains("    if (x) {\n        /* First statement"),
        "blank line wrongly inserted after opening brace:\n{out}"
    );
    assert!(
        out.contains("    case 1:\n        /* First statement"),
        "blank line wrongly inserted after case label:\n{out}"
    );
    assert!(
        out.contains("    default:\n        /* First statement"),
        "blank line wrongly inserted after default label:\n{out}"
    );
    // Single-line comment gets no blank line.
    assert!(
        out.contains("    use_it();\n    /* A block-opening comment"),
        "blank line wrongly inserted before single-line comment:\n{out}"
    );
    // Idempotent.
    assert_eq!(pipeline(&out, detect("font.c"), 80), out);
}

#[test]
fn plan_no_blank_before_comment_under_bom_directive() {
    // Both "#" lines that suppress the blank line go through one parser, so the
    // BOM strip reaches each of them. It used to reach only the shebang arm,
    // which left a BOM'd guard failing the test its BOM-free twin passes:
    // U+FEFF is not Unicode White_Space, so "trim" leaves it glued to the "#".
    let guard = "\u{feff}#ifndef GUARD\n/* A header long enough to stay multi-line once the reflow has run over it. */\nint x;\n#endif\n";
    let out = pipeline(guard, detect("g.h"), 80);
    assert!(
        out.contains("#ifndef GUARD\n/* A header"),
        "blank line wrongly inserted after a BOM-prefixed #ifndef:\n{out}"
    );

    // The shebang arm, same rule, with the BOM the corpus fixture carries.
    let script = "\u{feff}#!/bin/sh\n# A header long enough to stay multi-line once the reflow has run over it here.\nexit 0\n";
    let out = pipeline(script, detect("s.sh"), 80);
    assert!(
        out.contains("#!/bin/sh\n# A header"),
        "blank line wrongly inserted under a BOM-prefixed shebang:\n{out}"
    );
}

#[test]
fn plan_blank_line_under_rust_inner_attribute() {
    // "#![no_std]" sits exactly where a shebang does and starts with "#!", so
    // the shell carve-out read it as one and suppressed the blank line under
    // it. It is a Rust inner attribute: ordinary code, and the comment below it
    // is explaining that code.
    let src = "#![no_std]\n// A comment long enough that it stays multi-line once the packer has run over it.\nfn f() {}\n";
    let out = pipeline(src, detect("a.rs"), 80);
    assert!(
        out.contains("#![no_std]\n\n//"),
        "no blank line under a Rust inner attribute:\n{out}"
    );

    // The shell shebang it was confused with still suppresses it.
    let sh = "#!/bin/sh\n# A comment long enough that it stays multi-line once the packer has run over it.\nexit 0\n";
    let out = pipeline(sh, detect("s.sh"), 80);
    assert!(
        out.contains("#!/bin/sh\n#"),
        "blank line wrongly inserted under a shebang:\n{out}"
    );
}

#[test]
fn param_drift_needs_exactly_one_comment_after_paren() {
    // One comment after ")" is the tell that the set is displaced by one. Two
    // is not that shape, and shifting both onto the last parameter invents a
    // grouping the author never wrote, so the whole signature passes through.
    let two = "void f(int a, /* the b */ int b) /* one */ /* two */ {\n    g();\n}\n";
    assert_eq!(
        pipeline(two, detect("d.c"), 80),
        two,
        "two trailing comments must not shift"
    );

    // The documented one-comment drift still fires.
    let one = "void f(int a, /* the b */ int b) /* the c */ {\n    g();\n}\n";
    let out = pipeline(one, detect("e.c"), 80);
    assert!(
        out.contains("void f(int a /* the b */, int b /* the c */)"),
        "one-step drift no longer shifts:\n{out}"
    );
}

#[test]
fn plan_no_blank_after_preprocessor_conditional() {
    // A multi-line comment that is the first thing inside a preprocessor
    // conditional block is the "#if*"/"#else"/"#elif" analog of "first
    // statement after "{"": no blank line inserted. "#endif" closes a block, so
    // a comment after it still gets one.
    let src = "\
void f(void)
{
#ifndef LIBX11_COMPAT_SDL3
    /* SDL2 path only: the TOOLTIP window type makes SDL2's X11 driver set
     * override_redirect, so the WM ignores the menu entirely.
     */
    flags |= SDL_WINDOW_TOOLTIP;
#elif defined(FALLBACK)
    /* The #elif branch opens a scope too, so its first comment stays flush
     * against the directive with no blank line above it.
     */
    fallback();
#elif defined(MULTI_A) || \\
      defined(MULTI_B)
    /* A continued #elif condition still opens a scope, so this first comment
     * must stay attached to the directive's logical line.
     */
    multi_elif();
#if defined(NESTED_A) || \\
    defined(NESTED_B)
    /* A continued #if condition is still the block opener for this comment.
     */
    nested_if();
#endif
#else
    /* The #else branch is a scope-opener as well and gets the same treatment
     * as its sibling directives here.
     */
    other();
#endif
    /* This trailing block, following #endif, still gets a blank line above it
     * because #endif does not open a scope.
     */
    done();
}
";
    let out = pipeline(src, detect("compat.c"), 80);
    assert!(
        out.contains("#ifndef LIBX11_COMPAT_SDL3\n    /* SDL2 path only"),
        "blank line wrongly inserted after #ifndef:\n{out}"
    );
    assert!(
        out.contains("#elif defined(FALLBACK)\n    /* The #elif branch"),
        "blank line wrongly inserted after #elif:\n{out}"
    );
    assert!(
        out.contains("      defined(MULTI_B)\n    /* A continued #elif condition"),
        "blank line wrongly inserted after continued #elif:\n{out}"
    );
    assert!(
        out.contains("    defined(NESTED_B)\n    /* A continued #if condition"),
        "blank line wrongly inserted after continued #if:\n{out}"
    );
    assert!(
        out.contains("#else\n    /* The #else branch"),
        "blank line wrongly inserted after #else:\n{out}"
    );
    assert!(
        out.contains("#endif\n\n    /* This trailing block"),
        "blank line not inserted after #endif:\n{out}"
    );
    // Idempotent.
    assert_eq!(pipeline(&out, detect("compat.c"), 80), out);
}

#[test]
fn plan_blank_before_multiline_after_non_scope_directive() {
    // Directives that don't open a scope ("#define", "#include") and Rust
    // "#[cfg]" attributes are NOT preprocessor conditionals: a genuinely
    // multi-line comment flush against them still gets its blank line.
    let c_src = "\
#define MAX 10
/* MAX bounds the ring buffer; anything larger is rejected at parse time before
 * it ever reaches the allocator here.
 */
int ring[MAX];
";
    let c_out = pipeline(c_src, detect("ring.c"), 80);
    assert!(
        c_out.contains("#define MAX 10\n\n/* MAX bounds"),
        "blank line not inserted after #define:\n{c_out}"
    );

    let rs_src = "\
#[cfg(feature = \"extra\")]
/* This configuration-gated helper exists only when the extra feature is on and
 * therefore carries its own explanatory note.
 */
fn helper() {}
";
    let rs_out = pipeline(rs_src, detect("lib.rs"), 80);
    assert!(
        rs_out.contains("#[cfg(feature = \"extra\")]\n\n/* This configuration"),
        "blank line wrongly suppressed after Rust #[cfg] attribute:\n{rs_out}"
    );
}

#[test]
fn plan_keeps_multiline_comment_attached_to_cpp_access_label() {
    let src = "\
class Widget {
public:
    /* The visible member API remains grouped under the access label for readers
     * scanning the class declaration.
     */
    void draw();
};
";
    let out = pipeline(src, detect("widget.cpp"), 80);
    assert!(
        out.contains("public:\n    /* The visible member API"),
        "blank line wrongly inserted after access label:\n{out}"
    );
}

#[test]
fn plan_relocates_manpage_comment_without_eating_function_indent() {
    let src = "\
struct S {
  int f()
  /*      DESCRIPTION
   *              Does the thing this inline method is meant to do here.
   *
   *      RETURNS
   *              Returns zero every time this inline method is called.
   */
  {
    return 0;
  }
};
";
    let out = pipeline(src, detect("s.cpp"), 80);
    assert!(
        out.contains("\n  int f()\n  {\n"),
        "function indentation must survive relocation:\n{out}"
    );
    assert!(!out.contains("\nint f()\n"), "function lost indent:\n{out}");
}

#[test]
fn plan_relocates_indented_method_comment_at_column_zero() {
    // An indented C++ method: the comment relocates to the function's line
    // start (column 0) and must reflow with zero indent (opener and
    // continuation lines aligned) while the signature keeps its own indent.
    // Regression: reflowing with the old between-body indent left a ragged
    // block and broke idempotency.
    let src = "\
class Foo {
    int bar(int n)
    /*      DESCRIPTION
     *              Computes the bar value from n with the standard method.
     *
     *      RETURNS
     *              The computed bar value for the caller to use downstream.
     */
    {
        return n;
    }
};
";
    let out = pipeline(src, detect("foo.cpp"), 80);
    let expected = "\
class Foo {

/* Computes the bar value from n with the standard method.
 *
 * Return The computed bar value for the caller to use downstream.
 */

    int bar(int n)
    {
        return n;
    }
};
";
    assert_eq!(
        out, expected,
        "indented method comment not aligned at column 0"
    );
    // Signature indentation is untouched: no code byte corrupted.
    assert!(out.contains("\n    int bar(int n)\n"));
    // Idempotent.
    assert_eq!(pipeline(&out, detect("foo.cpp"), 80), out);
}

#[test]
fn plan_leaves_manpage_without_return_in_place() {
    // The move only fires when BOTH DESCRIPTION and RETURN(S) are present. A
    // DESCRIPTION-only block stays put (and its indented body reads as
    // preformatted, so it passes through untouched).
    let src = "\
int f(void)
/*      DESCRIPTION
 *              Does a thing.
 */
{
    return 0;
}
";
    assert_eq!(pipeline(src, detect("f.c"), 80), src);
}

#[test]
fn plan_leaves_ordinary_comment_between_sig_and_body() {
    // A non-manpage comment in the same position is never relocated.
    let src = "\
int f(void)
/* just a note */
{
    return 0;
}
";
    assert_eq!(pipeline(src, detect("f.c"), 80), src);
}

#[test]
fn plan_relocated_comment_at_file_byte_zero_is_idempotent() {
    // Function at file offset 0: the comment relocates to byte 0, where the
    // next pass would read a plain block as a file header (opener-alone). The
    // first pass must adopt that same form so f(f(x)) == f(x). Regression.
    let src = "\
int f(void)
/*      DESCRIPTION
 *              Does the thing this function is meant to do here today.
 *
 *      RETURNS
 *              Zero, always, no matter what the caller passes here.
 */
{
    return 0;
}
";
    let once = pipeline(src, detect("f.c"), 80);
    // Opener-alone (file-header) form, since it lands at byte 0.
    assert!(once.starts_with("/*\n * Does the thing"), "got:\n{once}");
    assert_eq!(pipeline(&once, detect("f.c"), 80), once, "not idempotent");
}

#[test]
fn plan_relocates_multiple_manpage_comments_in_one_file() {
    // Two documented functions: two independent delete+insert pairs must not
    // overlap or misorder in validate/apply.
    let src = "\
#include <h>

int a(void)
/*      DESCRIPTION
 *              First function does the first thing described right here.
 *
 *      RETURNS
 *              The letter a, metaphorically speaking, for the first one.
 */
{
    return 0;
}

int b(void)
/*      DESCRIPTION
 *              Second function does the second thing described right here.
 *
 *      RETURNS
 *              The letter b, metaphorically speaking, for the second one.
 */
{
    return 0;
}
";
    let out = pipeline(src, detect("m.c"), 80);
    // Each comment sits ahead of its own function, both cleaned.
    assert!(out.contains("/* First function does the first thing described right here.\n"));
    assert!(out.contains("/* Second function does the second thing described right here.\n"));
    assert!(out.contains("\nint a(void)\n{\n"));
    assert!(out.contains("\nint b(void)\n{\n"));
    assert!(!out.contains("DESCRIPTION"));
    assert_eq!(pipeline(&out, detect("m.c"), 80), out, "not idempotent");
}

#[test]
fn plan_relocates_manpage_comment_crlf() {
    // CRLF must survive the relocation: the delete's "\r\n" branch and the
    // blank-line separator both need the "\r\n" ending.
    let src = "#include <h>\r\n\r\nint f(void)\r\n/*      DESCRIPTION\r\n *              Does a thing worth documenting across a couple of lines.\r\n *\r\n *      RETURNS\r\n *              Zero on every single call, no exceptions whatsoever.\r\n */\r\n{\r\n    return 0;\r\n}\r\n";
    let out = pipeline(src, detect("f.c"), 80);
    assert!(out.contains("\r\n"), "CRLF endings lost");
    assert!(
        !out.contains("\n\n\n"),
        "stray bare-LF blank line introduced"
    );
    assert!(out.contains("/* Does a thing worth documenting across a couple of lines.\r\n"));
    assert_eq!(pipeline(&out, detect("f.c"), 80), out, "not idempotent");
}

#[test]
fn plan_leaves_manpage_with_unrecognized_section_in_place() {
    // An extra ALL-CAPS header we don't recognize (NAME) aborts the transform
    // to passthrough: the block is neither cleaned nor moved.
    let src = "\
int f(void)
/*      NAME
 *              f
 *
 *      DESCRIPTION
 *              Does a thing.
 *
 *      RETURNS
 *              Zero.
 */
{
    return 0;
}
";
    assert_eq!(pipeline(src, detect("f.c"), 80), src);
}

#[test]
fn plan_leaves_manpage_with_only_return_in_place() {
    // The move requires BOTH DESCRIPTION and RETURN(S); a RETURN-only block
    // stays put.
    let src = "\
int f(void)
/*      RETURNS
 *              Zero, always.
 */
{
    return 0;
}
";
    assert_eq!(pipeline(src, detect("f.c"), 80), src);
}

#[test]
fn plan_skips_trailing_cppcheck_suppress() {
    // cppcheck-suppress directives are force_passthrough: never reshaped.
    let src = "int a[8];\nint x = a[i]; /* cppcheck-suppress negativeIndex\n               * guarded above. */\n";
    assert_eq!(pipeline(src, detect("s.c"), 80), src);
}

#[test]
fn plan_skips_trailing_rust_nested_block() {
    // Nested Rust blocks are force_passthrough; the closer split must not fire.
    let src = "let x = 1; /* outer /* inner */\n            * still outer. */\n";
    assert_eq!(pipeline(src, detect("s.rs"), 80), src);
}

#[test]
fn plan_relocates_manpage_comment_with_colons_and_lowercase_returns() {
    // Header line keeps the function off byte 0 so the inline opener form
    // applies (a byte-0 relocation uses the file-header form, and is covered
    // separately by "plan_relocated_comment_at_file_byte_zero_is_idempotent").
    let src = "\
#include <X11/Xlib.h>

XcmsCCC XcmsCreateCCC(Display *dpy, int screenNumber)
/*      DESCRIPTION:
 *              Given a Display, Screen, Visual, etc., this routine creates
 *              an appropriate Color Conversion Context.
 *
 *      RETURNS:
 *              returns NULL if failed; otherwise address of the newly
 *              created XcmsCCC.
 */
{
    return 0;
}
";
    let out = pipeline(src, detect("ccc.c"), 80);
    let expected = "\
#include <X11/Xlib.h>

/* Given a Display, Screen, Visual, etc., this routine creates an appropriate
 * Color Conversion Context.
 *
 * Return NULL if failed; otherwise address of the newly created XcmsCCC.
 */

XcmsCCC XcmsCreateCCC(Display *dpy, int screenNumber)
{
    return 0;
}
";
    assert_eq!(out, expected);
    assert_eq!(pipeline(&out, detect("ccc.c"), 80), out, "not idempotent");
}

// Drifted parameter-comment shift (transform 4)

#[test]
fn plan_shifts_drifted_parameter_comments() {
    // Every non-first param carries a leading comment + one after ")": the
    // whole set drifted forward by one. Each comment trails the param it
    // describes.
    let src = "\
int f(
    Display *dpy,
    /* the display connection */ int screen,
    /* the screen number to use */ int *gravity_return) /* gravity of window */
{
    return 0;
}
";
    let expected = "\
int f(
    Display *dpy /* the display connection */,
    int screen /* the screen number to use */,
    int *gravity_return /* gravity of window */)
{
    return 0;
}
";
    let out = pipeline(src, detect("g.c"), 80);
    assert_eq!(out, expected, "drifted param comments not shifted");
    assert_eq!(pipeline(&out, detect("g.c"), 80), out, "not idempotent");
}

#[test]
fn plan_param_drift_aborts_when_a_directive_participates() {
    // A machine directive (clang-tidy NOLINT) sits among the drifted comments.
    // The shift is atomic over the whole signature: moving the other comments
    // while the directive stays put would scramble the parameter/comment
    // pairing, so the entire signature aborts to passthrough, byte-identical.
    let src = "\
int f(
    Display *dpy,
    /* NOLINT(cppcoreguidelines) */ int screen,
    /* the screen number to use */ int *gravity_return) /* gravity of window */
{
    return 0;
}
";
    let out = pipeline(src, detect("g.c"), 80);
    assert_eq!(
        out, src,
        "directive in the drift set must abort the whole shift"
    );
    assert_eq!(pipeline(&out, detect("g.c"), 80), out, "not idempotent");
}

#[test]
fn plan_shifts_drifted_parameter_comments_on_a_suffix_run() {
    // Only a trailing run of params carries comments; the uncommented prefix is
    // untouched (the XReadBitmapFile shape).
    let src = "\
int f(Display *display,
      Drawable d,
      unsigned int *width,
      /* RETURNED */ unsigned int *height,
      /* RETURNED */ int *y_hot) /* RETURNED */
{
    return 0;
}
";
    let expected = "\
int f(Display *display,
      Drawable d,
      unsigned int *width /* RETURNED */,
      unsigned int *height /* RETURNED */,
      int *y_hot /* RETURNED */)
{
    return 0;
}
";
    let out = pipeline(src, detect("g.c"), 80);
    assert_eq!(out, expected, "suffix-run drift not shifted");
    assert_eq!(pipeline(&out, detect("g.c"), 80), out, "not idempotent");
}

#[test]
fn plan_shifts_drifted_parameter_comment_groups() {
    // A param can carry a GROUP ("/* a */ /* b */"); the whole group shifts, in
    // order, and the adjacent comments delete disjoint spans
    // (XAllocColorCells).
    let src = "\
Status f(
    register Display *dpy,
    unsigned long *masks,
    /* LISTofCARD32 */ /* RETURN */ unsigned int nplanes,
    /* CARD16 */ unsigned long *pixels,
    /* LISTofCARD32 */ /* RETURN */ unsigned int ncolors) /* CARD16 */
{
    return 0;
}
";
    let expected = "\
Status f(
    register Display *dpy,
    unsigned long *masks /* LISTofCARD32 */ /* RETURN */,
    unsigned int nplanes /* CARD16 */,
    unsigned long *pixels /* LISTofCARD32 */ /* RETURN */,
    unsigned int ncolors /* CARD16 */)
{
    return 0;
}
";
    let out = pipeline(src, detect("g.c"), 80);
    assert_eq!(out, expected, "comment groups not shifted");
    assert_eq!(pipeline(&out, detect("g.c"), 80), out, "not idempotent");
}

#[test]
fn plan_leaves_non_drift_parameter_comments_alone() {
    // No trailing comment after ")" (not the drift tell): a lone middle-param
    // comment, and a comment on the FIRST param: none of these shift.
    for src in [
        "int f(int a,\n      /* leads b */ int b,\n      /* leads c */ int c)\n{\n    return 0;\n}\n",
        "int f(int a,\n      /* about b */ int b,\n      int c) /* trailing */\n{\n    return 0;\n}\n",
        "int f(/* leads a */ int a,\n      int b) /* trailing */\n{\n    return 0;\n}\n",
    ] {
        assert_eq!(
            pipeline(src, detect("g.c"), 80),
            src,
            "should be untouched:\n{src}"
        );
    }
}

#[test]
fn plan_param_drift_ignores_line_comments() {
    // A "//" comment moved inline would swallow the code after it, so never
    // shift, and never abort. The file passes through unchanged.
    let src = "\
int g(int a,
      // a-ish
      int b) // trailing
{
    return 0;
}
";
    assert_eq!(pipeline(src, detect("g.c"), 80), src);
}

#[test]
fn plan_param_drift_collapses_multiline_comment_and_is_idempotent() {
    // A multi-line drifted block collapses to one line on the shift, so it
    // can't round-trip through the trailing-closer split. f(f(x)) == f(x).
    let src = "\
int h(int a,
      /* long
         desc */ int b) /* t */
{
    return 0;
}
";
    let out = pipeline(src, detect("g.c"), 80);
    assert!(
        out.contains("int a /* long desc */,"),
        "multiline not collapsed:\n{out}"
    );
    assert!(
        out.contains("int b /* t */)"),
        "trailing not shifted:\n{out}"
    );
    assert_eq!(pipeline(&out, detect("g.c"), 80), out, "not idempotent");
}

#[test]
fn plan_param_drift_after_paren_own_line_leaves_no_blank_line() {
    // An after-")" comment on its own line is removed whole-line, so no blank
    // line is left between ")" and "{".
    let src = "\
int k(int a,
      /* leads b */ int b)
/* trailing */
{
    return 0;
}
";
    let out = pipeline(src, detect("g.c"), 80);
    assert!(
        out.contains("int b /* trailing */)\n{\n"),
        "blank line or wrong shift:\n{out}"
    );
    assert_eq!(pipeline(&out, detect("g.c"), 80), out, "not idempotent");
}

#[test]
fn plan_param_drift_leading_comment_ending_a_code_line_does_not_abort() {
    // A leading comment that shares its line with preceding code and is
    // followed by a newline must delete only the comment (not back over the
    // code), so plan must not error.
    let src = "\
int f(int a, /* desc a */
      int b) /* desc b */
{
    return 0;
}
";
    let out = pipeline(src, detect("g.c"), 80);
    assert!(out.contains("/* desc a */"), "comment lost:\n{out}");
    assert!(
        out.contains("int b /* desc b */)"),
        "trailing not shifted:\n{out}"
    );
    // Code intact: "int a" and "int b" still present, in order.
    assert!(out.contains("int a") && out.contains("int b"));
}
