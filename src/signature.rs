//! The two transforms that read a function's SIGNATURE, not just its comments.
//!
//! Everything else in "parse" locates comment nodes and builds "Comment"s from
//! them. These two ask tree-sitter what a "function_definition" looks like, to
//! decide where a comment BELONGS: the drifted-parameter shift (Scope transform
//! 4) and the X11 manual-page hoist (Scope transform 3), in that order below.
//! They are the only sanctioned uses of syntactic context beyond finding
//! comments, and they still move nothing but comment bytes.
//!
//! The module is documentation, not a wall: both are still CALLED from "parse"
//! (in "extract_comments_with" and "walk_collect"), so a third syntactic read
//! added at those call sites would not touch this file. What the boundary buys
//! is that the two sanctioned ones have a name and an address, so the ceiling
//! TODO.md sets is something a reviewer can point at.

use std::collections::HashMap;

use tree_sitter::Node;

use crate::parse::{Language, ParamShift, is_passthrough_directive, line_start_before};

/// Detect C/C++ signatures whose parameter comments all drifted forward by one:
/// every parameter after the first carries exactly one *leading* comment (the
/// ", comment param" shape inside "parameter_list"), plus exactly one comment
/// trailing the closing ")". That extra trailing comment is the tell that the
/// whole set is displaced: normal leading comments describe the *following*
/// parameter and leave nothing after ")". Returns a map from each drifted
/// comment's start byte to the offset where it should become a trailing comment
/// (the end of the parameter it actually describes: the previous one, or the
/// last parameter for the after-")" comment). Any deviation (a parameter with
/// zero or two leading comments, a missing trailing comment, a leading comment
/// on the first parameter) yields no entry for that function, so the transform
/// never fires on an ordinary signature. Idempotent: once shifted, each comment
/// sits as "param comment ," (before the comma), which is not the drift shape.
pub(crate) fn collect_param_shifts(
    root: Node,
    source: &str,
    lang: Language,
) -> HashMap<usize, ParamShift> {
    let mut map = HashMap::new();
    if !matches!(lang, Language::C | Language::Cpp) {
        return map;
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_definition" {
            detect_param_drift(node, source, lang, &mut map);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    map
}

fn detect_param_drift(
    func: Node,
    source: &str,
    lang: Language,
    map: &mut HashMap<usize, ParamShift>,
) {
    let Some(decl) = func.child_by_field_name("declarator") else {
        return;
    };
    if decl.kind() != "function_declarator" {
        return;
    }
    let Some(plist) = decl.child_by_field_name("parameters") else {
        return;
    };
    if plist.kind() != "parameter_list" {
        return;
    }
    let text_of = |node: Node| &source[node.start_byte()..node.end_byte()];
    // The comment group trailing ")" (between the declarator and the body).
    let body_start = func.child_by_field_name("body").map(|b| b.start_byte());
    let mut trailing: Vec<Node> = Vec::new();
    let mut cursor = func.walk();
    for child in func.children(&mut cursor) {
        if child.kind() == "comment"
            && child.start_byte() >= decl.end_byte()
            && body_start.is_none_or(|b| child.end_byte() <= b)
        {
            trailing.push(child);
        }
    }

    // Exactly one, which is what the tell means: a single comment displaced off
    // the end of the list. Two or more after ")" is not one-step drift, and
    // shifting them all onto the last parameter invents a grouping the author
    // never wrote. The doc comment above has always said "exactly one"; the
    // code only checked "at least one".
    if trailing.len() != 1 {
        return;
    }
    // A real manual-page block after ")" belongs to transform 3, not here.
    if trailing
        .iter()
        .any(|&t| has_manpage_section_headers(text_of(t)))
    {
        return;
    }

    // Only "/* */" blocks shift; a "//" line comment would swallow the code
    // after it once moved inline.
    if trailing.iter().any(|&t| !is_block_comment(text_of(t))) {
        return;
    }

    // Parse the parameter_list into (param, leading comment group). Shape must
    // be "(" param ("," comment* param)* ")": the first parameter has no
    // leading comment; each later parameter may carry a whole group ("/* a */
    // /* b */"), all of which shift together. A comment before the first param
    // or stray tokens bail.
    let mut pcur = plist.walk();
    let kids: Vec<Node> = plist.children(&mut pcur).collect();
    let mut params: Vec<(Node, Vec<Node>)> = Vec::new();
    let mut i = 0;
    if kids.first().map(Node::kind) != Some("(") {
        return;
    }
    i += 1;
    if kids.get(i).map(Node::kind) != Some("parameter_declaration") {
        return; // no first param, or a comment leads it
    }
    params.push((kids[i], Vec::new()));
    i += 1;
    while kids.get(i).map(Node::kind) == Some(",") {
        i += 1;
        let mut group = Vec::new();
        while kids.get(i).map(Node::kind) == Some("comment") {
            if !is_block_comment(text_of(kids[i])) {
                return; // "//" line comment; see the trailing check
            }
            group.push(kids[i]);
            i += 1;
        }
        if kids.get(i).map(Node::kind) != Some("parameter_declaration") {
            return; // a missing param
        }
        params.push((kids[i], group));
        i += 1;
    }
    if kids.get(i).map(Node::kind) != Some(")") || i != kids.len() - 1 {
        return;
    }

    // The commented parameters must form a contiguous suffix ending at the last
    // parameter (they and the after-")" group are all displaced forward by
    // one). The first parameter must be uncommented (nowhere to shift back to).
    let Some(m) = params.iter().position(|(_, g)| !g.is_empty()) else {
        return; // no leading comments at all
    };
    if m == 0 || !params[m..].iter().all(|(_, g)| !g.is_empty()) {
        return;
    }

    // A machine directive (NOLINT, ACSL, cppcheck, ...) in the drifted set must
    // not move. This transform is atomic over the whole signature: shifting
    // some comments while one stays put scrambles the parameter/comment pairing
    // into a state that is neither the original nor a clean de-drift. So if any
    // participant can't shift, abort the whole signature to passthrough.
    if trailing
        .iter()
        .chain(params[m..].iter().flat_map(|(_, g)| g))
        .any(|&n| is_passthrough_directive(text_of(n), lang))
    {
        return;
    }

    // Shift each group back one: the group leading params[i] describes
    // params[i-1]; the after-")" group describes the last parameter.
    for i in m..params.len() {
        let target = params[i - 1].0.end_byte();
        for comment in &params[i].1 {
            map.insert(
                comment.start_byte(),
                ParamShift {
                    insert_at: target,
                    after_paren: false,
                },
            );
        }
    }
    let last_end = params[params.len() - 1].0.end_byte();
    for comment in &trailing {
        map.insert(
            comment.start_byte(),
            ParamShift {
                insert_at: last_end,
                after_paren: true,
            },
        );
    }
}

/// A drifted parameter comment is only shiftable when it is a "/* … */" block:
/// a "//" line comment moved inline would comment out the following code. A
/// multi-line block is allowed but collapsed to one line on re-insert (see
/// "plan"), so it can't round-trip through the trailing-closer split.
fn is_block_comment(text: &str) -> bool {
    text.starts_with("/*")
}

fn has_manpage_section_headers(text: &str) -> bool {
    let mut description = false;
    let mut returns = false;
    for part in text.split(['\n', '\r', '*']) {
        let word = part
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches(':');
        description |= word == "DESCRIPTION";
        returns |= matches!(word, "RETURN" | "RETURNS");
    }
    description && returns
}

/// A C/C++ block comment wedged between a function's signature and its body
/// ("type name(...) /* here */ { ... }") is the X11 manual-page placement.
/// When the comment carries "DESCRIPTION"/"RETURN" sections, normalize hoists
/// it ahead of the function; this returns the insert target (the physical line
/// start of the "function_definition"). Any other position yields "None", so
/// the relocation never fires for an ordinary comment.
///
/// "parent" is the comment's enclosing node, handed down by the walk. Asking
/// tree-sitter for it instead ("Node::parent") costs a fresh descent from the
/// root, which is quadratic over a long run of sibling comments.
pub(crate) fn manpage_relocate_target(
    node: Node,
    parent: Option<Node>,
    source: &str,
    lang: Language,
) -> Option<usize> {
    if !matches!(lang, Language::C | Language::Cpp) {
        return None;
    }
    let parent = parent?;
    if parent.kind() != "function_definition" {
        return None;
    }
    let decl = parent.child_by_field_name("declarator")?;
    let body = parent.child_by_field_name("body")?;
    if node.start_byte() >= decl.end_byte() && node.end_byte() <= body.start_byte() {
        Some(line_start_before(source, parent.start_byte()))
    } else {
        None
    }
}
