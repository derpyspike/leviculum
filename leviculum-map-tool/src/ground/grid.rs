//! Plain raster kernels of the ground cut, on row-major `f32` grids:
//! box means and deviations by summed-area tables, slope, the mode
//! filter, the minimum mapping unit, connected components, nearest
//! fill, and D8 flow accumulation over a priority-flood filled surface.
//! Every kernel is deterministic: ties are broken by index, never by a
//! hash or a thread.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};

/// A row-major grid.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid<T> {
    pub w: usize,
    pub h: usize,
    pub v: Vec<T>,
}

impl<T: Copy> Grid<T> {
    pub fn new(w: usize, h: usize, fill: T) -> Self {
        Grid {
            w,
            h,
            v: vec![fill; w * h],
        }
    }

    pub fn at(&self, x: usize, y: usize) -> T {
        self.v[y * self.w + x]
    }
}

/// Summed-area table of `g` (and of its squares), `(w + 1) x (h + 1)`.
fn sums(g: &Grid<f32>, square: bool) -> Vec<f64> {
    let (w, h) = (g.w, g.h);
    let mut s = vec![0.0f64; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0.0f64;
        for x in 0..w {
            let v = f64::from(g.v[y * w + x]);
            row += if square { v * v } else { v };
            s[(y + 1) * (w + 1) + x + 1] = s[y * (w + 1) + x + 1] + row;
        }
    }
    s
}

/// The window of half-size `r` around (x, y), clipped to the grid:
/// (x0, y0, x1, y1) exclusive.
fn window(w: usize, h: usize, x: usize, y: usize, r: usize) -> (usize, usize, usize, usize) {
    (
        x.saturating_sub(r),
        y.saturating_sub(r),
        (x + r + 1).min(w),
        (y + r + 1).min(h),
    )
}

fn rect(s: &[f64], w: usize, (x0, y0, x1, y1): (usize, usize, usize, usize)) -> f64 {
    let s1 = w + 1;
    s[y1 * s1 + x1] - s[y0 * s1 + x1] - s[y1 * s1 + x0] + s[y0 * s1 + x0]
}

/// The mean over a `size x size` box (odd size), the box clipped at the
/// grid's edges.
pub fn box_mean(g: &Grid<f32>, size: usize) -> Grid<f32> {
    let s = sums(g, false);
    let r = size / 2;
    let mut out = Grid::new(g.w, g.h, 0.0);
    for y in 0..g.h {
        for x in 0..g.w {
            let win = window(g.w, g.h, x, y, r);
            let n = ((win.2 - win.0) * (win.3 - win.1)) as f64;
            out.v[y * g.w + x] = (rect(&s, g.w, win) / n) as f32;
        }
    }
    out
}

/// The standard deviation over a `size x size` box, clipped as
/// [`box_mean`].
pub fn box_std(g: &Grid<f32>, size: usize) -> Grid<f32> {
    let s = sums(g, false);
    let q = sums(g, true);
    let r = size / 2;
    let mut out = Grid::new(g.w, g.h, 0.0);
    for y in 0..g.h {
        for x in 0..g.w {
            let win = window(g.w, g.h, x, y, r);
            let n = ((win.2 - win.0) * (win.3 - win.1)) as f64;
            let m = rect(&s, g.w, win) / n;
            let v = rect(&q, g.w, win) / n - m * m;
            out.v[y * g.w + x] = v.max(0.0).sqrt() as f32;
        }
    }
    out
}

