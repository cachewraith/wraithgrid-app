//! Lanes for the Changes panel's history graph (port of `src/renderer/lib/commit-graph.ts`):
//! each commit gets a column, and each row says which lines cross its top half (from the row
//! above into this commit, or straight through) and its bottom half (from this commit to its
//! parents' lanes, or straight through). Pure, so a page of commits lays out the same every time.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::history::GitCommit;

/// What the layout needs of a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphCommit {
    pub sha: String,
    pub parents: Vec<String>,
}

impl From<&GitCommit> for GraphCommit {
    fn from(c: &GitCommit) -> Self {
        Self {
            sha: c.sha.clone(),
            parents: c.parents.clone(),
        }
    }
}

/// A line inside one half of a row, from column `from` (top) to column `to` (bottom).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    /// Lane color index.
    pub color: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphRow {
    pub col: usize,
    pub color: usize,
    pub top: Vec<Edge>,
    pub bottom: Vec<Edge>,
    /// Columns in use on this row, for the graph's width.
    pub width: usize,
}

#[derive(Debug, Clone)]
struct Lane {
    sha: String,
    color: usize,
}

/// Lanes by column; None is a free column. Writing past the end grows it, as a JS array does.
struct Lanes(Vec<Option<Lane>>);

impl Lanes {
    fn set(&mut self, i: usize, lane: Option<Lane>) {
        if i >= self.0.len() {
            self.0.resize(i + 1, None);
        }
        self.0[i] = lane;
    }

    fn find(&self, sha: &str) -> Option<usize> {
        self.0
            .iter()
            .position(|l| l.as_ref().is_some_and(|l| l.sha == sha))
    }

    fn color(&self, i: usize) -> usize {
        self.0[i].as_ref().map_or(0, |l| l.color)
    }

    /// The first free column, or a new one at the end.
    fn free(&self) -> usize {
        self.0
            .iter()
            .position(Option::is_none)
            .unwrap_or(self.0.len())
    }
}

