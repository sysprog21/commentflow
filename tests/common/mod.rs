//! The one pipeline harness the integration tests share.
//!
//! It lived in two copies, which meant the convergence assertion below had to
//! be kept in sync by hand across two files. It is the assertion that catches
//! the whole class of bug where one pass rewrites structure the next pass reads
//! differently, so it is not something to have two of.

use std::path::PathBuf;

use commentflow::config::IndentConfig;
use commentflow::parse::Language;
use commentflow::{parse, rewrite};

pub fn pipeline(source: &str, lang: Language, column_limit: usize) -> String {
    if column_limit == 0 {
        return source.to_string();
    }
    let indent_cfg = IndentConfig::default();

    // Drive the real entry point: the hand-rolled loop this replaced skipped
    // the comment-move transforms, the blank-line rule, and "plan"'s
    // postconditions, so a test could pass on behavior the binary never has.
    let mut pool = parse::ParserPool::new();
    let reps = commentflow::plan(source, lang, column_limit, indent_cfg, &mut pool).unwrap();
    let out = rewrite::apply(source, &reps);

    // Convergence is part of the shipped contract: "--check" has to settle, or
    // it reports a diff forever. Asserting it HERE means every test gets the
    // check for free. Five defects were found this way, all in shapes no
    // hand-written case covered, which is exactly what a per-test opt-in
    // misses.
    let reps2 = commentflow::plan(&out, lang, column_limit, indent_cfg, &mut pool).unwrap();
    let out2 = rewrite::apply(&out, &reps2);
    assert_eq!(
        out, out2,
        "second pass changed the output (not idempotent)\n--- source\n{source}--- pass 1\n{out}--- pass 2\n{out2}"
    );
    out
}

pub fn detect(path: &str) -> Language {
    parse::detect_language(&PathBuf::from(path)).unwrap()
}
