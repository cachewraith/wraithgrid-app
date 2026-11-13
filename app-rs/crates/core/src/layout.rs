//! Pure operations on a workspace's split tree (`src/renderer/layout/tree.ts`).

use std::collections::HashSet;

use crate::config::LayoutNode;
use crate::types::SplitDir;

/// A leaf: a pane or an empty slot.
#[derive(Clone, Debug, PartialEq)]
pub enum Leaf {
    Pane(String),
    Empty(String),
}

/// Leaves (panes and empty slots) in reading order.
pub fn leaves(node: Option<&LayoutNode>) -> Vec<Leaf> {
    let mut out = Vec::new();
    fn walk(n: &LayoutNode, out: &mut Vec<Leaf>) {
        match n {
            LayoutNode::Pane { pane_id } => out.push(Leaf::Pane(pane_id.clone())),
            LayoutNode::Empty { slot_id } => out.push(Leaf::Empty(slot_id.clone())),
            LayoutNode::Split { children, .. } => children.iter().for_each(|c| walk(c, out)),
        }
    }
    if let Some(n) = node {
        walk(n, &mut out);
    }
    out
}

pub fn pane_ids(node: Option<&LayoutNode>) -> Vec<String> {
    leaves(node)
        .into_iter()
        .filter_map(|l| {
            if let Leaf::Pane(id) = l {
                Some(id)
            } else {
                None
            }
        })
        .collect()
}

pub fn empty_slot_ids(node: Option<&LayoutNode>) -> Vec<String> {
    leaves(node)
        .into_iter()
        .filter_map(|l| {
            if let Leaf::Empty(id) = l {
                Some(id)
            } else {
                None
            }
        })
        .collect()
}

/// Scales sizes so they sum to 100, falling back to equal shares.
pub fn normalize_sizes(sizes: &[f64], count: usize) -> Vec<f64> {
    let valid = sizes.len() == count && sizes.iter().all(|s| s.is_finite() && *s > 0.0);
    if !valid {
        return vec![100.0 / count as f64; count];
    }
    let total: f64 = sizes.iter().sum();
    sizes.iter().map(|s| s / total * 100.0).collect()
}

pub fn split(dir: SplitDir, children: Vec<LayoutNode>, sizes: &[f64]) -> LayoutNode {
    let sizes = normalize_sizes(sizes, children.len());
    LayoutNode::Split {
        dir,
        sizes,
        children,
    }
}

fn pane(id: &str) -> LayoutNode {
    LayoutNode::Pane { pane_id: id.into() }
}

/// Drops leaves `keep` rejects, removes duplicate panes, collapses splits with a single
/// child and repairs sizes. None when nothing is left.
pub fn prune(node: Option<&LayoutNode>, keep: &dyn Fn(&Leaf) -> bool) -> Option<LayoutNode> {
    fn walk(
        n: &LayoutNode,
        keep: &dyn Fn(&Leaf) -> bool,
        seen: &mut HashSet<String>,
    ) -> Option<LayoutNode> {
        match n {
            LayoutNode::Pane { pane_id } => {
                if seen.contains(pane_id) || !keep(&Leaf::Pane(pane_id.clone())) {
                    return None;
                }
                seen.insert(pane_id.clone());
                Some(n.clone())
            }
            LayoutNode::Empty { slot_id } => keep(&Leaf::Empty(slot_id.clone())).then(|| n.clone()),
            LayoutNode::Split {
                dir,
                sizes,
                children,
            } => {
                let kept: Vec<(LayoutNode, f64)> = children
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| {
                        walk(c, keep, seen).map(|w| (w, sizes.get(i).copied().unwrap_or(0.0)))
                    })
                    .collect();
                match kept.len() {
                    0 => None,
                    1 => Some(kept.into_iter().next().unwrap().0),
                    _ => {
                        let (kids, sz): (Vec<_>, Vec<_>) = kept.into_iter().unzip();
                        Some(split(*dir, kids, &sz))
                    }
                }
            }
        }
    }
    node.and_then(|n| walk(n, keep, &mut HashSet::new()))
}

/// Keeps only panes the workspace still has.
pub fn normalize_layout(node: Option<&LayoutNode>, valid: &[String]) -> Option<LayoutNode> {
    prune(node, &|l| match l {
        Leaf::Empty(_) => true,
        Leaf::Pane(id) => valid.contains(id),
    })
}

pub fn remove_pane(node: Option<&LayoutNode>, pane_id: &str) -> Option<LayoutNode> {
    prune(node, &|l| l != &Leaf::Pane(pane_id.into()))
}