/// Slope in degrees by Horn's method; `res` the ground metres per
/// pixel of each row (Web Mercator pixels shrink with latitude). Edge
/// pixels use their nearest neighbours.
pub fn slope_deg(dem: &Grid<f32>, res: &[f32]) -> Grid<f32> {
    let (w, h) = (dem.w, dem.h);
    let mut out = Grid::new(w, h, 0.0);
    let at = |x: isize, y: isize| {
        let x = x.clamp(0, w as isize - 1) as usize;
        let y = y.clamp(0, h as isize - 1) as usize;
        dem.v[y * w + x]
    };
    for (y, r) in res.iter().enumerate().take(h) {
        let r = r.max(1e-3);
        for x in 0..w {
            let (xi, yi) = (x as isize, y as isize);
            let z = |dx: isize, dy: isize| at(xi + dx, yi + dy);
            let gx = ((z(1, -1) + 2.0 * z(1, 0) + z(1, 1))
                - (z(-1, -1) + 2.0 * z(-1, 0) + z(-1, 1)))
                / (8.0 * r);
            let gy = ((z(-1, 1) + 2.0 * z(0, 1) + z(1, 1))
                - (z(-1, -1) + 2.0 * z(0, -1) + z(1, -1)))
                / (8.0 * r);
            out.v[y * w + x] = (gx * gx + gy * gy).sqrt().atan().to_degrees();
        }
    }
    out
}

/// The mode of each `size x size` window over class bytes 1..=9, ties to
/// the lower byte; 0 (outside) stays 0 and never votes.
pub fn majority(cls: &Grid<u8>, size: usize) -> Grid<u8> {
    let r = size / 2;
    let mut out = cls.clone();
    // One summed-area table per class.
    let tables: Vec<Vec<f64>> = (1..=9u8)
        .map(|k| {
            let g = Grid {
                w: cls.w,
                h: cls.h,
                v: cls.v.iter().map(|&c| f32::from(u8::from(c == k))).collect(),
            };
            sums(&g, false)
        })
        .collect();
    for y in 0..cls.h {
        for x in 0..cls.w {
            if cls.v[y * cls.w + x] == 0 {
                continue;
            }
            let win = window(cls.w, cls.h, x, y, r);
            let mut best = (0.0f64, 0u8);
            for (k, t) in tables.iter().enumerate() {
                let n = rect(t, cls.w, win);
                if n > best.0 {
                    best = (n, k as u8 + 1);
                }
            }
            out.v[y * cls.w + x] = best.1;
        }
    }
    out
}

