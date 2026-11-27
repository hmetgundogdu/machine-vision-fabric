//! Graph layout for the TUI (#35).
//!
//! It keeps the legacy dashboard's picture: nodes in layers from left to right, each node in
//! the layer of its longest path from a source, boxes 22 × 5, edge columns 18 wide. Three
//! things are improved:
//! - **Long edges:** an edge that skips layers passes through dummy slots instead of crossing
//!   boxes.
//! - **Ordering:** each layer is ordered by the barycentre of its neighbours, so fewer edges cross.
//! - **Routing:** every edge gets its own vertical track in the column, and fans in and out over a
//!   box's three inner rows.
//!
//! The whole layout is pure and computed once per topology.

use std::collections::VecDeque;

/// Node box width, borders included.
pub const BOX_W: u16 = 22;
/// Node box height, borders included.
pub const BOX_H: u16 = 5;
/// Width of the column between two layers.
pub const EDGE_W: u16 = 18;
/// Rows between two items of a layer.
pub const PAD: u16 = 1;

/// An edge between two nodes, by index.
#[derive(Debug, Clone)]
pub struct EdgeIn {
    /// Source node.
    pub from: usize,
    /// Target node.
    pub to: usize,
    /// A control edge (drawn with `=`).
    pub control: bool,
    /// The source port, shown on the edge.
    pub label: String,
}

/// A slot in a layer: a node box, or a dummy an edge passes through.
#[derive(Debug, Clone)]
pub struct Item {
    /// The node, or `None` for a dummy.
    pub node: Option<usize>,
    /// Layer index.
    pub layer: usize,
    /// Top row.
    pub y: u16,
    /// Height in rows.
    pub h: u16,
}

/// One drawn cell of an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// Column.
    pub x: u16,
    /// Row.
    pub y: u16,
    /// ASCII glyph.
    pub ch: char,
}

/// An edge's drawing: its cells from source to target, and a label.
#[derive(Debug, Clone)]
pub struct Route {
    /// Index into the input edges.
    pub edge: usize,
    /// Cells in order from the source box to the arrow.
    pub cells: Vec<Cell>,
    /// (x, y, text) of the source-port label, when there is room.
    pub label: Option<(u16, u16, String)>,
}

/// The computed layout.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// Every slot.
    pub items: Vec<Item>,
    /// Item indices per layer, top to bottom.
    pub layers: Vec<Vec<usize>>,
    /// Node index → item index.
    pub node_item: Vec<usize>,
    /// One route per drawable edge.
    pub routes: Vec<Route>,
    /// Canvas width.
    pub width: u16,
    /// Canvas height.
    pub height: u16,
    /// Nodes in reading order (layer by layer, top to bottom): what ←/→ walk.
    pub order: Vec<usize>,
}

/// Left column of `layer`.
pub fn x_of(layer: usize) -> u16 {
    (layer as u16).saturating_mul(BOX_W + EDGE_W)
}