fn map_leaves(node: &LayoutNode, f: &dyn Fn(&LayoutNode) -> LayoutNode) -> LayoutNode {
    match node {
        LayoutNode::Split {
            dir,
            sizes,
            children,
        } => LayoutNode::Split {
            dir: *dir,
            sizes: sizes.clone(),
            children: children.iter().map(|c| map_leaves(c, f)).collect(),
        },
        leaf => f(leaf),
    }
}

pub fn fill_slot(node: &LayoutNode, slot: &str, pane_id: &str) -> LayoutNode {
    map_leaves(node, &|l| match l {
        LayoutNode::Empty { slot_id } if slot_id == slot => pane(pane_id),
        other => other.clone(),
    })
}

pub fn swap_panes(node: &LayoutNode, a: &str, b: &str) -> LayoutNode {
    map_leaves(node, &|l| match l {
        LayoutNode::Pane { pane_id } if pane_id == a => pane(b),
        LayoutNode::Pane { pane_id } if pane_id == b => pane(a),
        other => other.clone(),
    })
}

/// Puts `new_id` next to `target`. When the target already sits in a split of the same
/// direction the new pane becomes a sibling (sharing the target's space); otherwise the
/// target leaf becomes a two-way split.
pub fn split_pane(node: &LayoutNode, target: &str, new_id: &str, dir: SplitDir) -> LayoutNode {
    match node {
        LayoutNode::Pane { pane_id } if pane_id == target => {
            split(dir, vec![node.clone(), pane(new_id)], &[])
        }
        LayoutNode::Split {
            dir: d,
            sizes,
            children,
        } => {
            let idx = children
                .iter()
                .position(|c| matches!(c, LayoutNode::Pane { pane_id } if pane_id == target));
            match idx {
                Some(i) if *d == dir => {
                    let mut kids = children.clone();
                    let mut sz = sizes.clone();
                    let half = sz.get(i).copied().unwrap_or(0.0) / 2.0;
                    kids.insert(i + 1, pane(new_id));
                    sz.splice(i..i + 1, [half, half]);
                    split(dir, kids, &sz)
                }
                _ => LayoutNode::Split {
                    dir: *d,
                    sizes: sizes.clone(),
                    children: children
                        .iter()
                        .map(|c| split_pane(c, target, new_id, dir))
                        .collect(),
                },
            }
        }
        other => other.clone(),
    }
}

/// Stores a split's sizes after the user drags a divider.
pub fn set_sizes_at(node: &LayoutNode, path: &[usize], new_sizes: &[f64]) -> LayoutNode {
    match (node, path.split_first()) {
        (LayoutNode::Split { dir, children, .. }, None) if new_sizes.len() == children.len() => {
            LayoutNode::Split {
                dir: *dir,
                sizes: normalize_sizes(new_sizes, children.len()),
                children: children.clone(),
            }
        }
        (
            LayoutNode::Split {
                dir,
                sizes,
                children,
            },
            Some((head, rest)),
        ) => LayoutNode::Split {
            dir: *dir,
            sizes: sizes.clone(),
            children: children
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    if i == *head {
                        set_sizes_at(c, rest, new_sizes)
                    } else {
                        c.clone()
                    }
                })
                .collect(),
        },
        _ => node.clone(),
    }
}

/// An even grid for any number of panes: 1, 2 and 3 in one row, 4 as 2×2, more as two
/// rows (8 → 4×2).
pub fn auto_grid(ids: &[String]) -> Option<LayoutNode> {
    let cells: Vec<LayoutNode> = ids.iter().map(|id| pane(id)).collect();
    match cells.len() {
        0 => None,
        1 => cells.into_iter().next(),
        2 | 3 => Some(split(SplitDir::Row, cells, &[])),
        n => {
            let per_row = n.div_ceil(2);
            let rows = [cells[..per_row].to_vec(), cells[per_row..].to_vec()]
                .into_iter()
                .map(|r| {
                    if r.len() == 1 {
                        r.into_iter().next().unwrap()
                    } else {
                        split(SplitDir::Row, r, &[])
                    }
                })
                .collect();
            Some(split(SplitDir::Col, rows, &[]))
        }
    }
}

/// Changes whenever the tree's shape changes, but not when only sizes change.
pub fn shape_key(n: &LayoutNode) -> String {
    match n {
        LayoutNode::Pane { pane_id } => format!("p-{pane_id}"),
        LayoutNode::Empty { slot_id } => format!("e-{slot_id}"),
        LayoutNode::Split { dir, children, .. } => {
            format!(
                "{dir}({})",
                children.iter().map(shape_key).collect::<Vec<_>>().join(",")
            )
        }
    }
}

