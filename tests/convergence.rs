//! Randomized convergence and word-preservation sweep.
//!
//! Every other test in this suite feeds the pipeline an input somebody thought
//! of. That is how a real bug survived 400 green tests: the reflow packer
//! parked a Doxygen tag at the start of a continuation line, the next pass read
//! the line as a kernel-doc region and ate the tag word, and no hand-written
//! case happened to put a tag near a wrap boundary. The harness was never the
//! problem, "common::pipeline" has always asserted a second pass is a no-op.
//! The inputs were.
//!
//! So this file writes the inputs instead. A fixed-seed LCG builds comment
//! bodies from a vocabulary picked to sit on the seams (real tags, tags that
//! only look real, the kernel-doc "@name :" shape the converter emits, path
//! and email shapes, multi-byte words) and runs each through every comment
//! style at a spread of column limits.
//!
//! Deterministic on purpose: a failure here reproduces from the seed and the
//! case number in the panic message, and CI never flakes. Widen "VOCAB" or
//! "shapes" when a new class of input burns you; do not switch to a random
//! seed.

use common::{detect, pipeline};

mod common;

/// Fixed-seed LCG. Not cryptography, just a reproducible word picker.
struct Rng(u64);

impl Rng {
    /// The high 31 bits of the state, so the result fits a `u32` exactly and
    /// widening it to `usize` is lossless on a 32-bit target too.
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    fn below(&mut self, n: u32) -> usize {
        (self.next() % n) as usize
    }

    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.next() as usize % xs.len()]
    }
}

/// Every word of comment prose in a source, comment markers dropped. Comparing
/// this before and after is what catches a swallowed word, which is the failure
/// mode that matters: an ugly line is recoverable, a deleted word is not.
fn words_of(s: &str) -> Vec<&str> {
    s.split_whitespace()
        .filter(|w| {
            !matches!(
                *w,
                "//" | "/*" | "*" | "*/" | "///" | "/**" | "//!" | "/*!" | "#"
            )
        })
        .collect()
}

const VOCAB: &[&str] = &[
    // Convertible tags: the set "normalize::kdoc_tag_of" rewrites, so the set
    // that can eat a word if reflow parks it at a line start.
    "@param",
    "\\param",
    "@return",
    "\\return",
    "@returns",
    "\\returns",
    "@param[in]",
    "@param[out]",
    // Known but unconvertible. These abort the conversion instead of driving
    // it, and must survive just the same.
    "@note",
    "@brief",
    "@retval",
    "@warning",
    "@see",
    "@code",
    "@endcode",
    // The shape "convert_kernel_doc" emits, glued and split. A second pass over
    // converted output has to reproduce it byte for byte.
    "@buf:",
    "@len:",
    "@epsilon",
    ":",
    // Bare words that double as Doxygen tag keywords. A param declared with one
    // of these ("@param note") makes a later "@note" spell exactly like the
    // reference "is_param_xref" exempts, so its "DOXY_TAGS" membership test is
    // the only thing keeping a real section out of the preceding param's
    // description. Without these in the vocabulary the collision is
    // ungeneratable.
    //
    // Both need a BARE tag token above to collide with, which is why "file" is
    // not here despite being the most realistic collision of the three: the
    // only file-shaped token is the path "@file.txt", and "kdoc_xref_name"
    // rejects it on the dot, so it aborts as a foreign tag whether or not a
    // param shares the name. Adding a bare "@file" makes the collision
    // generatable and immediately trips a pre-existing blank-line bug unrelated
    // to the exemption, so the file case is pinned by hand instead, in
    // "kernel_doc_param_cross_reference_is_description_text" and
    // "doxygen_section_tag_survives_a_param_of_the_same_name".
    "note",
    "warning",
    // Tag names outside "[A-Za-z]", which "is_foreign_tag" does not claim.
    "@1buf",
    "@1buf:",
    "@2:",
    "@_x",
    "@x_y:",
    "@param:",
    "@return:",
    "@PARAM",
    "@Param",
    // Path and email shapes. "doxy_keyword" truncates at the dot, so these read
    // as tags to a naive test and must not.
    "@file.txt",
    "@param.txt",
    "@param.",
    "@return/x",
    "@note/path",
    "@code.c",
    "\\param.h",
    "@see.com",
    "user@example.com",
    // Rule runs, which the bookend and one-sided-banner rules key on. A run
    // that lands alone in a paragraph is decoration and freezes; one inside a
    // paragraph is an em-dash and must keep reflowing.
    "--------",
    "========",
    "---",
    "***",
    // ACSL clause shapes. The annotation body is passthrough, but its closer
    // decides between split, veto, and no-op, so the words around it have to be
    // generatable. A bare "@" is the continuation marker Frama-C reads as
    // whitespace. "@*/" is deliberately NOT here: a body word spelling a block
    // closer would terminate every block shape early, so the two closers are
    // pinned in "shapes" instead.
    "\\valid(p);",
    "\\result",
    "@",
    // Escapes that open with a marker but are ordinary prose.
    "\\0",
    "\\n",
    "\\t",
    "\\\\",
    "@foo",
    // Plain prose across a spread of widths and scripts.
    "a",
    "xy",
    "alpha",
    "gamma",
    "epsilon",
    "reallyquitelongword",
    "幅の広い",
    "café",
];

