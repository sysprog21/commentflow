// Integration tests for the stdin ("-") filter mode and "--files-from". All
// drive the real binary, so argument parsing, "Args::validate", and the stdin
// plumbing are exercised end to end rather than through the parser alone.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_commentflow")
}

fn fresh_tmp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("commentflow-{tag}-{}-{nanos}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run the binary with the given args, feeding "input" on stdin. Returns
/// (exit code, stdout, stderr).
fn run(args: &[&str], input: &[u8]) -> (Option<i32>, String, String) {
    let mut child = Command::new(bin())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");

    // A run that rejects its arguments exits before it ever reads stdin, so
    // this write races that exit and loses whenever the child wins: EPIPE. That
    // is the binary behaving correctly, and the assertions that follow are
    // about the exit code and stderr, which wait_with_output still collects.
    // Only a real I/O failure is worth failing a test over.
    let mut stdin = child.stdin.take().unwrap();
    if let Err(e) = stdin.write_all(input)
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        panic!("write to child stdin: {e}");
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const LONG: &str =
    "comment that is plainly long enough to wrap when the column limit is forced down to forty";

#[test]
fn stdin_filter_reflows_each_language() {
    let cases = [
        ("c", format!("// {LONG}\nint x = 0;\n")),
        ("cpp", format!("// {LONG}\nint x = 0;\n")),
        ("rust", format!("// {LONG}\nfn f() {{}}\n")),
        ("shell", format!("# {LONG}\nx=0\n")),
    ];
    for (lang, src) in cases {
        let (code, stdout, stderr) = run(
            &["-", "--lang", lang, "--column-limit", "40"],
            src.as_bytes(),
        );
        assert_eq!(code, Some(0), "{lang}: exit 0; stderr={stderr}");
        assert!(
            stdout.lines().count() > src.lines().count(),
            "{lang}: comment must wrap, got:\n{stdout}"
        );
        // Out-of-comment code line survives verbatim.
        let code_line = src.lines().last().unwrap();
        assert!(
            stdout.contains(code_line),
            "{lang}: code line must be untouched, got:\n{stdout}"
        );
    }
}

#[test]
fn stdin_check_exits_1_and_writes_nothing() {
    let src = format!("// {LONG}\nint x = 0;\n");
    let (code, stdout, _) = run(
        &["--check", "-", "--lang", "c", "--column-limit", "40"],
        src.as_bytes(),
    );
    assert_eq!(code, Some(1), "would-change must exit 1");
    assert!(stdout.is_empty(), "--check must not emit the source");
}

#[test]
fn stdin_diff_uses_stdin_label() {
    let src = format!("// {LONG}\nint x = 0;\n");
    let (code, stdout, _) = run(
        &["--diff", "-", "--lang", "c", "--column-limit", "40"],
        src.as_bytes(),
    );
    assert_eq!(code, Some(0));
    assert!(stdout.contains("<stdin>"), "diff header must label <stdin>");
}

#[test]
fn stdin_without_lang_errors() {
    let (code, _, stderr) = run(&["-"], b"// hi\n");
    assert_eq!(code, Some(2), "missing --lang must exit 2");
    assert!(stderr.contains("--lang"), "error must mention --lang");
}

#[test]
fn stdin_mixed_with_paths_errors() {
    // foo.c need not exist: validation fires before any file is opened.
    let (code, _, stderr) = run(&["-", "foo.c", "--lang", "c"], b"// hi\n");
    assert_eq!(code, Some(2), "a '-' mixed with paths must exit 2");
    assert!(stderr.contains('-'), "error must reference the '-' source");
}

#[test]
fn files_from_file_backed_reflows_in_place() {
    let tmp = fresh_tmp("ff-file");
    let a = tmp.join("a.c");
    let b = tmp.join("b.rs");
    let orig = format!("// {LONG}\nint x = 0;\n");
    std::fs::write(&a, &orig).unwrap();
    std::fs::write(&b, format!("// {LONG}\nfn f() {{}}\n")).unwrap();
    let list = tmp.join("list.txt");
    // Blank line in the middle must be skipped.
    std::fs::write(&list, format!("{}\n\n{}\n", a.display(), b.display())).unwrap();

    let (code, _, stderr) = run(
        &[
            "--files-from",
            list.to_str().unwrap(),
            "--column-limit",
            "40",
        ],
        b"",
    );
    assert_eq!(code, Some(0), "stderr={stderr}");
    assert!(
        std::fs::read_to_string(&a).unwrap().lines().count() > orig.lines().count(),
        "a.c must be reflowed in place"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn files_from_nul_delimited_and_stdin() {
    let tmp = fresh_tmp("ff-nul");
    let a = tmp.join("a.c");
    let orig = format!("// {LONG}\nint x = 0;\n");
    std::fs::write(&a, &orig).unwrap();
    // NUL-delimited list fed on stdin via "--files-from -".
    let list = format!("{}\0", a.display());
    let (code, _, stderr) = run(
        &["--files-from", "-", "--column-limit", "40"],
        list.as_bytes(),
    );
    assert_eq!(code, Some(0), "stderr={stderr}");
    assert!(
        std::fs::read_to_string(&a).unwrap().lines().count() > orig.lines().count(),
        "a.c must be reflowed via NUL stdin list"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

// Unix only because the fixture cannot exist elsewhere: Win32 rejects '\n' in a
// filename outright (ERROR_INVALID_NAME), so there is no such path to feed the
// list reader. The reader itself is platform-independent; this is the only
// platform that can hold the input that proves it.
#[cfg(unix)]
#[test]
fn files_from_nul_path_with_embedded_newline() {
    // A NUL-delimited list must NOT also split on '\n': a path containing a
    // newline (legal on Unix, exactly why -print0 exists) stays one entry.
    let tmp = fresh_tmp("ff-nul-nl");
    let weird = tmp.join("a\nb.c");
    let orig = format!("// {LONG}\nint x = 0;\n");
    std::fs::write(&weird, &orig).unwrap();
    let list = format!("{}\0", weird.display());
    let (code, _, stderr) = run(
        &["--files-from", "-", "--column-limit", "40"],
        list.as_bytes(),
    );
    assert_eq!(
        code,
        Some(0),
        "embedded-newline path must resolve; stderr={stderr}"
    );
    assert!(
        std::fs::read_to_string(&weird).unwrap().lines().count() > orig.lines().count(),
        "the embedded-newline path must be reflowed, not shredded into two"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn files_from_empty_list_exits_0() {
    let (code, _, _) = run(&["--files-from", "-"], b"\n\n");
    assert_eq!(code, Some(0), "empty list is success");
}

#[test]
fn files_from_unreadable_list_exits_2() {
    let (code, _, _) = run(&["--files-from", "/no/such/list/file/here.txt"], b"");
    assert_eq!(code, Some(2), "unreadable list must exit 2");
}

#[test]
fn no_input_source_errors() {
    let (code, _, _) = run(&["--column-limit", "40"], b"");
    assert_eq!(code, Some(2), "no input source must error");
}

#[test]
fn stdin_filter_byte_faithful_unchanged() {
    // 1. LF, short, no trailing newline
    let src1 = "// short\nfn f() {}";
    let (code1, stdout1, _) = run(
        &["-", "--lang", "rust", "--column-limit", "80"],
        src1.as_bytes(),
    );
    assert_eq!(code1, Some(0));
    assert_eq!(stdout1, src1);

    // 2. CRLF, short, with trailing CRLF
    let src2 = "// short\r\nfn f() {}\r\n";
    let (code2, stdout2, _) = run(
        &["-", "--lang", "rust", "--column-limit", "80"],
        src2.as_bytes(),
    );
    assert_eq!(code2, Some(0));
    assert_eq!(stdout2, src2);
}

#[test]
fn stdin_filter_byte_faithful_changed() {
    // Check that reflowing a long comment preserves the exact file-ending
    // format
    let src = format!("// {LONG}");
    let (code, stdout, _) = run(
        &["-", "--lang", "rust", "--column-limit", "40"],
        src.as_bytes(),
    );
    assert_eq!(code, Some(0));
    assert!(!stdout.ends_with('\n'));
    assert!(!stdout.ends_with('\r'));
    assert!(stdout.contains("\n// "));
}

#[test]
fn files_from_list_contains_dash_errors() {
    let (code, _, stderr) = run(&["--files-from", "-"], b"-\n");
    assert_eq!(code, Some(2));
    assert!(stderr.contains('-') || stderr.contains("extension"));
}

#[test]
fn dir_reflows_supported_skips_unsupported() {
    let tmp = fresh_tmp("dir-mixed");
    let wrap = format!("// {LONG}\nint x = 0;\n");
    std::fs::write(tmp.join("a.c"), &wrap).unwrap();
    std::fs::write(tmp.join("b.rs"), format!("// {LONG}\nfn f() {{}}\n")).unwrap();
    // Unsupported extensions are silently skipped, not errors.
    std::fs::write(tmp.join("notes.txt"), &wrap).unwrap();
    std::fs::write(tmp.join("README.md"), &wrap).unwrap();

    let (code, _, stderr) = run(&[tmp.to_str().unwrap(), "--column-limit", "40"], b"");
    assert_eq!(
        code,
        Some(0),
        "unsupported files must not error; stderr={stderr}"
    );
    assert!(
        std::fs::read_to_string(tmp.join("a.c"))
            .unwrap()
            .lines()
            .count()
            > wrap.lines().count(),
        "a.c must be reflowed"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.join("notes.txt")).unwrap(),
        wrap,
        ".txt must be untouched"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn dir_skips_vcs_metadata() {
    let tmp = fresh_tmp("dir-git");
    let wrap = format!("// {LONG}\nint x = 0;\n");
    std::fs::write(tmp.join("a.c"), &wrap).unwrap();
    let git = tmp.join(".git/hooks");
    std::fs::create_dir_all(&git).unwrap();
    std::fs::write(git.join("inside.c"), &wrap).unwrap();

    let (code, _, _) = run(&[tmp.to_str().unwrap(), "--column-limit", "40"], b"");
    assert_eq!(code, Some(0));
    assert_eq!(
        std::fs::read_to_string(git.join("inside.c")).unwrap(),
        wrap,
        ".git contents must be skipped"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[cfg(unix)]
#[test]
fn dir_does_not_follow_symlinks() {
    let tmp = fresh_tmp("dir-symlink");
    let tree = tmp.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    let wrap = format!("// {LONG}\nint x = 0;\n");
    // Target lives OUTSIDE the walked tree; a symlink inside points to it.
    let outside = tmp.join("outside.c");
    std::fs::write(&outside, &wrap).unwrap();
    std::os::unix::fs::symlink(&outside, tree.join("link.c")).unwrap();

    let (code, _, _) = run(&[tree.to_str().unwrap(), "--column-limit", "40"], b"");
    assert_eq!(code, Some(0));
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        wrap,
        "symlink target must not be followed/modified"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[cfg(unix)]
#[test]
fn explicit_dir_symlink_arg_is_not_expanded() {
    // A symlink whose target is a directory, passed directly on the CLI, must
    // not be walked: "never follow symlinks" applies to the explicit arg too,
    // so files in the target tree stay untouched.
    let tmp = fresh_tmp("arg-dirlink");
    let real = tmp.join("real");
    std::fs::create_dir_all(&real).unwrap();
    let wrap = format!("// {LONG}\nint x = 0;\n");
    std::fs::write(real.join("a.c"), &wrap).unwrap();
    let link = tmp.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let (_code, _, _) = run(&[link.to_str().unwrap(), "--column-limit", "40"], b"");
    assert_eq!(
        std::fs::read_to_string(real.join("a.c")).unwrap(),
        wrap,
        "a dir-symlink arg must not be walked"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn dir_output_order_is_lexical_and_reproducible() {
    let tmp = fresh_tmp("dir-order");
    let wrap = format!("// {LONG}\nint x = 0;\n");
    for name in ["z.c", "a.c", "m.c"] {
        std::fs::write(tmp.join(name), &wrap).unwrap();
    }
    // --dry-run prints one line per file in processing order, without writing.
    let args = [tmp.to_str().unwrap(), "--dry-run", "--column-limit", "40"];
    let (code1, out1, _) = run(&args, b"");
    let (_code2, out2, _) = run(&args, b"");
    assert_eq!(code1, Some(0));
    assert_eq!(out1, out2, "output order must be reproducible");
    let pos = |needle: &str| out1.find(needle).unwrap();
    assert!(
        pos("a.c") < pos("m.c") && pos("m.c") < pos("z.c"),
        "files must be visited in lexical order, got:\n{out1}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn extensionless_posix_shell_shebang_is_reflowed() {
    let tmp = fresh_tmp("shebang-sh");
    let script = tmp.join("myscript");
    let orig = format!("#!/bin/sh\n# {LONG}\nx=0\n");
    std::fs::write(&script, &orig).unwrap();
    let (code, _, stderr) = run(&[script.to_str().unwrap(), "--column-limit", "40"], b"");
    assert_eq!(code, Some(0), "stderr={stderr}");
    let out = std::fs::read_to_string(&script).unwrap();
    assert!(
        out.starts_with("#!/bin/sh\n"),
        "shebang must be untouched, got:\n{out}"
    );
    assert!(
        out.lines().count() > orig.lines().count(),
        "comment must wrap, got:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn extensionless_non_shell_shebang_errors() {
    let tmp = fresh_tmp("shebang-zsh");
    let script = tmp.join("zscript");
    std::fs::write(&script, format!("#!/bin/zsh\n# {LONG}\nz=0\n")).unwrap();
    let (code, _, stderr) = run(&[script.to_str().unwrap(), "--column-limit", "40"], b"");
    assert_eq!(code, Some(2), "non-shell shebang must error");
    assert!(
        stderr.contains("shebang") || stderr.contains("no extension"),
        "stderr={stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn extensioned_file_with_shell_shebang_is_not_promoted() {
    // A file WITH an extension is never content-inspected, even if its first
    // line is a shell shebang.
    let tmp = fresh_tmp("shebang-txt");
    let f = tmp.join("notes.txt");
    let orig = format!("#!/bin/sh\n# {LONG}\nx=0\n");
    std::fs::write(&f, &orig).unwrap();
    let (code, _, _) = run(&[f.to_str().unwrap(), "--column-limit", "40"], b"");
    assert_eq!(
        code,
        Some(2),
        ".txt must be rejected by extension, not promoted"
    );
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        orig,
        "must be untouched"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn dir_walk_picks_up_extensionless_shell_scripts() {
    let tmp = fresh_tmp("dir-shebang");
    let script = tmp.join("script");
    let data = tmp.join("data");
    let swrap = format!("#!/bin/sh\n# {LONG}\nx=0\n");
    std::fs::write(&script, &swrap).unwrap();
    // Extensionless, no shell shebang: skipped silently during the walk.
    let dorig = format!("# {LONG}\nplain text\n");
    std::fs::write(&data, &dorig).unwrap();

    let (code, _, stderr) = run(&[tmp.to_str().unwrap(), "--column-limit", "40"], b"");
    assert_eq!(
        code,
        Some(0),
        "non-shell extensionless files must be skipped, not error; stderr={stderr}"
    );
    assert!(
        std::fs::read_to_string(&script).unwrap().lines().count() > swrap.lines().count(),
        "extensionless shell script must be reflowed"
    );
    assert_eq!(
        std::fs::read_to_string(&data).unwrap(),
        dorig,
        "non-shell file untouched"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn to_blocks_converts_line_runs_only_when_asked() {
    let tmp = fresh_tmp("to-blocks");
    let file = tmp.join("a.c");
    let src =
        "int a;\n// first line of the thought\n// second line of it\nint b; // trailing stays\n";

    // Default: style is preserved, so the run stays "//" however it reflows.
    std::fs::write(&file, src).unwrap();
    let (code, _, stderr) = run(&[file.to_str().unwrap()], b"");
    assert_eq!(code, Some(0), "stderr={stderr}");
    let out = std::fs::read_to_string(&file).unwrap();
    assert!(
        !out.contains("/*"),
        "no block may appear without --to-blocks, got:\n{out}"
    );

    // Opt in: the standalone run becomes one block, the trailing "//" does not.
    std::fs::write(&file, src).unwrap();
    let (code, _, stderr) = run(&["--to-blocks", file.to_str().unwrap()], b"");
    assert_eq!(code, Some(0), "stderr={stderr}");
    let out = std::fs::read_to_string(&file).unwrap();
    assert!(
        out.contains("/* first line of the thought second line of it */"),
        "standalone run must become one block, got:\n{out}"
    );
    assert!(
        out.contains("int b; // trailing stays"),
        "trailing // must survive, got:\n{out}"
    );

    // A second run is a no-op: conversion has nothing left to convert.
    let (code, _, _) = run(&["--to-blocks", "--check", file.to_str().unwrap()], b"");
    assert_eq!(code, Some(0), "--to-blocks must settle after one run");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[cfg(unix)]
#[test]
fn in_place_write_preserves_restrictive_permissions() {
    use std::os::unix::fs::PermissionsExt;

    // The temp copy the write goes through must never be more permissive than
    // the file it replaces, not even for the duration of the write: it holds a
    // full copy of the source. The final mode is the observable half of that.
    let dir = fresh_tmp("perm");
    let file = dir.join("secret.c");
    std::fs::write(
        &file,
        "/* a comment long enough that reflow has to rewrite it at the column limit below */\nint x;\n",
    )
    .unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();

    // Pin the column limit: the assertion below needs the write to actually
    // happen, and discovery would otherwise let an ambient ".clang-format"
    // anywhere above the test directory decide whether it does.
    let (code, _, stderr) = run(&["--column-limit", "40", file.to_str().unwrap()], b"");
    assert_eq!(code, Some(0), "write failed: {stderr}");
    assert!(
        std::fs::read_to_string(&file).unwrap().lines().count() > 2,
        "the comment should have been reflowed onto more lines"
    );
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "permissions must survive the rename");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The set of supported extensions is written down four times: the match arms
/// in "detect_language", the error message beside them, the "--help" text, and
/// the README. Only the first two share a constant; the other two are prose, so
/// this is what keeps them from drifting apart. Comparison is case-folded,
/// since the tool folds too: prose may spell out ".S" where the constant has
/// "s", and that is the same extension, not a discrepancy.
#[test]
fn extension_list_is_consistent_across_code_help_and_readme() {
    use commentflow::parse::{SUPPORTED_EXTENSIONS, detect_language};

    // Every listed extension is actually accepted, in either case.
    for ext in SUPPORTED_EXTENSIONS {
        for spelling in [ext.to_string(), ext.to_uppercase()] {
            let path = PathBuf::from(format!("probe.{spelling}"));
            assert!(
                detect_language(&path).is_ok(),
                "listed extension is rejected: .{spelling}"
            );
        }
    }

    /// Every ".ext" token in one paragraph of prose, case-folded and deduped.
    fn quoted_extensions(text: &str, marker: &str) -> Vec<String> {
        let tail = text.split(marker).nth(1).unwrap_or_else(|| {
            panic!("marker {marker:?} not found; the prose describing extensions moved")
        });
        let paragraph = tail.split("\n\n").next().unwrap_or(tail);
        let mut found: Vec<String> = paragraph
            .split(|c: char| c.is_whitespace() || c == '`')
            .filter_map(|tok| {
                let tok = tok.trim_end_matches([',', '.', ':']);
                let rest = tok.strip_prefix('.')?;
                (!rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '+'))
                    .then(|| rest.to_ascii_lowercase())
            })
            .collect();
        found.sort();
        found.dedup();
        found
    }

    let mut expected: Vec<String> = SUPPORTED_EXTENSIONS
        .iter()
        .map(|e| (*e).to_string())
        .collect();
    expected.sort();

    let (_, help, _) = run(&["--help"], b"");
    assert_eq!(
        quoted_extensions(&help, "Extensions"),
        expected,
        "--help text does not match SUPPORTED_EXTENSIONS"
    );

    let readme =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md"))
            .expect("read README.md");
    assert_eq!(
        quoted_extensions(&readme, "Supported extensions"),
        expected,
        "README does not match SUPPORTED_EXTENSIONS"
    );
}
