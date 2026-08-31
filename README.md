# commentflow

commentflow reflows comments in C, C++, Rust, POSIX shell, and GAS assembly so
they fit a column limit. It rewrites comment bytes only; code bytes stay
unchanged.

## Why It Exists

Code formatters do not fully fix badly wrapped comments. `clang-format` can
break a comment line that exceeds `ColumnLimit`, but it will not join short
lines into a clean paragraph. A paragraph wrapped at column 50 stays wrapped at
column 50. `rustfmt`'s `wrap_comments` is nightly-only and has the same one-way
behavior. `shfmt` does not touch comment text.

That gap matters most in generated or heavily edited code, where comments often
arrive wrapped at an arbitrary width, as one very long line, or in the shape of
an older sentence. Standard formatters preserve that shape.

commentflow is the missing pass: run it before `clang-format`, `rustfmt`, or
`shfmt`, not instead of them.

```diff
-    // Grow the ring when it is full. We double the capacity so that a
-    // sequence of pushes stays
-    // amortized O(1); the caller never
-    // sees the reallocation.
+    // Grow the ring when it is full. We double the capacity so that a sequence
+    // of pushes stays amortized O(1); the caller never sees the reallocation.
```

## What Makes It Different

Conservative by default. Preformatted regions pass through byte for byte:
code fences, Doxygen `@code` blocks, tables, indented samples, ASCII and Unicode
diagrams, license and SPDX identifiers, and `Key: value` banner rows. Ambiguous
lines are preserved, because corrupting a diagram is worse than leaving one
paragraph unreflowed.

Tool directives stay intact. `// clang-format off`, `// NOLINT`,
`cppcheck-suppress`, Frama-C ACSL `/*@ ... */`, `// IWYU pragma:`,
`# shellcheck disable=`, `/// cbindgen:`, and similar directives are machine
instructions, not prose. Reflowing one can move it away from the line it guards,
so these comments pass through untouched. The one layout fix applied to a
multi-line Frama-C ACSL block is moving a glued closing `*/` onto its own line,
under the opener's `*`. Every token of the annotation survives; the only bytes
that move with the closer are the horizontal spaces that sat in front of it,
which would otherwise be left trailing the line. A closer already spelled `@*/`
is left alone, since that is Splint's required delimiter and the idiomatic
`@`-marker ACSL closer.

No build context required. Parsing is Tree-sitter-based, so each file is
analyzed on its own. There are no translation units, include paths,
`compile_commands.json`, Cargo workspace, or macro expansion requirements.

Assembly support is cautious. GAS chooses its line-comment character per
target, and every candidate has meaning elsewhere: `#` is a comment on x86 but
an immediate on ARM32; `@` is a comment on ARM32 but a relocation suffix on
x86-64. Because a `.s` file does not state its target, commentflow rewrites only
`/* */` comments and skips any block comment that a target-specific line comment
could have swallowed.

Output is stable. Running commentflow twice produces the same bytes as
running it once. The test suite asserts this because `--check` is only useful in
CI when output converges.

Beyond layout, commentflow applies four narrowly gated content transforms and
one spacing rule:

- Decorative bookends: `/* ---- name ---- */` collapses to its label. A run
  on only one side is treated as content and left alone.
- Doxygen to kernel-doc: `\param name desc` becomes `@name : desc`;
  `\return desc` becomes a blank-separated `Return desc`. C and C++ only.
- X11 manual pages: A `DESCRIPTION` / `RETURNS` block wedged between a
  signature and its `{` is cleaned and hoisted above the function.
- Drifted parameter comments: `f(int a, /* the b */ int b) /* the c */`
  becomes `f(int a /* the b */, int b /* the c */)`.
- Blank line before a block: An explanatory comment dropped flush onto the
  statement above it gets one blank line.

## Prebuilt Binaries

Every push to `main` that passes the test suite on all three platforms replaces
the [`latest`](https://github.com/sysprog21/commentflow/releases/tag/latest)
release. There is one tarball per target, and only the newest build is kept:

| Platform | Asset |
| --- | --- |
| Linux x86\_64 | `commentflow-x86_64-unknown-linux-gnu.tar.gz` |
| macOS arm64 | `commentflow-aarch64-apple-darwin.tar.gz` |
| Windows x86\_64 | `commentflow-x86_64-pc-windows-msvc.tar.gz` |

The Linux asset is spelled out below; substitute the row above that matches the
machine you are on.

```sh
curl -fsSL https://github.com/sysprog21/commentflow/releases/download/latest/commentflow-x86_64-unknown-linux-gnu.tar.gz | tar xz
./commentflow --help
```

`tar` extracts all three, Windows included: `bsdtar` has shipped with Windows
since 10 1803. These binaries carry no Developer ID signature. The `curl` line
above attaches no quarantine attribute, so what it extracts runs as is; a copy
fetched through a browser is quarantined until
`xattr -d com.apple.quarantine commentflow` clears it.

## Usage

```sh
cargo build --release

commentflow src/foo.c              # rewrite in place
commentflow --check src/           # exit 1 if anything would change, no writes
commentflow --diff src/foo.c       # unified diff to stdout, no writes
commentflow --dry-run src/         # one summary line per file
commentflow --to-blocks src/foo.c  # also convert standalone // runs to /* */
cat foo.c | commentflow - --lang c # filter mode
```

Directory arguments are walked recursively, extension-gated, and never follow
symlinks. `--files-from <file|->` reads a newline- or NUL-delimited path list.

The column limit comes from the nearest `.clang-format` (`ColumnLimit`, default:
80), discovered by walking upward from the file. The same value governs Rust,
shell, and assembly sources. There is no `rustfmt.toml` or shfmt config parsing.
`--column-limit N` overrides discovery; `--column-limit 0` disables reflow.

Supported extensions: `.c` `.h` `.m` `.cc` `.cpp` `.cxx` `.c++` `.hh` `.hpp`
`.hxx` `.h++` `.mm` `.rs` `.sh` `.bash` `.s` `.S`. An extensionless file is read
as shell only when line 1 is a recognized POSIX-shell shebang. Anything else
fails fast instead of being handled on a best-effort basis.

## Contract

These are correctness invariants, not preferences. A violation is a bug.

- Bytes outside comment ranges are never modified. The two transforms that move
  a comment insert only at a recorded, validated target; the blank-line rule
  rewrites only the whitespace above a comment. Neither overwrites an existing
  code byte, and the token stream is unchanged.
- Comment style is preserved. `/* */` stays `/* */`, `//` stays `//`. The single
  exception is the opt-in `--to-blocks`, which runs as a separate pass ahead of
  reflow.
- Blank lines inside comments survive; original indentation survives; the
  comment's own line endings survive (CRLF stays CRLF).
- Preformatted regions pass through untouched.
- The same input and column limit yield identical bytes, run after run.
- `--check` and `--diff` never write.

## Not In Scope

Code formatting, include resolution, macro expansion, semantic analysis, doc
generation, spelling or grammar correction, LLM rewriting, and languages beyond
the five above, even where the comment syntax overlaps (Python, TOML, Makefiles,
zsh, nasm/masm). The value of this tool is that it does one thing.

## Development

```sh
./scripts/check.sh   # rustfmt, clippy -D warnings, and the full test suite
```

CI runs that same script, so it cannot drift from what a contributor runs
locally.

## License

`commentflow` is available under a permissive
[MIT](https://opensource.org/license/mit)-style license.
Use of this source code is governed by a MIT license that can be found
in the [LICENSE](LICENSE) file.