/// 4-connected components of `mask`: a label per pixel (0 off the mask)
/// and each label's size, labels numbered in scan order from 1.
pub fn components(mask: &[bool], w: usize, h: usize) -> (Vec<u32>, Vec<u32>) {
    let mut label = vec![0u32; w * h];
    let mut sizes = vec![0u32];
    let mut queue = VecDeque::new();
    for start in 0..w * h {
        if !mask[start] || label[start] != 0 {
            continue;
        }
        let id = sizes.len() as u32;
        let mut n = 0u32;
        label[start] = id;
        queue.push_back(start);
        while let Some(i) = queue.pop_front() {
            n += 1;
            let (x, y) = (i % w, i / w);
            let mut visit = |j: usize| {
                if mask[j] && label[j] == 0 {
                    label[j] = id;
                    queue.push_back(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
        sizes.push(n);
    }
    (label, sizes)
}

/// Fill every pixel where `hole` is set from the nearest pixel where it
/// is not (breadth first, 4-connected, ties in queue order). A grid with
/// no kept pixel is left as it is.
pub fn nearest_fill<T: Copy>(g: &mut Grid<T>, hole: &[bool]) {
    let (w, h) = (g.w, g.h);
    let mut done: Vec<bool> = hole.iter().map(|&b| !b).collect();
    let mut queue: VecDeque<usize> = (0..w * h).filter(|&i| done[i]).collect();
    while let Some(i) = queue.pop_front() {
        let (x, y) = (i % w, i / w);
        let v = g.v[i];
        let mut visit = |j: usize, g: &mut Grid<T>| {
            if !done[j] {
                done[j] = true;
                g.v[j] = v;
                queue.push_back(j);
            }
        };
        if x > 0 {
            visit(i - 1, g);
        }
        if x + 1 < w {
            visit(i + 1, g);
        }
        if y > 0 {
            visit(i - w, g);
        }
        if y + 1 < h {
            visit(i + w, g);
        }
    }
}

/// Merge every connected patch of one class smaller than `min_px` into
/// the nearest kept class (the pilot's minimum mapping unit); classes in
/// `keep` are exempt (wadis: thin by nature). 0 stays 0.
pub fn mmu(cls: &Grid<u8>, min_px: u32, keep: &[u8]) -> Grid<u8> {
    let (w, h) = (cls.w, cls.h);
    let mut out = cls.clone();
    let mut hole = vec![false; w * h];
    for k in 1..=9u8 {
        if keep.contains(&k) {
            continue;
        }
        let mask: Vec<bool> = cls.v.iter().map(|&c| c == k).collect();
        let (label, sizes) = components(&mask, w, h);
        for i in 0..w * h {
            if label[i] != 0 && sizes[label[i] as usize] < min_px {
                hole[i] = true;
            }
        }
    }
    let outside: Vec<bool> = cls.v.iter().map(|&c| c == 0).collect();
    // The outside never fills a hole: fill from classes only, then put
    // the outside back.
    let any_kept = (0..w * h).any(|i| !hole[i] && !outside[i]);
    if any_kept {
        let blocked: Vec<bool> = (0..w * h).map(|i| hole[i] || outside[i]).collect();
        nearest_fill(&mut out, &blocked);
        for (o, &out_side) in out.v.iter_mut().zip(&outside) {
            if out_side {
                *o = 0;
            }
        }
    }
    out
}

/// A cell in the priority flood: lower first, then lower index.
#[derive(PartialEq)]
struct Cell(f64, usize);

impl Eq for Cell {}

impl Ord for Cell {
    fn cmp(&self, o: &Self) -> Ordering {
        o.0.total_cmp(&self.0).then(o.1.cmp(&self.1))
    }
}

impl PartialOrd for Cell {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

const D8: [(isize, isize); 8] = [
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];

/// D8 flow accumulation in cells (each cell counts itself), after a
/// priority-flood fill with a small epsilon (Barnes et al. 2014) so every
/// pit and flat drains to the grid's edge. Water leaving the grid is
/// lost: a catchment reaching past the grid is counted only inside it.
pub fn flow_accumulation(dem: &Grid<f32>) -> Grid<f32> {
    let (w, h) = (dem.w, dem.h);
    let n = w * h;
    let mut filled: Vec<f64> = dem.v.iter().map(|&v| f64::from(v)).collect();
    let mut closed = vec![false; n];
    let mut heap = BinaryHeap::new();
    for i in 0..n {
        let (x, y) = (i % w, i / w);
        if x == 0 || y == 0 || x + 1 == w || y + 1 == h {
            closed[i] = true;
            heap.push(Cell(filled[i], i));
        }
    }
    // The order cells leave the flood is an order from the outlets
    // upstream: every cell drains to one popped before it.
    let mut order = Vec::with_capacity(n);
    let mut down = vec![usize::MAX; n];
    while let Some(Cell(z, i)) = heap.pop() {
        order.push(i);
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        for (dx, dy) in D8 {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                continue;
            }
            let j = ny as usize * w + nx as usize;
            if closed[j] {
                continue;
            }
            closed[j] = true;
            let zj = filled[j].max(z + 1e-6);
            filled[j] = zj;
            down[j] = i;
            heap.push(Cell(zj, j));
        }
    }
    // Steepest descent among the lower neighbours where there is a real
    // drop; the flood's own link where the ground was filled flat.
    for &i in &order {
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        let mut best = (0.0f64, usize::MAX);
        for (dx, dy) in D8 {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                continue;
            }
            let j = ny as usize * w + nx as usize;
            let dist = if dx != 0 && dy != 0 {
                core::f64::consts::SQRT_2
            } else {
                1.0
            };
            let drop = (filled[i] - filled[j]) / dist;
            if drop > best.0 + 1e-9 {
                best = (drop, j);
            }
        }
        if best.1 != usize::MAX && best.0 > 1e-3 {
            down[i] = best.1;
        }
    }
    let mut acc = vec![1.0f32; n];
    // Upstream to downstream: the reverse of the flood order, which a
    // steepest-descent link also respects (it points strictly lower).
    let mut by_height: Vec<usize> = (0..n).collect();
    by_height.sort_by(|&a, &b| filled[b].total_cmp(&filled[a]).then(a.cmp(&b)));
    for i in by_height {
        let d = down[i];
        if d != usize::MAX {
            acc[d] += acc[i];
        }
    }
    Grid { w, h, v: acc }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_filters_match_a_direct_sum() {
        let g = Grid {
            w: 5,
            h: 4,
            v: (0..20).map(|i| (i * i % 7) as f32).collect(),
        };
        let m = box_mean(&g, 3);
        let s = box_std(&g, 3);
        for y in 0..4usize {
            for x in 0..5usize {
                let mut vals = Vec::new();
                for yy in y.saturating_sub(1)..(y + 2).min(4) {
                    for xx in x.saturating_sub(1)..(x + 2).min(5) {
                        vals.push(g.at(xx, yy) as f64);
                    }
                }
                let mean = vals.iter().sum::<f64>() / vals.len() as f64;
                let var = vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / vals.len() as f64;
                assert!((m.at(x, y) as f64 - mean).abs() < 1e-5);
                assert!((s.at(x, y) as f64 - var.sqrt()).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn a_ramp_has_its_slope() {
        // 1 m up per 1 m east: 45 degrees.
        let g = Grid {
            w: 6,
            h: 6,
            v: (0..36).map(|i| (i % 6) as f32).collect(),
        };
        let s = slope_deg(&g, &[1.0; 6]);
        assert!((s.at(3, 3) - 45.0).abs() < 1e-3);
    }

    #[test]
    fn a_valley_gathers_its_slopes() {
        // A V valley running south, falling 1 m a row: the valley floor's
        // last row gathers the whole grid but what leaves at the sides.
        let (w, h) = (9usize, 12usize);
        let v = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                (x as f32 - 4.0).abs() * 3.0 + (h - y) as f32
            })
            .collect();
        let acc = flow_accumulation(&Grid { w, h, v });
        let outlet = acc.at(4, h - 1);
        let elsewhere = (0..w)
            .filter(|&x| x != 4)
            .map(|x| acc.at(x, h - 1))
            .fold(0.0, f32::max);
        assert!(outlet > 40.0, "outlet gathers {outlet}");
        assert!(outlet > 5.0 * elsewhere);
    }

    #[test]
    fn a_pit_drains_after_the_fill() {
        let (w, h) = (7usize, 7usize);
        let mut v = vec![10.0f32; w * h];
        v[3 * w + 3] = 0.0; // a pit in a plateau
        let acc = flow_accumulation(&Grid { w, h, v });
        // Everything reaches an edge: the total leaving equals the cells.
        let mut out = 0.0;
        for i in 0..w * h {
            let (x, y) = (i % w, i / w);
            if x == 0 || y == 0 || x + 1 == w || y + 1 == h {
                out += acc.v[i];
            }
        }
        assert!(out >= (w * h) as f32 - 1e-3, "lost water: {out}");
    }

    #[test]
    fn small_patches_merge_into_their_surroundings() {
        let mut g = Grid::new(10, 10, 4u8);
        g.v[55] = 3; // one hamada pixel in reg
        g.v[0] = 0; // outside stays outside
        g.v[99] = 7; // a wadi pixel is exempt
        let out = mmu(&g, 2, &[7]);
        assert_eq!(out.v[55], 4);
        assert_eq!(out.v[0], 0);
        assert_eq!(out.v[99], 7);
    }

    #[test]
    fn the_mode_filter_takes_the_neighbourhood() {
        let mut g = Grid::new(5, 5, 4u8);
        g.v[12] = 1;
        let out = majority(&g, 3);
        assert_eq!(out.v[12], 4);
    }
}