/// True when the tree has the shape `auto_grid` would give its panes (sizes aside).
pub fn is_regular_grid(node: Option<&LayoutNode>) -> bool {
    let Some(n) = node else { return false };
    if !empty_slot_ids(node).is_empty() {
        return false;
    }
    auto_grid(&pane_ids(node)).is_some_and(|g| shape_key(&g) == shape_key(n))
}

/// Where a new pane goes: the requested empty slot, else the first empty slot; a regular
/// grid is re-arranged to stay regular; a custom tree splits the target pane.
pub fn place_new_pane(
    node: Option<&LayoutNode>,
    pane_id: &str,
    slot: Option<&str>,
    target: Option<&str>,
    dir: SplitDir,
) -> LayoutNode {
    let Some(n) = node else { return pane(pane_id) };
    let slots = empty_slot_ids(node);
    if let Some(s) = slot.filter(|s| slots.iter().any(|x| x == s)) {
        return fill_slot(n, s, pane_id);
    }
    if let Some(first) = slots.first() {
        return fill_slot(n, first, pane_id);
    }
    let visible = pane_ids(node);
    if visible.is_empty() {
        return pane(pane_id);
    }
    if is_regular_grid(node) {
        let mut ids = visible;
        ids.push(pane_id.into());
        return auto_grid(&ids).unwrap();
    }
    let target = target
        .filter(|t| visible.iter().any(|v| v == t))
        .map(String::from)
        .unwrap_or_else(|| visible.last().unwrap().clone());
    split_pane(n, &target, pane_id, dir)
}

/// What a workspace shows: its saved tree without stale panes, or an even grid of its panes.
pub fn shown_layout(saved: Option<&LayoutNode>, pane_ids: &[String]) -> Option<LayoutNode> {
    normalize_layout(saved, pane_ids).or_else(|| auto_grid(pane_ids))
}

/// The top bar's layout presets (`PRESETS`): 1 pane, 2 side by side, 2×2, 3 columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    One,
    Two,
    Four,
    Three,
}

impl Preset {
    pub const ALL: [Preset; 4] = [Preset::One, Preset::Two, Preset::Four, Preset::Three];

    pub fn slots(self) -> usize {
        match self {
            Preset::One => 1,
            Preset::Two => 2,
            Preset::Four => 4,
            Preset::Three => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Preset::One => "1 pane",
            Preset::Two => "2 side by side",
            Preset::Four => "2×2 grid",
            Preset::Three => "3 columns",
        }
    }
}

/// `buildPreset`: panes fill the slots in order; leftover panes stay in the project, hidden;
/// missing ones become empty "New pane here" slots.
pub fn build_preset(
    preset: Preset,
    ordered: &[String],
    mut new_slot_id: impl FnMut() -> String,
) -> LayoutNode {
    let mut cells: Vec<LayoutNode> = (0..preset.slots())
        .map(|i| match ordered.get(i) {
            Some(id) => pane(id),
            None => LayoutNode::Empty {
                slot_id: new_slot_id(),
            },
        })
        .collect();
    match preset {
        Preset::One => cells.remove(0),
        Preset::Two | Preset::Three => split(SplitDir::Row, cells, &[]),
        Preset::Four => {
            let bottom = cells.split_off(2);
            split(
                SplitDir::Col,
                vec![
                    split(SplitDir::Row, cells, &[]),
                    split(SplitDir::Row, bottom, &[]),
                ],
                &[],
            )
        }
    }
}

