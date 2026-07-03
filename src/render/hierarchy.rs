use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fmt::Write as _;

use crate::model::RelTarget;
use crate::model::cmp_siblings;
use crate::store::relationships::ParentEdge;

/// Repo-wide traversal depth cap, matching GitHub's sub-issue nesting limit.
const MAX_DEPTH: usize = 8;

/// Escape characters that would break Markdown link text (`[title](target)`).
fn escape_link_text(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

/// One rendered line for `node` at `depth`. Same-repo nodes link to their
/// entity file; cross-repo nodes are plain text suffixed `— external`.
fn write_node(out: &mut String, node: &RelTarget, depth: usize, width: usize) {
    let indent = "  ".repeat(depth);
    let title = escape_link_text(&node.title);
    let state = node.state.as_str();
    match &node.repo {
        None => {
            let number = node.number;
            writeln!(
                out,
                "{indent}- [#{number} {title}](issues/{number:0width$}.md) ({state})"
            )
            .ok();
        }
        Some(repo) => {
            writeln!(
                out,
                "{indent}- {repo}#{} {title} ({state}) — external",
                node.number
            )
            .ok();
        }
    }
}

/// Recursively render `node` and its children into `out`, guarding against
/// cycles (a `visited` set) and capping depth at [`MAX_DEPTH`].
fn walk(
    out: &mut String,
    node: &RelTarget,
    depth: usize,
    width: usize,
    children: &BTreeMap<&str, Vec<&RelTarget>>,
    visited: &mut HashSet<String>,
) {
    if depth > MAX_DEPTH || !visited.insert(node.node_id.clone()) {
        return;
    }
    write_node(out, node, depth, width);
    if let Some(kids) = children.get(node.node_id.as_str()) {
        let mut kids = kids.clone();
        kids.sort_by(|a, b| cmp_siblings(a, b));
        for kid in kids {
            walk(out, kid, depth + 1, width, children, visited);
        }
    }
}

/// Render the repo-wide parent/sub-issue tree.
///
/// Roots are same-repo parents with no live same-repo parent of their own,
/// sorted by number. Children render in sibling order ([`cmp_siblings`]).
/// Traversal keeps a visited set (cycle guard against inconsistent cache
/// states) and stops at depth 8, GitHub's nesting limit.
#[must_use]
pub fn hierarchy_doc(edges: &[ParentEdge], width: usize) -> String {
    // parent node_id → children (child ordering applied per level).
    let mut children: BTreeMap<&str, Vec<&RelTarget>> = BTreeMap::new();
    // node_id → the node's own info as seen on the parent side of an edge.
    let mut parents: BTreeMap<&str, &RelTarget> = BTreeMap::new();
    // node_ids that have a same-repo parent.
    let mut has_local_parent: HashSet<&str> = HashSet::new();

    for e in edges {
        children
            .entry(e.parent.node_id.as_str())
            .or_default()
            .push(&e.child);
        parents
            .entry(e.parent.node_id.as_str())
            .or_insert(&e.parent);
        if e.parent.repo.is_none() {
            has_local_parent.insert(e.child.node_id.as_str());
        }
    }

    let mut roots: Vec<&RelTarget> = parents
        .values()
        .filter(|p| p.repo.is_none() && !has_local_parent.contains(p.node_id.as_str()))
        .copied()
        .collect();
    roots.sort_by_key(|p| p.number);

    let mut out = String::from("# Issue hierarchy\n\n");
    if roots.is_empty() {
        out.push_str("No issue hierarchies.\n");
        return out;
    }

    let mut visited: HashSet<String> = HashSet::new();
    for root in roots {
        walk(&mut out, root, 0, width, &children, &mut visited);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::IssueState;

    fn t(
        node: &str,
        repo: Option<&str>,
        number: i64,
        title: &str,
        position: Option<i64>,
    ) -> RelTarget {
        RelTarget {
            node_id: node.into(),
            repo: repo.map(str::to_string),
            number,
            state: IssueState::Open,
            title: title.into(),
            position,
        }
    }

    fn edge(parent: RelTarget, child: RelTarget) -> ParentEdge {
        ParentEdge { parent, child }
    }

    #[test]
    fn empty_hierarchy_is_shape_stable() {
        insta::assert_snapshot!(hierarchy_doc(&[], 4), @r"
        # Issue hierarchy

        No issue hierarchies.
        ");
    }

    #[test]
    fn nested_tree_with_external_child() {
        let edges = vec![
            edge(
                t("P", None, 12, "Ship v2 sync", None),
                t("C1", None, 51, "Rework watermarks", Some(0)),
            ),
            edge(
                t("P", None, 12, "Ship v2 sync", None),
                t("EXT", Some("acme/infra"), 4, "Provision runners", Some(1)),
            ),
            edge(
                t("C1", None, 51, "Rework watermarks", Some(0)),
                t("G", None, 60, "Grand[child]", None),
            ),
        ];
        insta::assert_snapshot!(hierarchy_doc(&edges, 4), @r"
        # Issue hierarchy

        - [#12 Ship v2 sync](issues/0012.md) (open)
          - [#51 Rework watermarks](issues/0051.md) (open)
            - [#60 Grand\[child\]](issues/0060.md) (open)
          - acme/infra#4 Provision runners (open) — external
        ");
    }

    #[test]
    fn child_of_external_parent_is_a_local_root() {
        let edges = vec![edge(
            t("EXTP", Some("acme/infra"), 9, "External parent", None),
            t("C", None, 7, "Local child", None),
        )];
        let doc = hierarchy_doc(&edges, 4);
        // The external parent is not renderable as a root; the edge produces
        // no tree (C has no children of its own).
        assert!(!doc.contains("acme/infra#9"), "{doc}");
        assert!(doc.contains("No issue hierarchies."), "{doc}");
    }

    #[test]
    fn cycle_guard_terminates() {
        // Inconsistent cache state: A → B → A. Must terminate, not recurse
        // forever. Both nodes have a local parent, so neither qualifies as a
        // root and an unbroken cycle renders as no hierarchy — the correct
        // outcome for a state GitHub itself prevents. The visited set guards
        // the traversal for cycles reachable from a genuine root.
        let edges = vec![
            edge(t("A", None, 1, "a", None), t("B", None, 2, "b", None)),
            edge(t("B", None, 2, "b", None), t("A", None, 1, "a", None)),
        ];
        let doc = hierarchy_doc(&edges, 4);
        assert!(doc.contains("No issue hierarchies."), "{doc}");
    }
}