impl Layout {
    /// Lays out `n` nodes (in declaration order) and their edges.
    pub fn build(n: usize, edges: &[EdgeIn]) -> Self {
        // Layers: the longest path from a source (Kahn, in declaration order).
        let mut indeg = vec![0usize; n];
        let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
        for e in edges {
            if e.from < n && e.to < n && e.from != e.to {
                out[e.from].push(e.to);
                indeg[e.to] += 1;
            }
        }
        let mut layer = vec![0usize; n];
        let mut queue: VecDeque<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
        while let Some(u) = queue.pop_front() {
            for &v in &out[u] {
                layer[v] = layer[v].max(layer[u] + 1);
                indeg[v] -= 1;
                if indeg[v] == 0 {
                    queue.push_back(v);
                }
            }
        }
        let layer_count = layer.iter().copied().max().map_or(0, |m| m + 1);

        // Items: nodes, then a dummy per skipped layer of every long edge.
        let mut items: Vec<Item> = (0..n)
            .map(|i| Item {
                node: Some(i),
                layer: layer[i],
                y: 0,
                h: BOX_H,
            })
            .collect();
        let node_item: Vec<usize> = (0..n).collect();
        // (from item, to item, edge)
        let mut segments: Vec<(usize, usize, usize)> = Vec::new();
        for (k, e) in edges.iter().enumerate() {
            if e.from >= n || e.to >= n || layer[e.to] <= layer[e.from] {
                continue;
            }
            let mut prev = node_item[e.from];
            for l in layer[e.from] + 1..layer[e.to] {
                items.push(Item {
                    node: None,
                    layer: l,
                    y: 0,
                    h: 1,
                });
                let d = items.len() - 1;
                segments.push((prev, d, k));
                prev = d;
            }
            segments.push((prev, node_item[e.to], k));
        }

        // Order each layer: start in creation order, then barycentre sweeps down and up.
        let mut layers: Vec<Vec<usize>> = vec![Vec::new(); layer_count];
        for (i, it) in items.iter().enumerate() {
            layers[it.layer].push(i);
        }
        let mut pos = vec![0f64; items.len()];
        let index = |layers: &Vec<Vec<usize>>, pos: &mut Vec<f64>| {
            for l in layers {
                for (p, &i) in l.iter().enumerate() {
                    pos[i] = p as f64;
                }
            }
        };
        index(&layers, &mut pos);
        for pass in 0..4 {
            let down = pass % 2 == 0;
            let range: Vec<usize> = if down {
                (1..layer_count).collect()
            } else {
                (0..layer_count.saturating_sub(1)).rev().collect()
            };
            for l in range {
                let mut keyed: Vec<(f64, usize)> = layers[l]
                    .iter()
                    .map(|&i| {
                        let near: Vec<f64> = segments
                            .iter()
                            .filter_map(|&(a, b, _)| match down {
                                true if b == i => Some(pos[a]),
                                false if a == i => Some(pos[b]),
                                _ => None,
                            })
                            .collect();
                        let key = if near.is_empty() {
                            pos[i]
                        } else {
                            near.iter().sum::<f64>() / near.len() as f64
                        };
                        (key, i)
                    })
                    .collect();
                keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
                layers[l] = keyed.into_iter().map(|(_, i)| i).collect();
                index(&layers, &mut pos);
            }
        }

        // Rows: stack each layer, centred on the tallest one.
        let heights: Vec<u16> = layers
            .iter()
            .map(|l| {
                l.iter()
                    .map(|&i| items[i].h + PAD)
                    .sum::<u16>()
                    .saturating_sub(PAD)
            })
            .collect();
        let height = heights.iter().copied().max().unwrap_or(0);
        for (l, list) in layers.iter().enumerate() {
            let mut y = (height - heights[l]) / 2;
            for &i in list {
                items[i].y = y;
                y += items[i].h + PAD;
            }
        }

        // Attachment rows: a box fans its edges over its three inner rows, sorted by the far end.
        let attach = |item: &Item, k: usize| -> u16 {
            match item.node {
                Some(_) => item.y + 1 + (k % 3) as u16,
                None => item.y,
            }
        };
        let mut from_row = vec![0u16; segments.len()];
        let mut to_row = vec![0u16; segments.len()];
        for i in 0..items.len() {
            let mut outs: Vec<usize> = (0..segments.len())
                .filter(|&s| segments[s].0 == i)
                .collect();
            outs.sort_by_key(|&s| items[segments[s].1].y);
            for (k, &s) in outs.iter().enumerate() {
                from_row[s] = attach(&items[i], k);
            }
            let mut ins: Vec<usize> = (0..segments.len())
                .filter(|&s| segments[s].1 == i)
                .collect();
            ins.sort_by_key(|&s| items[segments[s].0].y);
            for (k, &s) in ins.iter().enumerate() {
                to_row[s] = attach(&items[i], k);
            }
        }

        // Routing, column by column: each segment has its own vertical track.
        let mut cells_of: Vec<Vec<Cell>> = vec![Vec::new(); edges.len()];
        let mut labels: Vec<Option<(u16, u16, String)>> = vec![None; edges.len()];
        for l in 0..layer_count.saturating_sub(1) {
            let mut here: Vec<usize> = (0..segments.len())
                .filter(|&s| items[segments[s].0].layer == l)
                .collect();
            here.sort_by_key(|&s| (from_row[s], to_row[s]));
            let cs = x_of(l) + BOX_W;
            let ce = x_of(l + 1);
            let mut bends = 0u16;
            for &s in &here {
                let (a, b, k) = segments[s];
                let (sy, dy) = (from_row[s], to_row[s]);
                let control = edges[k].control;
                let hz = if control { '=' } else { '-' };
                let mut cells = Vec::new();
                // A dummy is crossed straight through its slot in the box column.
                if items[a].node.is_none() {
                    for x in x_of(l)..cs {
                        cells.push(Cell { x, y: sy, ch: hz });
                    }
                }
                let target_real = items[b].node.is_some();
                let last = if target_real { ce - 2 } else { ce - 1 };
                if sy == dy {
                    for x in cs..=last {
                        cells.push(Cell { x, y: sy, ch: hz });
                    }
                } else {
                    let tx = ce.saturating_sub(4 + 2 * bends).max(cs + 2);
                    bends += 1;
                    for x in cs..tx {
                        cells.push(Cell { x, y: sy, ch: hz });
                    }
                    cells.push(Cell {
                        x: tx,
                        y: sy,
                        ch: '+',
                    });
                    let (lo, hi) = (sy.min(dy), sy.max(dy));
                    let rows: Vec<u16> = if dy > sy {
                        (lo + 1..hi).collect()
                    } else {
                        (lo + 1..hi).rev().collect()
                    };
                    for y in rows {
                        cells.push(Cell { x: tx, y, ch: '|' });
                    }
                    cells.push(Cell {
                        x: tx,
                        y: dy,
                        ch: '+',
                    });
                    for x in tx + 1..=last {
                        cells.push(Cell { x, y: dy, ch: hz });
                    }
                }
                if target_real {
                    cells.push(Cell {
                        x: ce - 1,
                        y: dy,
                        ch: '>',
                    });
                }
                // The label rides the first run out of a real box, when it has room.
                if items[a].node.is_some() && labels[k].is_none() {
                    let run = cells.iter().take_while(|c| c.y == sy && c.ch == hz).count() as u16;
                    if run >= 4 {
                        let text: String =
                            edges[k].label.chars().take((run - 2) as usize).collect();
                        labels[k] = Some((cs + 1, sy, text));
                    }
                }
                cells_of[k].extend(cells);
            }
        }
        let routes = cells_of
            .into_iter()
            .zip(labels)
            .enumerate()
            .filter(|(_, (cells, _))| !cells.is_empty())
            .map(|(edge, (cells, label))| Route { edge, cells, label })
            .collect();
        let width = x_of(layer_count.saturating_sub(1)) + if layer_count > 0 { BOX_W } else { 0 };
        let order = layers
            .iter()
            .flat_map(|l| l.iter().filter_map(|&i| items[i].node))
            .collect();
        Self {
            items,
            layers,
            node_item,
            routes,
            width,
            height,
            order,
        }
    }