pub fn layout_graph(commits: &[GraphCommit]) -> Vec<GraphRow> {
    let mut lanes = Lanes(Vec::new());
    let mut next_color = 0;
    commits
        .iter()
        .map(|c| {
            // Top half: lanes waiting for this commit run into it (the first one is its
            // column); every other lane goes straight through.
            let found = lanes.find(&c.sha);
            let tip = found.is_none();
            let mut top = Vec::new();
            for i in 0..lanes.0.len() {
                let Some(l) = &lanes.0[i] else { continue };
                let color = l.color;
                if l.sha == c.sha {
                    // `found` is the first lane with this sha, so it is set here.
                    let col = found.unwrap_or(i);
                    top.push(Edge {
                        from: i,
                        to: col,
                        color,
                    });
                    if i != col {
                        lanes.0[i] = None;
                    }
                } else {
                    top.push(Edge {
                        from: i,
                        to: i,
                        color,
                    });
                }
            }
            // A branch tip nobody above points at starts at its node, in the first free column.
            let col = found.unwrap_or_else(|| lanes.free());
            let color = if tip {
                next_color += 1;
                next_color - 1
            } else {
                lanes.color(col)
            };
            lanes.set(col, None);

            // Bottom half: to the first parent (in this lane, or into the lane that already
            // waits for it), to every other parent (a merge), and straight through for the rest.
            let mut bottom = Vec::new();
            let mut created = HashSet::new();
            let mut parents = c.parents.iter();
            if let Some(first) = parents.next() {
                let own = Lane {
                    sha: first.clone(),
                    color,
                };
                match lanes.find(first) {
                    Some(at) if at < col => bottom.push(Edge {
                        from: col,
                        to: at,
                        color: lanes.color(at),
                    }),
                    Some(at) if at > col => {
                        // The lane to the right joins this one, so the main line stays in the
                        // leftmost column.
                        bottom.push(Edge {
                            from: at,
                            to: col,
                            color: lanes.color(at),
                        });
                        lanes.set(at, None);
                        lanes.set(col, Some(own));
                        created.insert(col);
                        bottom.push(Edge {
                            from: col,
                            to: col,
                            color,
                        });
                    }
                    _ => {
                        lanes.set(col, Some(own));
                        created.insert(col);
                        bottom.push(Edge {
                            from: col,
                            to: col,
                            color,
                        });
                    }
                }
            }
            for p in parents {
                let at = match lanes.find(p) {
                    Some(at) => at,
                    None => {
                        let at = lanes.free();
                        lanes.set(
                            at,
                            Some(Lane {
                                sha: p.clone(),
                                color: next_color,
                            }),
                        );
                        next_color += 1;
                        created.insert(at);
                        at
                    }
                };
                bottom.push(Edge {
                    from: col,
                    to: at,
                    color: lanes.color(at),
                });
            }
            for (i, l) in lanes.0.iter().enumerate() {
                if let Some(l) = l {
                    if !created.contains(&i) {
                        bottom.push(Edge {
                            from: i,
                            to: i,
                            color: l.color,
                        });
                    }
                }
            }
            while lanes.0.last().is_some_and(Option::is_none) {
                lanes.0.pop();
            }
            let width = top
                .iter()
                .chain(&bottom)
                .flat_map(|e| [e.from, e.to])
                .fold(col, usize::max)
                + 1;
            GraphRow {
                col,
                color,
                top,
                bottom,
                width,
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    Head,
    Branch,
    Remote,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefLabel {
    pub text: String,
    pub kind: RefKind,
}

/// Labels from git's decoration: `HEAD -> main`, `origin/main`, `tag: v1.2.0`.
pub fn ref_labels(refs: &[String]) -> Vec<RefLabel> {
    refs.iter()
        .map(|r| {
            let (text, kind) = if let Some(b) = r.strip_prefix("HEAD -> ") {
                (b, RefKind::Head)
            } else if let Some(t) = r.strip_prefix("tag: ") {
                (t, RefKind::Tag)
            } else if r.contains('/') {
                (r.as_str(), RefKind::Remote)
            } else {
                (r.as_str(), RefKind::Branch)
            };
            RefLabel {
                text: text.to_string(),
                kind,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(sha: &str, parents: &[&str]) -> GraphCommit {
        GraphCommit {
            sha: sha.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn edges(e: &[Edge]) -> Vec<String> {
        let mut v: Vec<String> = e.iter().map(|x| format!("{}>{}", x.from, x.to)).collect();
        v.sort();
        v
    }

    fn cols(rows: &[GraphRow]) -> Vec<usize> {
        rows.iter().map(|r| r.col).collect()
    }

    #[test]
    fn a_straight_line_stays_in_column_0() {
        let rows = layout_graph(&[c("a", &["b"]), c("b", &["c"]), c("c", &[])]);
        assert_eq!(cols(&rows), [0, 0, 0]);
        assert!(edges(&rows[0].top).is_empty()); // the tip starts at its node
        assert_eq!(edges(&rows[0].bottom), ["0>0"]);
        assert!(edges(&rows[2].bottom).is_empty()); // the root ends at its node
        assert!(rows.iter().all(|r| r.width == 1));
    }

    #[test]
    fn a_merge_opens_a_lane_for_the_second_parent_and_it_joins_back_at_the_fork() {
        // m merges f1 (feature) into b1 (main); both started at b0.
        let rows = layout_graph(&[
            c("m", &["b1", "f1"]),
            c("f1", &["b0"]),
            c("b1", &["b0"]),
            c("b0", &[]),
        ]);
        assert_eq!(cols(&rows), [0, 1, 0, 0]);
        assert_eq!(edges(&rows[0].bottom), ["0>0", "0>1"]);
        assert_eq!(edges(&rows[1].top), ["0>0", "1>1"]);
        // b1's parent b0 already waits in lane 1: that lane comes back to column 0.
        assert_eq!(edges(&rows[2].bottom), ["0>0", "1>0"]);
        assert_eq!(edges(&rows[3].top), ["0>0"]);
        assert_eq!(rows[1].width, 2);
        // Lanes keep a color: the feature lane differs from main.
        assert_ne!(rows[1].color, rows[0].color);
    }

    #[test]
    fn two_branch_tips_the_second_starts_in_a_new_column() {
        let rows = layout_graph(&[c("x", &["base"]), c("y", &["base"]), c("base", &[])]);
        assert_eq!(cols(&rows), [0, 1, 0]);
        assert_eq!(edges(&rows[1].top), ["0>0"]);
        // y's parent already has a lane: its line joins it right away.
        assert_eq!(edges(&rows[1].bottom), ["0>0", "1>0"]);
        assert_eq!(edges(&rows[2].top), ["0>0"]);
    }

    #[test]
    fn labels_refs_by_kind() {
        let refs = ["HEAD -> main", "origin/main", "tag: v1.2.0", "feat"].map(String::from);
        let label = |text: &str, kind| RefLabel {
            text: text.into(),
            kind,
        };
        assert_eq!(
            ref_labels(&refs),
            [
                label("main", RefKind::Head),
                label("origin/main", RefKind::Remote),
                label("v1.2.0", RefKind::Tag),
                label("feat", RefKind::Branch),
            ]
        );
        assert_eq!(
            serde_json::to_string(&ref_labels(&refs[..1])).unwrap(),
            r#"[{"text":"main","kind":"head"}]"#
        );
    }
}