/// One body, wrapped in every comment style the tool handles. Assembly is here
/// because it is the one language with no grammar (a byte scanner produces its
/// comments), so it is the one that can drift from the tree-sitter path.
fn shapes(body: &str) -> Vec<(&'static str, String)> {
    vec![
        ("foo.c", format!("int f(void) {{ return 0; }}\n// {body}\n")),
        (
            "foo.c",
            format!("int f(void) {{ return 0; }}\n/* {body} */\n"),
        ),
        (
            "foo.c",
            format!("/** lead {body} */\nint f(int w) {{ return w; }}\n"),
        ),
        (
            "foo.c",
            format!("int f(void) {{ return 0; }}\n/* lead one\n *\n * {body}\n */\n"),
        ),
        ("foo.rs", format!("/// lead {body}\nfn f() {{}}\n")),
        ("foo.rs", format!("fn f() {{}}\n// {body}\n")),
        ("foo.sh", format!("f() {{ :; }}\n# {body}\n")),
        ("foo.S", format!("nop\n/* {body} */\n")),
        // ACSL, both closer spellings. The glued "*/" is split onto its own
        // line and must land on a fixed point; the "@*/" form is vetoed and
        // must come back byte-identical.
        (
            "foo.c",
            format!("/*@ requires x;\n    {body} */\nint f(void);\n"),
        ),
        (
            "foo.c",
            format!("/*@ requires x;\n  @ {body}\n  @*/\nint f(void);\n"),
        ),
    ]
}

fn body(rng: &mut Rng) -> String {
    let n = 2 + rng.below(16);
    (0..n)
        .map(|_| rng.pick(VOCAB))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A body that opens on plain prose, which keeps transform 2 out of the way: a
/// comment genuinely opening on a tag is kernel-doc, and rewriting its words is
/// that transform's job, not a bug. What is left is the reflow invariant alone,
/// that no word gets parked where a later pass will eat it.
fn prose_led_body(rng: &mut Rng) -> String {
    format!("lead {}", body(rng))
}

/// No comment word is ever lost, and the output is a fixed point.
#[test]
fn no_comment_word_is_lost() {
    let mut rng = Rng(0xC0FF_EE01);
    for case in 0..1500u32 {
        let text = prose_led_body(&mut rng);
        let width = 16 + rng.below(64);
        for (name, src) in shapes(&text) {
            let p1 = pipeline(&src, detect(name), width);
            assert_eq!(
                words_of(&src),
                words_of(&p1),
                "case {case} {name} w={width} lost or invented a word\n--- src\n{src}--- out\n{p1}"
            );
            let p2 = pipeline(&p1, detect(name), width);
            assert_eq!(p1, p2, "case {case} {name} w={width} pass 2 diverged");
        }
    }
}

/// Convergence when transform 2 does fire. Its output feeds straight back into
/// reflow on the next run, so the converted shape has to be a fixed point too.
/// Three passes, because a two-cycle only shows up on the third.
#[test]
fn converged_output_survives_reruns() {
    let mut rng = Rng(0x5EED_0002);
    for case in 0..1000u32 {
        let text = body(&mut rng);
        let width = 16 + rng.below(64);
        for (name, src) in shapes(&text) {
            let p1 = pipeline(&src, detect(name), width);
            let p2 = pipeline(&p1, detect(name), width);
            let p3 = pipeline(&p2, detect(name), width);
            assert_eq!(
                p1, p2,
                "case {case} {name} w={width} pass 2 diverged\n--- src\n{src}--- p1\n{p1}--- p2\n{p2}"
            );
            assert_eq!(p2, p3, "case {case} {name} w={width} pass 3 diverged");
        }
    }
}

/// The tag rule is allowed to overrun the column limit, but only where it has
/// no other move. There are exactly two such reasons, and both trade width for
/// bytes: a run of tags it cannot break in front of, and a rule run it must not
/// strand on its own line. Any other overlong line is a packing bug, so pin the
/// two exceptions rather than the absence of one. The rule-run reason has two
/// spellings, since escaping a two-sided bookend appends a word past the run.
#[test]
fn only_a_tag_or_a_rule_may_overflow() {
    let mut rng = Rng(0xBEEF_0003);
    for case in 0..1500u32 {
        let text = prose_led_body(&mut rng);
        let width = 40 + rng.below(40);
        let src = format!("int f(void) {{ return 0; }}\n// {text}\n");
        let out = pipeline(&src, detect("foo.c"), width);
        for line in out.lines() {
            if line.chars().count() <= width || !line.starts_with("//") {
                continue;
            }
            let words: Vec<&str> = line.split_whitespace().collect();
            let is_rule = |w: &str| w.starts_with(['-', '=', '*']);
            let last = words.last().copied().unwrap_or("");
            let tag = last.starts_with('@') || last.starts_with('\\');
            // A rule run can end the line, or sit one word from the end: the
            // packer escapes a two-sided bookend by appending one more word
            // AFTER the trailing run, so the line then ends in whatever word
            // broke it ("--- *** a -------- user@example.com").
            let rule = is_rule(last) || (words.len() >= 2 && is_rule(words[words.len() - 2]));
            assert!(
                tag || rule,
                "case {case} w={width} line over the limit ends in neither a tag nor a rule: {line:?}\n--- src\n{src}--- out\n{out}"
            );
        }
    }
}