/// `detectPreset`: which preset the tree is exactly (shape and even sizes), if any.
pub fn detect_preset(node: Option<&LayoutNode>) -> Option<Preset> {
    let leaf = |n: &LayoutNode| !matches!(n, LayoutNode::Split { .. });
    let evenly = |sizes: &[f64]| {
        sizes
            .iter()
            .all(|s| (s - 100.0 / sizes.len() as f64).abs() < 0.5)
    };
    match node? {
        n if leaf(n) => Some(Preset::One),
        LayoutNode::Split {
            dir: SplitDir::Row,
            sizes,
            children,
        } if children.iter().all(leaf) && evenly(sizes) => match children.len() {
            2 => Some(Preset::Two),
            3 => Some(Preset::Three),
            _ => None,
        },
        LayoutNode::Split {
            dir: SplitDir::Col,
            sizes,
            children,
        } if children.len() == 2
            && evenly(sizes)
            && children.iter().all(|r| {
                matches!(r, LayoutNode::Split { dir: SplitDir::Row, sizes, children }
                    if children.len() == 2 && children.iter().all(leaf) && evenly(sizes))
            }) =>
        {
            Some(Preset::Four)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_build_and_detect() {
        let ids: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let mut n = 0;
        let mut slot = || {
            n += 1;
            format!("s{n}")
        };
        let four = build_preset(Preset::Four, &ids, &mut slot);
        assert_eq!(pane_ids(Some(&four)), ["a", "b", "c"]);
        assert_eq!(empty_slot_ids(Some(&four)), ["s1"]);
        assert_eq!(detect_preset(Some(&four)), Some(Preset::Four));
        let two = build_preset(Preset::Two, &ids, &mut slot);
        assert_eq!(pane_ids(Some(&two)), ["a", "b"]);
        assert_eq!(detect_preset(Some(&two)), Some(Preset::Two));
        assert_eq!(
            detect_preset(Some(&build_preset(Preset::One, &ids, &mut slot))),
            Some(Preset::One)
        );
        assert_eq!(
            detect_preset(Some(&build_preset(Preset::Three, &ids, &mut slot))),
            Some(Preset::Three)
        );
        let uneven = split(SplitDir::Row, vec![pane("a"), pane("b")], &[70.0, 30.0]);
        assert_eq!(detect_preset(Some(&uneven)), None);
        assert_eq!(detect_preset(None), None);
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn grids() {
        assert_eq!(shape_key(&auto_grid(&ids(&["a"])).unwrap()), "p-a");
        assert_eq!(
            shape_key(&auto_grid(&ids(&["a", "b", "c"])).unwrap()),
            "row(p-a,p-b,p-c)"
        );
        assert_eq!(
            shape_key(&auto_grid(&ids(&["a", "b", "c", "d"])).unwrap()),
            "col(row(p-a,p-b),row(p-c,p-d))"
        );
        assert_eq!(
            shape_key(&auto_grid(&ids(&["a", "b", "c", "d", "e"])).unwrap()),
            "col(row(p-a,p-b,p-c),row(p-d,p-e))"
        );
        assert!(auto_grid(&[]).is_none());
    }

    #[test]
    fn prune_and_normalize() {
        let t = split(
            SplitDir::Row,
            vec![pane("a"), pane("b"), pane("a")],
            &[20.0, 30.0, 50.0],
        );
        let n = normalize_layout(Some(&t), &ids(&["a"])).unwrap();
        assert_eq!(n, pane("a"));
        let n = normalize_layout(Some(&t), &ids(&["a", "b"])).unwrap();
        let LayoutNode::Split { sizes, .. } = &n else {
            panic!()
        };
        assert_eq!(sizes, &vec![40.0, 60.0]);
        assert_eq!(remove_pane(Some(&pane("a")), "a"), None);
    }

    #[test]
    fn placing_panes() {
        let grid = auto_grid(&ids(&["a", "b", "c"])).unwrap();
        assert_eq!(
            shape_key(&place_new_pane(Some(&grid), "d", None, None, SplitDir::Row)),
            "col(row(p-a,p-b),row(p-c,p-d))"
        );
        let custom = split(SplitDir::Col, vec![pane("a"), pane("b")], &[70.0, 30.0]);
        let placed = place_new_pane(Some(&custom), "c", None, Some("a"), SplitDir::Col);
        assert_eq!(shape_key(&placed), "col(p-a,p-c,p-b)");
        let LayoutNode::Split { sizes, .. } = &placed else {
            panic!()
        };
        assert_eq!(sizes, &vec![35.0, 35.0, 30.0]);
        let slotted = split(
            SplitDir::Row,
            vec![
                pane("a"),
                LayoutNode::Empty {
                    slot_id: "s".into(),
                },
            ],
            &[],
        );
        assert_eq!(
            shape_key(&place_new_pane(
                Some(&slotted),
                "z",
                None,
                None,
                SplitDir::Row
            )),
            "row(p-a,p-z)"
        );
    }

    #[test]
    fn sizes_and_swap() {
        let t = split(
            SplitDir::Row,
            vec![
                pane("a"),
                split(SplitDir::Col, vec![pane("b"), pane("c")], &[]),
            ],
            &[],
        );
        let t2 = set_sizes_at(&t, &[1], &[1.0, 3.0]);
        let LayoutNode::Split { children, .. } = &t2 else {
            panic!()
        };
        let LayoutNode::Split { sizes, .. } = &children[1] else {
            panic!()
        };
        assert_eq!(sizes, &vec![25.0, 75.0]);
        assert_eq!(
            shape_key(&swap_panes(&t, "a", "c")),
            "row(p-c,col(p-b,p-a))"
        );
    }
}
