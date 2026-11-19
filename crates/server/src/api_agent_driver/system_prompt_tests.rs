//! The system prompt tells the model what it may touch. With no `allowed_paths` the role is
//! limited to the worktree by the jail, so the prompt must not call it unrestricted.

use super::*;

fn role(allowed_paths: Vec<String>) -> camerata_core::Role {
    camerata_core::Role {
        name: "t".to_string(),
        rule_subset: vec![],
        allowed_paths,
    }
}

#[test]
fn no_allowed_paths_says_only_the_worktree_not_unrestricted() {
    let prompt = build_system_prompt(&role(vec![]), "claude-sonnet-5").unwrap();
    assert!(!prompt.contains("unrestricted"), "{prompt}");
    assert!(prompt.contains("only the worktree"), "{prompt}");
}

#[test]
fn listed_allowed_paths_are_named() {
    let prompt = build_system_prompt(
        &role(vec!["src/".into(), "tests/".into()]),
        "claude-sonnet-5",
    )
    .unwrap();
    assert!(prompt.contains("Allowed paths: src/, tests/"), "{prompt}");
}