    /// The node above (`-1`) or below (`+1`) `node` in its layer.
    pub fn vertical(&self, node: usize, step: i32) -> Option<usize> {
        let item = &self.items[self.node_item[node]];
        let nodes: Vec<usize> = self.layers[item.layer]
            .iter()
            .filter_map(|&i| self.items[i].node)
            .collect();
        let at = nodes.iter().position(|&n| n == node)? as i32;
        nodes.get((at + step).max(0) as usize).copied()
    }

    /// Top-left corner of `node`'s box.
    pub fn origin(&self, node: usize) -> (u16, u16) {
        let item = &self.items[self.node_item[node]];
        (x_of(item.layer), item.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(from: usize, to: usize) -> EdgeIn {
        EdgeIn {
            from,
            to,
            control: false,
            label: "frame".into(),
        }
    }

    #[test]
    fn layers_follow_the_longest_path_and_long_edges_get_dummies() {
        // 0 -> 1 -> 2, and 0 -> 2 directly: 2 sits in layer 2, the direct edge passes a dummy.
        let l = Layout::build(3, &[edge(0, 1), edge(1, 2), edge(0, 2)]);
        let layer = |n: usize| l.items[l.node_item[n]].layer;
        assert_eq!((layer(0), layer(1), layer(2)), (0, 1, 2));
        assert_eq!(l.items.iter().filter(|i| i.node.is_none()).count(), 1);
        assert_eq!(l.routes.len(), 3);
        assert_eq!(l.order, vec![0, 1, 2]);
        assert_eq!(l.width, 2 * (BOX_W + EDGE_W) + BOX_W);
    }

    #[test]
    fn every_route_starts_at_its_source_box_and_ends_in_an_arrow_at_its_target() {
        let edges = [edge(0, 1), edge(0, 2), edge(0, 3), edge(1, 4), edge(2, 4)];
        let l = Layout::build(5, &edges);
        for r in &l.routes {
            let e = &edges[r.edge];
            let (sx, _) = l.origin(e.from);
            let (tx, ty) = l.origin(e.to);
            let first = r.cells.first().expect("cells");
            let last = r.cells.last().expect("cells");
            assert_eq!(first.x, sx + BOX_W, "starts right of the source box");
            assert_eq!((last.x, last.ch), (tx - 1, '>'), "ends in an arrow");
            assert!(
                last.y > ty && last.y < ty + BOX_H - 1,
                "inside the target's rows"
            );
            // Contiguous: every step moves one cell.
            for w in r.cells.windows(2) {
                let d = w[0].x.abs_diff(w[1].x) + w[0].y.abs_diff(w[1].y);
                assert_eq!(d, 1, "{:?} -> {:?}", w[0], w[1]);
            }
        }
        // Boxes of one layer never overlap.
        for list in &l.layers {
            for w in list.windows(2) {
                let (a, b) = (&l.items[w[0]], &l.items[w[1]]);
                assert!(a.y + a.h <= b.y);
            }
        }
    }

    #[test]
    fn barycentre_ordering_uncrosses_a_swapped_pair() {
        // Declared so that a naive order crosses: 0 -> 3, 1 -> 2.
        let l = Layout::build(4, &[edge(0, 3), edge(1, 2)]);
        let y = |n: usize| l.items[l.node_item[n]].y;
        assert_eq!(
            y(0) < y(1),
            y(3) < y(2),
            "targets ordered like their sources"
        );
    }

    #[test]
    fn control_edges_are_drawn_with_their_own_glyph() {
        let mut e = edge(0, 1);
        e.control = true;
        let l = Layout::build(2, &[e]);
        assert!(l.routes[0].cells.iter().any(|c| c.ch == '='));
        assert!(!l.routes[0].cells.iter().any(|c| c.ch == '-'));
    }

    #[test]
    fn an_isolated_node_and_an_empty_graph_lay_out() {
        assert_eq!(Layout::build(0, &[]).width, 0);
        let l = Layout::build(2, &[]);
        assert_eq!(l.layers.len(), 1);
        assert_eq!(l.vertical(0, 1), Some(1));
        assert_eq!(l.vertical(1, 1), None);
    }
}
