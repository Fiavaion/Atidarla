//! Edge topology shared by Smooth, Clean, Bevel and Clip: undirected edges
//! with the polygons that use them, point neighbourhoods, and chaining of
//! directed edges into closed loops.

use std::collections::HashMap;

use crate::mesh::Mesh;

/// The edges of a mesh: polygon edges with their uses, polyline segments,
/// and each point's neighbours (sorted, so results are deterministic).
#[derive(Debug)]
pub struct Edges {
    /// Uses of each undirected edge `(low, high)` by polygons: `(prim,
    /// corner)` where the polygon runs `corner → corner + 1` along it.
    pub uses: HashMap<(u32, u32), Vec<(u32, u32)>>,
    /// Neighbours per point (CSR) with whether the edge is a border (used
    /// by one polygon).
    start: Vec<u32>,
    around: Vec<(u32, bool)>,
}

impl Edges {
    pub fn new(m: &Mesh) -> Self {
        let mut uses: HashMap<(u32, u32), Vec<(u32, u32)>> = HashMap::new();
        let mut segments: Vec<(u32, u32)> = Vec::new();
        for f in 0..m.prims() {
            let p = m.prim(f);
            let k = p.len();
            if m.closed[f] {
                if k < 2 {
                    continue;
                }
                for i in 0..k {
                    let (a, b) = (p[i], p[(i + 1) % k]);
                    if a != b {
                        uses.entry(key(a, b))
                            .or_default()
                            .push((f as u32, i as u32));
                    }
                }
            } else {
                for w in p.windows(2) {
                    if w[0] != w[1] {
                        segments.push(key(w[0], w[1]));
                    }
                }
            }
        }
        let n = m.points();
        let mut pairs: Vec<(u32, u32, bool)> = Vec::with_capacity(uses.len() * 2);
        for (&(a, b), u) in &uses {
            let border = u.len() == 1;
            pairs.push((a, b, border));
            pairs.push((b, a, border));
        }
        for &(a, b) in &segments {
            if !uses.contains_key(&(a, b)) {
                pairs.push((a, b, false));
                pairs.push((b, a, false));
            }
        }
        pairs.sort_unstable();
        pairs.dedup_by(|x, y| x.0 == y.0 && x.1 == y.1);
        let mut start = vec![0u32; n + 1];
        for &(a, _, _) in &pairs {
            start[a as usize + 1] += 1;
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let around = pairs.iter().map(|&(_, b, border)| (b, border)).collect();
        Self {
            uses,
            start,
            around,
        }
    }

    /// The neighbours of point `p` and whether each edge is a border.
    pub fn neighbours(&self, p: u32) -> &[(u32, bool)] {
        let p = p as usize;
        &self.around[self.start[p] as usize..self.start[p + 1] as usize]
    }

    /// Per point: on a border edge.
    pub fn border_points(&self, m: &Mesh) -> Vec<bool> {
        let mut out = vec![false; m.points()];
        for (&(a, b), u) in &self.uses {
            if u.len() == 1 {
                out[a as usize] = true;
                out[b as usize] = true;
            }
        }
        out
    }

    /// The uses of edge `a`–`b` (either direction).
    pub fn of(&self, a: u32, b: u32) -> &[(u32, u32)] {
        self.uses.get(&key(a, b)).map_or(&[], Vec::as_slice)
    }
}

/// An undirected edge key.
pub fn key(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

/// Chains directed edges into closed loops (each edge used once; at a
/// point with several ways on, the lowest unused edge is taken). Open
/// chains are dropped.
pub fn loops(mut edges: Vec<(u32, u32)>) -> Vec<Vec<u32>> {
    edges.sort_unstable();
    edges.dedup();
    let mut used = vec![false; edges.len()];
    let from = |a: u32, used: &[bool]| -> Option<usize> {
        let i = edges.partition_point(|e| e.0 < a);
        (i..edges.len())
            .take_while(|&j| edges[j].0 == a)
            .find(|&j| !used[j])
    };
    let mut out = Vec::new();
    for s in 0..edges.len() {
        if used[s] {
            continue;
        }
        used[s] = true;
        let first = edges[s].0;
        let mut chain = vec![first];
        let mut at = edges[s].1;
        let mut closed = false;
        while chain.len() <= edges.len() {
            if at == first {
                closed = true;
                break;
            }
            chain.push(at);
            match from(at, &used) {
                Some(j) => {
                    used[j] = true;
                    at = edges[j].1;
                }
                None => break,
            }
        }
        if closed && chain.len() >= 3 {
            out.push(chain);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loops_chain_and_drop_open_chains() {
        let l = loops(vec![(2, 0), (0, 1), (1, 2), (5, 6), (7, 8), (8, 9), (9, 7)]);
        assert_eq!(l, vec![vec![0, 1, 2], vec![7, 8, 9]]);
    }

    #[test]
    fn grid_border_and_neighbours() {
        let mut m = Mesh::new();
        for p in [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ] {
            m.add_point(p);
        }
        m.add_polygon(&[0, 1, 2]);
        m.add_polygon(&[0, 2, 3]);
        let e = Edges::new(&m);
        assert_eq!(e.neighbours(0), [(1, true), (2, false), (3, true)]);
        assert_eq!(e.of(2, 0).len(), 2);
        assert!(e.border_points(&m).iter().all(|&b| b));
    }
}
