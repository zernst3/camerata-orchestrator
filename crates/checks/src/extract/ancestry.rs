//! Constant-time parent lookup for the literal extractors.
//!
//! `tree_sitter::Node::parent` is not O(1): it re-descends from the root to find the parent, so
//! its cost grows with the depth of the node and the number of siblings along the way. The
//! literal extractors climb several ancestors per literal, and a file with thousands of
//! literals made that quadratic in practice. The walk here carries the ancestor chain with it,
//! so [`Pos::parent`] is a slice split.

use tree_sitter::Node;

use super::{LiteralCtx, ModuleConst};

/// A node together with its ancestors (root first, nearest parent last).
#[derive(Clone, Copy)]
pub(crate) struct Pos<'a, 't> {
    pub node: Node<'t>,
    chain: &'a [Node<'t>],
}

impl<'a, 't> Pos<'a, 't> {
    /// Whether the node or any ancestor is a tree-sitter error node: a region the grammar had
    /// to recover around, whose structure cannot be trusted.
    pub fn under_error(self) -> bool {
        self.node.is_error() || self.chain.iter().any(|n| n.is_error())
    }

    /// The parent, or `None` at the root.
    pub fn parent(self) -> Option<Pos<'a, 't>> {
        let (parent, rest) = self.chain.split_last()?;
        Some(Pos {
            node: *parent,
            chain: rest,
        })
    }
}

/// Visit every node under `root` in document order, handing the visitor each node's position.
/// Iterative (a `TreeCursor` plus an explicit ancestor stack), so a pathologically nested file
/// cannot overflow the call stack.
pub(crate) fn walk_with_ancestors<'t>(root: Node<'t>, mut visit: impl FnMut(Pos<'_, 't>)) {
    let mut cursor = root.walk();
    let mut chain: Vec<Node<'t>> = Vec::new();
    loop {
        let node = cursor.node();
        visit(Pos {
            node,
            chain: &chain,
        });
        if cursor.goto_first_child() {
            chain.push(node);
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
            chain.pop();
        }
    }
}

/// What the home climb found for a literal.
pub(crate) enum Home {
    /// The literal initializes this named constant.
    Const(ModuleConst),
    /// A constant-like declaration the walker cannot name; neither a home nor a bare copy.
    Unattributed,
    /// Not the initializer of a constant.
    None,
}

impl Home {
    /// Record the verdict on the literal's context.
    pub fn apply(self, ctx: &mut LiteralCtx) {
        match self {
            Home::Const(home) => ctx.module_const = Some(home),
            Home::Unattributed => ctx.unattributed = true,
            Home::None => {}
        }
    }
}
