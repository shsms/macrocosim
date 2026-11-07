//! The order components are built in: children before parents, so a
//! battery is ticked before the inverter that pushes into it. Shared
//! by the spec build and the site import.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// `ids` ordered children first over the parent → child `edges`.
///
/// A post-order walk: every child of a component comes before it. A
/// component in no edge is its own whole subtree, so it lands where
/// the walk over `ids` reaches it, interleaved with the connected
/// ones, not collected at the end. A component under two parents
/// comes once. `edges` must have no cycle (see [`cycle_edge`]).
pub fn children_first(ids: impl IntoIterator<Item = u64>, edges: &[(u64, u64)]) -> Vec<u64> {
    children_first_counting(ids, edges).0
}

/// [`children_first`], with the number of `visit_post_order` calls it
/// took.
///
/// The count is for the site import's
/// `forms_walk_shared_subtrees_once`. Losing the entry memo in
/// `visit_post_order` does not change the order this returns, only
/// how long it takes: on a chain of 25 diamonds the walk still
/// finishes, in minutes, with exactly the same answer. So a test that
/// reads the output alone stays green on the regressed walk, and the
/// count is the one cheap thing that does not.
pub(crate) fn children_first_counting(
    ids: impl IntoIterator<Item = u64>,
    edges: &[(u64, u64)],
) -> (Vec<u64>, u64) {
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    for &(from, to) in edges {
        children.entry(from).or_default().push(to);
    }
    let mut ordered = Vec::new();
    let mut seen = HashSet::new();
    let mut calls = 0;
    for id in ids {
        visit_post_order(id, &children, &mut seen, &mut ordered, &mut calls);
    }
    (ordered, calls)
}

/// Depth-first post-order visit over the parent → child edges: every
/// child of `id` is emitted before `id` itself. `seen` stops a
/// component reached from more than one parent, or through more than
/// one path, from being pushed twice — and, because it is also
/// checked on entry, from being *descended into* twice. That entry
/// check is what keeps the walk linear: a component is marked only
/// once its whole subtree has been emitted, so meeting a marked id
/// again means that subtree is already in `ordered` and re-walking it
/// would only rediscover the same nodes. Without it a chain of
/// diamonds re-walks every shared subtree once per path into it,
/// which is exponential in the chain's length. The callers have
/// already rejected cycles, so the entry check is a performance
/// guard, not the recursion's base case.
///
/// `calls` counts every entry, including the ones the memo turns
/// straight back — that count is what the test asserts on, since the
/// memo changes only the walk's cost, never its output.
fn visit_post_order(
    id: u64,
    children: &HashMap<u64, Vec<u64>>,
    seen: &mut HashSet<u64>,
    ordered: &mut Vec<u64>,
    calls: &mut u64,
) {
    *calls += 1;
    if seen.contains(&id) {
        return;
    }
    if let Some(kids) = children.get(&id) {
        for &child in kids {
            visit_post_order(child, children, seen, ordered, calls);
        }
    }
    if seen.insert(id) {
        ordered.push(id);
    }
}

/// Kahn topological sort over the connection edges; on a cycle,
/// returns an edge between two of the unresolvable nodes so the error
/// can name where to look.
pub fn cycle_edge(edges: &[(u64, u64)]) -> Option<(u64, u64)> {
    let mut indegree: BTreeMap<u64, usize> = BTreeMap::new();
    let mut out: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for &(src, dst) in edges {
        indegree.entry(src).or_default();
        *indegree.entry(dst).or_default() += 1;
        out.entry(src).or_default().push(dst);
    }
    let mut queue: VecDeque<u64> = indegree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| *n)
        .collect();
    while let Some(n) = queue.pop_front() {
        for &m in out.get(&n).into_iter().flatten() {
            let d = indegree.get_mut(&m).expect("edge target has an indegree");
            *d -= 1;
            if *d == 0 {
                queue.push_back(m);
            }
        }
        indegree.remove(&n);
    }
    edges
        .iter()
        .copied()
        .find(|(s, d)| indegree.contains_key(s) && indegree.contains_key(d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn children_come_before_their_parents() {
        // 1 → 2 → 3, and 4 on its own between them in the id list.
        let order = children_first([1, 4, 2, 3], &[(1, 2), (2, 3)]);
        assert_eq!(order, vec![3, 2, 1, 4]);
    }

    #[test]
    fn a_shared_child_comes_once_before_both_parents() {
        // 2 and 3 both feed 4.
        let order = children_first([1, 2, 3, 4], &[(1, 2), (1, 3), (2, 4), (3, 4)]);
        assert_eq!(order, vec![4, 2, 3, 1]);
    }

    #[test]
    fn a_cycle_is_found_and_a_tree_has_none() {
        assert_eq!(cycle_edge(&[(1, 2), (2, 3)]), None);
        let edge = cycle_edge(&[(1, 2), (2, 3), (3, 2)]).expect("a cycle");
        assert!([(2, 3), (3, 2)].contains(&edge), "{edge:?}");
    }
}
