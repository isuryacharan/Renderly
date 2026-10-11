//! Feature-based motion analysis primitives (pure Rust, no dependencies beyond std).
//!
//! Everything here works on small grayscale `f32` images (the command layer decodes video
//! at <= 360 px tall before handing frames over). The building blocks are:
//!
//! * [`GrayImage`] with Gaussian blur, 2x box downscale and a pyramid builder;
//! * Shi-Tomasi corner detection ([`detect_corners`]) with grid-distributed selection;
//! * pyramidal Lucas-Kanade optical flow ([`track_lk`], [`track_lk_fb`] adds a
//!   forward-backward consistency check);
//! * RANSAC fitting of a translation or a 4-DOF similarity ([`fit_translation_ransac`],
//!   [`fit_similarity_ransac`]);
//! * [`MotionTracker`], which chains the above frame to frame, re-detecting features when too
//!   few survive;
//! * path utilities for stabilisation ([`gaussian_smooth`], [`simplify_polyline`]).
//!
//! Coordinates are pixels with the origin at the top-left of the image and +y pointing down.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TrackingError {
    #[error("image is too small to analyse ({0}x{1})")]
    ImageTooSmall(usize, usize),
    #[error("no trackable features found (the region is too flat or too dark)")]
    NoFeatures,
}

/// Row-major single-channel `f32` image (nominal range 0..255).
#[derive(Debug, Clone)]
pub struct GrayImage {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

impl GrayImage {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![0.0; width * height],
        }
    }

    pub fn from_fn(width: usize, height: usize, mut f: impl FnMut(usize, usize) -> f32) -> Self {
        let mut data = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                data.push(f(x, y));
            }
        }
        Self {
            width,
            height,
            data,
        }
    }

    /// BT.601 luma from tightly-packed RGBA8. Returns `None` on a size mismatch.
    pub fn from_rgba8(width: usize, height: usize, rgba: &[u8]) -> Option<Self> {
        if rgba.len() < width * height * 4 {
            return None;
        }
        let data = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .take(width * height)
            .map(|p| 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32)
            .collect();
        Some(Self {
            width,
            height,
            data,
        })
    }

    pub fn from_u8(width: usize, height: usize, gray: &[u8]) -> Option<Self> {
        if gray.len() < width * height {
            return None;
        }
        Some(Self {
            width,
            height,
            data: gray
                .iter()
                .take(width * height)
                .map(|&v| v as f32)
                .collect(),
        })
    }

    /// Pixel with coordinates clamped to the image.
    #[inline]
    pub fn at(&self, x: i64, y: i64) -> f32 {
        let xi = x.clamp(0, self.width as i64 - 1) as usize;
        let yi = y.clamp(0, self.height as i64 - 1) as usize;
        self.data[yi * self.width + xi]
    }

    /// Bilinear sample with edge clamping. Pixel centres sit at integer coordinates.
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        if self.width == 0 || self.height == 0 {
            return 0.0;
        }
        let x0f = x.floor();
        let y0f = y.floor();
        let fx = x - x0f;
        let fy = y - y0f;
        let x0 = x0f as i64;
        let y0 = y0f as i64;
        let a = self.at(x0, y0);
        let b = self.at(x0 + 1, y0);
        let c = self.at(x0, y0 + 1);
        let d = self.at(x0 + 1, y0 + 1);
        let top = a + (b - a) * fx;
        let bot = c + (d - c) * fx;
        top + (bot - top) * fy
    }

    /// Fill `out` with the `(2*half+1)^2` bilinear samples centred on `(cx, cy)` (row-major).
    /// All samples share one fractional offset, so interior patches use a fast integer path.
    pub fn sample_patch(&self, cx: f32, cy: f32, half: i64, out: &mut Vec<f32>) {
        let side = (2 * half + 1) as usize;
        out.clear();
        out.resize(side * side, 0.0);
        if self.width == 0 || self.height == 0 {
            return;
        }
        let fx0 = cx.floor();
        let fy0 = cy.floor();
        let (fx, fy) = (cx - fx0, cy - fy0);
        let (x0, y0) = (fx0 as i64 - half, fy0 as i64 - half);
        let (w, h) = (self.width as i64, self.height as i64);
        let (w00, w10, w01, w11) = (
            (1.0 - fx) * (1.0 - fy),
            fx * (1.0 - fy),
            (1.0 - fx) * fy,
            fx * fy,
        );
        if x0 >= 0 && y0 >= 0 && x0 + (side as i64) < w && y0 + (side as i64) < h {
            let stride = self.width;
            for j in 0..side {
                let row0 = (y0 as usize + j) * stride + x0 as usize;
                let row1 = row0 + stride;
                let o = &mut out[j * side..(j + 1) * side];
                for (i, v) in o.iter_mut().enumerate() {
                    *v = w00 * self.data[row0 + i]
                        + w10 * self.data[row0 + i + 1]
                        + w01 * self.data[row1 + i]
                        + w11 * self.data[row1 + i + 1];
                }
            }
        } else {
            for j in 0..side {
                for i in 0..side {
                    let (x, y) = (x0 + i as i64, y0 + j as i64);
                    out[j * side + i] = w00 * self.at(x, y)
                        + w10 * self.at(x + 1, y)
                        + w01 * self.at(x, y + 1)
                        + w11 * self.at(x + 1, y + 1);
                }
            }
        }
    }

    /// Separable Gaussian blur (clamped borders). `sigma <= 0` returns a copy.
    pub fn gaussian_blur(&self, sigma: f32) -> GrayImage {
        if sigma <= 1e-3 || self.width == 0 || self.height == 0 {
            return self.clone();
        }
        let radius = (sigma * 3.0).ceil().max(1.0) as i64;
        let mut kernel: Vec<f32> = (-radius..=radius)
            .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
            .collect();
        let sum: f32 = kernel.iter().sum();
        for k in &mut kernel {
            *k /= sum;
        }
        let (w, h) = (self.width, self.height);
        let mut tmp = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for (ki, k) in kernel.iter().enumerate() {
                    acc += k * self.at(x as i64 + ki as i64 - radius, y as i64);
                }
                tmp[y * w + x] = acc;
            }
        }
        let tmp_img = GrayImage {
            width: w,
            height: h,
            data: tmp,
        };
        let mut out = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for (ki, k) in kernel.iter().enumerate() {
                    acc += k * tmp_img.at(x as i64, y as i64 + ki as i64 - radius);
                }
                out[y * w + x] = acc;
            }
        }
        GrayImage {
            width: w,
            height: h,
            data: out,
        }
    }

    /// 2x2 box downscale to `(w/2, h/2)` (min 1).
    pub fn downscale_half(&self) -> GrayImage {
        let w = (self.width / 2).max(1);
        let h = (self.height / 2).max(1);
        GrayImage::from_fn(w, h, |x, y| {
            let (sx, sy) = (2 * x as i64, 2 * y as i64);
            0.25 * (self.at(sx, sy)
                + self.at(sx + 1, sy)
                + self.at(sx, sy + 1)
                + self.at(sx + 1, sy + 1))
        })
    }
}

/// Gaussian pyramid: level 0 is `base`; each further level is blurred then halved. Stops early
/// when a level would drop below 24 px on its short side.
pub fn build_pyramid(base: &GrayImage, levels: usize) -> Vec<GrayImage> {
    let mut out = Vec::with_capacity(levels.max(1));
    out.push(base.clone());
    while out.len() < levels.max(1) {
        let Some(last) = out.last() else { break };
        if last.width / 2 < 24 || last.height / 2 < 24 {
            break;
        }
        let next = last.gaussian_blur(1.0).downscale_half();
        out.push(next);
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn dist(self, other: Point) -> f32 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}

/// Axis-aligned rectangle in pixels (`x1`/`y1` exclusive).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn centre(&self) -> Point {
        Point::new((self.x0 + self.x1) * 0.5, (self.y0 + self.y1) * 0.5)
    }

    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }
}

// ---------------------------------------------------------------------------------------------
// Corner detection
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CornerParams {
    /// Upper bound on returned corners.
    pub max_corners: usize,
    /// Grid (columns, rows) over the search region used to spread features out.
    pub grid: (usize, usize),
    /// Accept corners whose response is at least this fraction of the strongest in the region.
    pub quality: f32,
    /// Absolute floor on the Shi-Tomasi response (mean min-eigenvalue of the structure tensor).
    pub min_response: f32,
    /// Minimum pixel distance between accepted corners.
    pub min_distance: f32,
    /// Half-size of the structure-tensor window.
    pub block_radius: usize,
    /// Keep corners at least this many pixels from the image edge (room for the LK window).
    pub border: usize,
}

impl Default for CornerParams {
    fn default() -> Self {
        Self {
            max_corners: 150,
            grid: (8, 6),
            quality: 0.01,
            min_response: 1.0,
            min_distance: 6.0,
            block_radius: 2,
            border: 8,
        }
    }
}

/// Shi-Tomasi "good features to track", restricted to `region` (whole image when `None`) and
/// spread over a grid so features do not cluster on one high-contrast object.
pub fn detect_corners(img: &GrayImage, region: Option<Rect>, params: &CornerParams) -> Vec<Point> {
    let (w, h) = (img.width, img.height);
    let border = params.border.max(params.block_radius + 1);
    if w < 2 * border + 3 || h < 2 * border + 3 || params.max_corners == 0 {
        return Vec::new();
    }
    let full = Rect::new(0.0, 0.0, w as f32, h as f32);
    let reg = region.unwrap_or(full);
    let bx0 = (reg.x0.floor().max(border as f32)) as usize;
    let by0 = (reg.y0.floor().max(border as f32)) as usize;
    let bx1 = (reg.x1.ceil().min((w - border) as f32)).max(0.0) as usize;
    let by1 = (reg.y1.ceil().min((h - border) as f32)).max(0.0) as usize;
    if bx1 <= bx0 + 2 || by1 <= by0 + 2 {
        return Vec::new();
    }

    // Structure tensor sums over a (2r+1)^2 window for every pixel of [bx0,bx1) x [by0,by1).
    let r = params.block_radius as i64;
    let ew = bx1 - bx0 + 2 * r as usize;
    let eh = by1 - by0 + 2 * r as usize;
    let ox = bx0 as i64 - r;
    let oy = by0 as i64 - r;
    let mut ixx = vec![0.0f32; ew * eh];
    let mut ixy = vec![0.0f32; ew * eh];
    let mut iyy = vec![0.0f32; ew * eh];
    for yy in 0..eh {
        for xx in 0..ew {
            let x = ox + xx as i64;
            let y = oy + yy as i64;
            let gx = 0.5 * (img.at(x + 1, y) - img.at(x - 1, y));
            let gy = 0.5 * (img.at(x, y + 1) - img.at(x, y - 1));
            let i = yy * ew + xx;
            ixx[i] = gx * gx;
            ixy[i] = gx * gy;
            iyy[i] = gy * gy;
        }
    }
    let rw = bx1 - bx0;
    let rh = by1 - by0;
    let win = (2 * r + 1) as usize;
    let norm = 1.0 / (win * win) as f32;
    let box_sum = |src: &[f32]| -> Vec<f32> {
        // Horizontal then vertical running sums; output is rw x rh.
        let mut horiz = vec![0.0f32; rw * eh];
        for yy in 0..eh {
            let row = &src[yy * ew..(yy + 1) * ew];
            let mut acc: f32 = row[..win].iter().sum();
            horiz[yy * rw] = acc;
            for x in 1..rw {
                acc += row[x + win - 1] - row[x - 1];
                horiz[yy * rw + x] = acc;
            }
        }
        let mut out = vec![0.0f32; rw * rh];
        for x in 0..rw {
            let mut acc = 0.0;
            for yy in 0..win {
                acc += horiz[yy * rw + x];
            }
            out[x] = acc;
            for y in 1..rh {
                acc += horiz[(y + win - 1) * rw + x] - horiz[(y - 1) * rw + x];
                out[y * rw + x] = acc;
            }
        }
        out
    };
    let sxx = box_sum(&ixx);
    let sxy = box_sum(&ixy);
    let syy = box_sum(&iyy);
    let mut resp = vec![0.0f32; rw * rh];
    let mut max_resp = 0.0f32;
    for i in 0..rw * rh {
        let a = sxx[i] * norm;
        let b = sxy[i] * norm;
        let c = syy[i] * norm;
        let tr = a + c;
        let disc = ((a - c) * (a - c) + 4.0 * b * b).sqrt();
        let lam = 0.5 * (tr - disc);
        resp[i] = lam;
        max_resp = max_resp.max(lam);
    }
    let thresh = (params.quality * max_resp).max(params.min_response);
    if max_resp < params.min_response {
        return Vec::new();
    }

    // Local maxima (3x3 non-maximum suppression).
    struct Cand {
        x: f32,
        y: f32,
        r: f32,
        cell: usize,
    }
    let (gc, gr) = (params.grid.0.max(1), params.grid.1.max(1));
    let mut cands: Vec<Cand> = Vec::new();
    for y in 1..rh.saturating_sub(1) {
        for x in 1..rw.saturating_sub(1) {
            let v = resp[y * rw + x];
            if v < thresh {
                continue;
            }
            let mut is_max = true;
            'n: for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nv = resp[((y as i64 + dy) as usize) * rw + (x as i64 + dx) as usize];
                    // Strict on the "earlier" neighbours so plateaus yield one maximum.
                    let earlier = dy < 0 || (dy == 0 && dx < 0);
                    if nv > v || (earlier && nv >= v) {
                        is_max = false;
                        break 'n;
                    }
                }
            }
            if !is_max {
                continue;
            }
            let col = (x * gc / rw).min(gc - 1);
            let row = (y * gr / rh).min(gr - 1);
            cands.push(Cand {
                x: (bx0 + x) as f32,
                y: (by0 + y) as f32,
                r: v,
                cell: row * gc + col,
            });
        }
    }
    cands.sort_by(|a, b| b.r.partial_cmp(&a.r).unwrap_or(std::cmp::Ordering::Equal));

    let quota = params.max_corners.div_ceil(gc * gr).max(1);
    let min_d2 = params.min_distance * params.min_distance;
    let mut per_cell = vec![0usize; gc * gr];
    let mut out: Vec<Point> = Vec::with_capacity(params.max_corners);
    let mut taken = vec![false; cands.len()];
    let far_enough = |out: &[Point], x: f32, y: f32| {
        out.iter()
            .all(|p| (p.x - x).powi(2) + (p.y - y).powi(2) >= min_d2)
    };
    // Pass 1: per-cell quota (strongest first). Pass 2: top up from whatever is left.
    for (i, c) in cands.iter().enumerate() {
        if out.len() >= params.max_corners {
            break;
        }
        if per_cell[c.cell] >= quota {
            continue;
        }
        if far_enough(&out, c.x, c.y) {
            out.push(Point::new(c.x, c.y));
            per_cell[c.cell] += 1;
            taken[i] = true;
        }
    }
    for (i, c) in cands.iter().enumerate() {
        if out.len() >= params.max_corners {
            break;
        }
        if taken[i] {
            continue;
        }
        if far_enough(&out, c.x, c.y) {
            out.push(Point::new(c.x, c.y));
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Lucas-Kanade
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LkParams {
    /// Pyramid levels used (clamped to what the supplied pyramids provide).
    pub levels: usize,
    /// Half-size of the integration window (window is `2r+1` square).
    pub window_radius: usize,
    pub max_iters: usize,
    /// Stop iterating when the per-iteration update is below this many pixels.
    pub epsilon: f32,
    /// Reject points whose window has mean min-eigenvalue below this (flat / aperture problem).
    pub min_eigen: f32,
    /// Forward-backward consistency threshold in pixels (used by [`track_lk_fb`]).
    pub max_fb_error: f32,
}

impl Default for LkParams {
    fn default() -> Self {
        Self {
            levels: 3,
            window_radius: 7,
            max_iters: 20,
            epsilon: 0.004,
            min_eigen: 0.5,
            max_fb_error: 1.0,
        }
    }
}

fn track_point(
    prev: &[GrayImage],
    next: &[GrayImage],
    p: Point,
    params: &LkParams,
) -> Option<Point> {
    let levels = params.levels.min(prev.len()).min(next.len()).max(1);
    let r = params.window_radius as i64;
    let side = (2 * r + 1) as usize;
    let side_t = side + 2;
    let n = (side * side) as f32;
    let mut tpl = Vec::new();
    let mut cur = Vec::new();
    let mut patch = vec![0.0f32; side * side];
    let mut gx_buf = vec![0.0f32; side * side];
    let mut gy_buf = vec![0.0f32; side * side];
    let (mut gx, mut gy) = (0.0f32, 0.0f32);
    if !p.x.is_finite() || !p.y.is_finite() {
        return None;
    }
    for lvl in (0..levels).rev() {
        let scale = 1.0 / (1u32 << lvl) as f32;
        let (px, py) = (p.x * scale, p.y * scale);
        let img_j = &next[lvl];
        // Template patch (with a 1-px apron for central-difference gradients).
        prev[lvl].sample_patch(px, py, r + 1, &mut tpl);
        let (mut gxx, mut gxy, mut gyy) = (0.0f32, 0.0f32, 0.0f32);
        for j in 0..side {
            for i in 0..side {
                let c = (j + 1) * side_t + i + 1;
                let ix = 0.5 * (tpl[c + 1] - tpl[c - 1]);
                let iy = 0.5 * (tpl[c + side_t] - tpl[c - side_t]);
                let k = j * side + i;
                patch[k] = tpl[c];
                gx_buf[k] = ix;
                gy_buf[k] = iy;
                gxx += ix * ix;
                gxy += ix * iy;
                gyy += iy * iy;
            }
        }
        let tr = gxx + gyy;
        let disc = ((gxx - gyy) * (gxx - gyy) + 4.0 * gxy * gxy).sqrt();
        let min_eig = 0.5 * (tr - disc) / n;
        let det = gxx * gyy - gxy * gxy;
        if min_eig < params.min_eigen || det.abs() < 1e-6 {
            if lvl == 0 {
                return None;
            }
            // Too flat at this coarse level: carry the estimate down unrefined.
            gx *= 2.0;
            gy *= 2.0;
            continue;
        }
        for _ in 0..params.max_iters {
            img_j.sample_patch(px + gx, py + gy, r, &mut cur);
            let (mut bx, mut by) = (0.0f32, 0.0f32);
            for k in 0..side * side {
                let di = patch[k] - cur[k];
                bx += di * gx_buf[k];
                by += di * gy_buf[k];
            }
            let mut dxu = (gyy * bx - gxy * by) / det;
            let mut dyu = (gxx * by - gxy * bx) / det;
            let mag = (dxu * dxu + dyu * dyu).sqrt();
            let cap = params.window_radius as f32;
            if mag > cap {
                dxu *= cap / mag;
                dyu *= cap / mag;
            }
            gx += dxu;
            gy += dyu;
            let tx = px + gx;
            let ty = py + gy;
            let wl = img_j.width as f32;
            let hl = img_j.height as f32;
            if tx < -(r as f32) - 1.0
                || ty < -(r as f32) - 1.0
                || tx > wl + r as f32
                || ty > hl + r as f32
            {
                return None;
            }
            if dxu * dxu + dyu * dyu < params.epsilon * params.epsilon {
                break;
            }
        }
        if lvl > 0 {
            gx *= 2.0;
            gy *= 2.0;
        }
    }
    let out = Point::new(p.x + gx, p.y + gy);
    // The integration window must lie inside the image: clamped border pixels are static and
    // would bias the displacement towards zero for points that are leaving the frame.
    let margin = (params.window_radius + 1) as f32;
    let w0 = next[0].width as f32;
    let h0 = next[0].height as f32;
    if !out.x.is_finite()
        || !out.y.is_finite()
        || out.x < margin
        || out.y < margin
        || out.x > w0 - 1.0 - margin
        || out.y > h0 - 1.0 - margin
    {
        return None;
    }
    Some(out)
}

/// Pyramidal Lucas-Kanade: where did each point of `prev` (level-0 coordinates) move to in
/// `next`? `None` marks points that could not be tracked (flat window, left the image, ...).
pub fn track_lk(
    prev: &[GrayImage],
    next: &[GrayImage],
    points: &[Point],
    params: &LkParams,
) -> Vec<Option<Point>> {
    if prev.is_empty() || next.is_empty() {
        return vec![None; points.len()];
    }
    points
        .iter()
        .map(|&p| track_point(prev, next, p, params))
        .collect()
}

/// [`track_lk`] plus a forward-backward check: each surviving point is tracked back to `prev`
/// and dropped if it does not return within `params.max_fb_error` pixels.
pub fn track_lk_fb(
    prev: &[GrayImage],
    next: &[GrayImage],
    points: &[Point],
    params: &LkParams,
) -> Vec<Option<Point>> {
    if prev.is_empty() || next.is_empty() {
        return vec![None; points.len()];
    }
    points
        .iter()
        .map(|&p| {
            let fwd = track_point(prev, next, p, params)?;
            let back = track_point(next, prev, fwd, params)?;
            (back.dist(p) <= params.max_fb_error).then_some(fwd)
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Motion models + RANSAC
// ---------------------------------------------------------------------------------------------

/// 4-DOF similarity: `(x, y) -> (a*x - b*y + tx, b*x + a*y + ty)`, i.e. `z -> m*z + t` with
/// `m = a + ib = scale * e^{i*angle}` in the image's own (y-down) pixel frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Similarity {
    pub a: f64,
    pub b: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Similarity {
    pub const IDENTITY: Similarity = Similarity {
        a: 1.0,
        b: 0.0,
        tx: 0.0,
        ty: 0.0,
    };

    pub fn from_translation(tx: f64, ty: f64) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            tx,
            ty,
        }
    }

    pub fn from_parts(scale: f64, angle_rad: f64, tx: f64, ty: f64) -> Self {
        Self {
            a: scale * angle_rad.cos(),
            b: scale * angle_rad.sin(),
            tx,
            ty,
        }
    }

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x - self.b * y + self.tx,
            self.b * x + self.a * y + self.ty,
        )
    }

    pub fn scale(&self) -> f64 {
        self.a.hypot(self.b)
    }

    pub fn angle(&self) -> f64 {
        self.b.atan2(self.a)
    }

    /// `self` applied first, then `next`.
    pub fn then(&self, next: &Similarity) -> Similarity {
        Similarity {
            a: next.a * self.a - next.b * self.b,
            b: next.a * self.b + next.b * self.a,
            tx: next.a * self.tx - next.b * self.ty + next.tx,
            ty: next.b * self.tx + next.a * self.ty + next.ty,
        }
    }

    pub fn inverse(&self) -> Option<Similarity> {
        let d = self.a * self.a + self.b * self.b;
        if d < 1e-18 {
            return None;
        }
        let ia = self.a / d;
        let ib = -self.b / d;
        Some(Similarity {
            a: ia,
            b: ib,
            tx: -(ia * self.tx - ib * self.ty),
            ty: -(ib * self.tx + ia * self.ty),
        })
    }
}

#[derive(Debug, Clone)]
pub struct RansacParams {
    pub iterations: usize,
    /// Residual (pixels) under which a correspondence counts as an inlier.
    pub inlier_threshold: f64,
    /// Fewer inliers than this means the fit is rejected (`None`).
    pub min_inliers: usize,
    pub seed: u64,
}

impl Default for RansacParams {
    fn default() -> Self {
        Self {
            iterations: 200,
            inlier_threshold: 1.0,
            min_inliers: 4,
            seed: 0x9E37_79B9_7F4A_7C15,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RansacResult {
    pub model: Similarity,
    pub inliers: Vec<bool>,
    pub inlier_count: usize,
}

struct XorShift(u64);

impl XorShift {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x2545_F491_4F6C_DD1D
        } else {
            seed
        })
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn residual(model: &Similarity, s: Point, d: Point) -> f64 {
    let (px, py) = model.apply(s.x as f64, s.y as f64);
    (px - d.x as f64).hypot(py - d.y as f64)
}

/// Least-squares translation over the selected correspondences.
fn lsq_translation(src: &[Point], dst: &[Point], idx: &[usize]) -> Option<Similarity> {
    if idx.is_empty() {
        return None;
    }
    let n = idx.len() as f64;
    let (mut dx, mut dy) = (0.0, 0.0);
    for &i in idx {
        dx += (dst[i].x - src[i].x) as f64;
        dy += (dst[i].y - src[i].y) as f64;
    }
    Some(Similarity::from_translation(dx / n, dy / n))
}

/// Closed-form least-squares similarity over the selected correspondences.
fn lsq_similarity(src: &[Point], dst: &[Point], idx: &[usize]) -> Option<Similarity> {
    if idx.len() < 2 {
        return None;
    }
    let n = idx.len() as f64;
    let (mut sx, mut sy, mut dx, mut dy) = (0.0, 0.0, 0.0, 0.0);
    for &i in idx {
        sx += src[i].x as f64;
        sy += src[i].y as f64;
        dx += dst[i].x as f64;
        dy += dst[i].y as f64;
    }
    let (sx, sy, dx, dy) = (sx / n, sy / n, dx / n, dy / n);
    let (mut num_a, mut num_b, mut den) = (0.0, 0.0, 0.0);
    for &i in idx {
        let u = src[i].x as f64 - sx;
        let v = src[i].y as f64 - sy;
        let p = dst[i].x as f64 - dx;
        let q = dst[i].y as f64 - dy;
        num_a += u * p + v * q;
        num_b += u * q - v * p;
        den += u * u + v * v;
    }
    if den < 1e-9 {
        return None;
    }
    let a = num_a / den;
    let b = num_b / den;
    Some(Similarity {
        a,
        b,
        tx: dx - (a * sx - b * sy),
        ty: dy - (b * sx + a * sy),
    })
}

fn ransac(
    src: &[Point],
    dst: &[Point],
    params: &RansacParams,
    sample_size: usize,
    hypothesis: impl Fn(&[Point], &[Point], &[usize]) -> Option<Similarity>,
    refit: impl Fn(&[Point], &[Point], &[usize]) -> Option<Similarity>,
) -> Option<RansacResult> {
    let n = src.len().min(dst.len());
    if n < sample_size.max(params.min_inliers.max(1)) {
        return None;
    }
    let mut rng = XorShift::new(params.seed);
    let mut best: Option<(usize, f64, Similarity)> = None;
    let mut sample = vec![0usize; sample_size];
    for _ in 0..params.iterations.max(1) {
        // Distinct random indices (sample_size is 1 or 2).
        for k in 0..sample_size {
            loop {
                let c = rng.below(n);
                if !sample[..k].contains(&c) {
                    sample[k] = c;
                    break;
                }
            }
        }
        let Some(model) = hypothesis(src, dst, &sample) else {
            continue;
        };
        let sc = model.scale();
        if !(0.5..=2.0).contains(&sc) {
            continue;
        }
        let mut count = 0usize;
        let mut err = 0.0;
        for i in 0..n {
            let r = residual(&model, src[i], dst[i]);
            if r <= params.inlier_threshold {
                count += 1;
                err += r;
            }
        }
        let better = match &best {
            None => true,
            Some((bc, be, _)) => count > *bc || (count == *bc && err < *be),
        };
        if better {
            best = Some((count, err, model));
            if count == n {
                break;
            }
        }
    }
    let (_, _, mut model) = best?;
    // Refine on the inlier set (twice: the inlier set shifts slightly once the model improves).
    let mut inliers = vec![false; n];
    for _ in 0..3 {
        let idx: Vec<usize> = (0..n)
            .filter(|&i| residual(&model, src[i], dst[i]) <= params.inlier_threshold)
            .collect();
        if idx.len() < sample_size {
            break;
        }
        match refit(src, dst, &idx) {
            Some(m) => model = m,
            None => break,
        }
    }
    let mut count = 0;
    for (i, flag) in inliers.iter_mut().enumerate() {
        *flag = residual(&model, src[i], dst[i]) <= params.inlier_threshold;
        if *flag {
            count += 1;
        }
    }
    if count < params.min_inliers.max(sample_size) {
        return None;
    }
    Some(RansacResult {
        model,
        inliers,
        inlier_count: count,
    })
}

/// Robust 2-DOF translation fit (`src[i] -> dst[i]`).
pub fn fit_translation_ransac(
    src: &[Point],
    dst: &[Point],
    params: &RansacParams,
) -> Option<RansacResult> {
    ransac(src, dst, params, 1, lsq_translation, lsq_translation)
}

/// Robust 4-DOF similarity fit (translation + rotation + uniform scale).
pub fn fit_similarity_ransac(
    src: &[Point],
    dst: &[Point],
    params: &RansacParams,
) -> Option<RansacResult> {
    let hyp = |src: &[Point], dst: &[Point], s: &[usize]| -> Option<Similarity> {
        let (i, j) = (s[0], s[1]);
        let (ux, uy) = ((src[j].x - src[i].x) as f64, (src[j].y - src[i].y) as f64);
        let (vx, vy) = ((dst[j].x - dst[i].x) as f64, (dst[j].y - dst[i].y) as f64);
        let den = ux * ux + uy * uy;
        if den < 4.0 {
            return None; // points too close together to define rotation/scale reliably
        }
        // m = v / u (complex division).
        let a = (vx * ux + vy * uy) / den;
        let b = (vy * ux - vx * uy) / den;
        Some(Similarity {
            a,
            b,
            tx: dst[i].x as f64 - (a * src[i].x as f64 - b * src[i].y as f64),
            ty: dst[i].y as f64 - (b * src[i].x as f64 + a * src[i].y as f64),
        })
    };
    ransac(src, dst, params, 2, hyp, lsq_similarity)
}

// ---------------------------------------------------------------------------------------------
// Path utilities
// ---------------------------------------------------------------------------------------------

/// Least-squares line through `y[0..]` against `x = 0, 1, ...`: `(intercept, slope)`.
fn fit_line(y: &[f64]) -> (f64, f64) {
    let n = y.len() as f64;
    let mean_x = (n - 1.0) * 0.5;
    let mean_y = y.iter().sum::<f64>() / n;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (i, v) in y.iter().enumerate() {
        let dx = i as f64 - mean_x;
        sxy += dx * (v - mean_y);
        sxx += dx * dx;
    }
    let slope = if sxx > 1e-12 { sxy / sxx } else { 0.0 };
    (mean_y - slope * mean_x, slope)
}

/// Gaussian low-pass of a 1-D path (`sigma` in samples). Borders are padded by extending the
/// least-squares line through the first/last `~2*sigma` samples, which reproduces linear
/// trends exactly (a steady pan is not dragged towards the first/last position, as
/// clamp-to-edge padding would) without letting one jittery end sample dominate.
pub fn gaussian_smooth(path: &[f64], sigma: f64) -> Vec<f64> {
    let n = path.len();
    if n < 3 || sigma <= 1e-6 {
        return path.to_vec();
    }
    let radius = ((sigma * 3.0).ceil() as usize).max(1);
    let kernel: Vec<f64> = (0..=2 * radius)
        .map(|i| {
            let d = i as f64 - radius as f64;
            (-d * d / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let norm: f64 = kernel.iter().sum();
    let w = ((sigma * 2.0).ceil() as usize).clamp(4, n);
    let (head_a, head_b) = fit_line(&path[..w]);
    let (tail_a, tail_b) = fit_line(&path[n - w..]);
    let ext = |i: i64| -> f64 {
        if i < 0 {
            head_a + head_b * i as f64
        } else if i as usize >= n {
            tail_a + tail_b * (i - (n - w) as i64) as f64
        } else {
            path[i as usize]
        }
    };
    (0..n)
        .map(|c| {
            let mut acc = 0.0;
            for (k, wt) in kernel.iter().enumerate() {
                acc += wt * ext(c as i64 + k as i64 - radius as i64);
            }
            acc / norm
        })
        .collect()
}

/// Ramer-Douglas-Peucker on `(times[i], values[i])`: indices of the samples to keep so that
/// linear interpolation between them stays within `tolerance` of every dropped sample.
pub fn simplify_polyline(times: &[f64], values: &[f64], tolerance: f64) -> Vec<usize> {
    let n = times.len().min(values.len());
    if n <= 2 {
        return (0..n).collect();
    }
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    let mut stack = vec![(0usize, n - 1)];
    while let Some((lo, hi)) = stack.pop() {
        if hi <= lo + 1 {
            continue;
        }
        let (t0, v0, t1, v1) = (times[lo], values[lo], times[hi], values[hi]);
        let span = (t1 - t0).max(1e-12);
        let mut worst = 0.0;
        let mut worst_i = lo;
        for i in lo + 1..hi {
            let interp = v0 + (v1 - v0) * (times[i] - t0) / span;
            let e = (values[i] - interp).abs();
            if e > worst {
                worst = e;
                worst_i = i;
            }
        }
        if worst > tolerance {
            keep[worst_i] = true;
            stack.push((lo, worst_i));
            stack.push((worst_i, hi));
        }
    }
    (0..n).filter(|&i| keep[i]).collect()
}

// ---------------------------------------------------------------------------------------------
// Frame-to-frame tracker
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionModel {
    Translation,
    Similarity,
}

#[derive(Debug, Clone)]
pub struct TrackerConfig {
    pub model: MotionModel,
    pub corners: CornerParams,
    pub lk: LkParams,
    pub ransac: RansacParams,
    pub pyramid_levels: usize,
    /// When `true` the search region is carried along with the estimated motion (object
    /// tracking). When `false` it stays fixed (global camera motion over the whole frame).
    pub follow_region: bool,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            model: MotionModel::Similarity,
            corners: CornerParams::default(),
            lk: LkParams::default(),
            ransac: RansacParams::default(),
            pyramid_levels: 3,
            follow_region: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StepResult {
    /// Maps points of the previous frame to the new frame (identity when `lost`).
    pub motion: Similarity,
    /// Features that survived LK + forward-backward checks.
    pub tracked: usize,
    /// RANSAC inliers among them.
    pub inliers: usize,
    /// Features were (re-)detected on the new frame.
    pub redetected: bool,
    /// Too few correspondences to estimate motion; `motion` is identity.
    pub lost: bool,
}

/// Tracks a feature set frame to frame and fits one global motion per step.
pub struct MotionTracker {
    cfg: TrackerConfig,
    region: Rect,
    prev: Vec<GrayImage>,
    points: Vec<Point>,
    redetect_below: usize,
    steps: u64,
}

impl MotionTracker {
    /// Detect initial features in `region` (whole image when `None`) of the first frame.
    pub fn new(
        first: &GrayImage,
        region: Option<Rect>,
        cfg: TrackerConfig,
    ) -> Result<Self, TrackingError> {
        if first.width < 48 || first.height < 48 {
            return Err(TrackingError::ImageTooSmall(first.width, first.height));
        }
        let region = region.unwrap_or(Rect::new(0.0, 0.0, first.width as f32, first.height as f32));
        let points = detect_corners(first, Some(region), &cfg.corners);
        if points.len() < cfg.ransac.min_inliers.max(3) {
            return Err(TrackingError::NoFeatures);
        }
        let redetect_below = ((points.len() as f32 * 0.5) as usize).max(8);
        let prev = build_pyramid(first, cfg.pyramid_levels);
        Ok(Self {
            cfg,
            region,
            prev,
            points,
            redetect_below,
            steps: 0,
        })
    }

    pub fn region(&self) -> Rect {
        self.region
    }

    pub fn feature_count(&self) -> usize {
        self.points.len()
    }

    /// Advance to `next` and estimate the motion since the previous frame.
    pub fn step(&mut self, next: &GrayImage) -> StepResult {
        let next_pyr = build_pyramid(next, self.cfg.pyramid_levels);
        self.steps += 1;
        let tracked = track_lk_fb(&self.prev, &next_pyr, &self.points, &self.cfg.lk);
        let mut src = Vec::with_capacity(tracked.len());
        let mut dst = Vec::with_capacity(tracked.len());
        for (p, t) in self.points.iter().zip(tracked.iter()) {
            if let Some(t) = t {
                src.push(*p);
                dst.push(*t);
            }
        }
        let tracked_count = src.len();
        let mut rp = self.cfg.ransac.clone();
        rp.seed = rp.seed.wrapping_add(self.steps.wrapping_mul(0x9E37_79B9));
        let fit = match self.cfg.model {
            MotionModel::Translation => fit_translation_ransac(&src, &dst, &rp),
            MotionModel::Similarity => fit_similarity_ransac(&src, &dst, &rp),
        };
        let (motion, inliers, lost) = match fit {
            Some(r) => {
                self.points = dst
                    .iter()
                    .zip(r.inliers.iter())
                    .filter_map(|(p, &keep)| keep.then_some(*p))
                    .collect();
                (r.model, r.inlier_count, false)
            }
            None => {
                self.points.clear();
                (Similarity::IDENTITY, 0, true)
            }
        };
        if self.cfg.follow_region && !lost {
            let c = self.region.centre();
            let (nx, ny) = motion.apply(c.x as f64, c.y as f64);
            let s = motion.scale() as f32;
            let (hx, hy) = (
                self.region.width() * 0.5 * s,
                self.region.height() * 0.5 * s,
            );
            self.region = Rect::new(
                nx as f32 - hx,
                ny as f32 - hy,
                nx as f32 + hx,
                ny as f32 + hy,
            );
        }
        let mut redetected = false;
        if self.points.len() < self.redetect_below {
            let found = detect_corners(next, Some(self.region), &self.cfg.corners);
            let min_d = self.cfg.corners.min_distance;
            for f in found {
                if self.points.len() >= self.cfg.corners.max_corners {
                    break;
                }
                if self.points.iter().all(|p| p.dist(f) >= min_d) {
                    self.points.push(f);
                    redetected = true;
                }
            }
        }
        self.prev = next_pyr;
        StepResult {
            motion,
            tracked: tracked_count,
            inliers,
            redetected,
            lost,
        }
    }
}

/// Deterministic procedural texture used by the unit tests here and by the command-level
/// tests: a sum of oriented sinusoids (smooth, rich in corners, defined for any real
/// coordinate so ground-truth sub-pixel shifts/rotations are exact).
#[cfg(test)]
pub(crate) mod test_support {

    pub fn texture(x: f64, y: f64) -> f32 {
        // (fx, fy, phase, amplitude): wavelengths ~7..60 px, irrational-ish mix.
        const WAVES: [(f64, f64, f64, f64); 14] = [
            (0.021, 0.013, 0.3, 22.0),
            (-0.017, 0.029, 1.1, 20.0),
            (0.043, -0.011, 2.0, 16.0),
            (0.009, 0.047, 0.7, 15.0),
            (-0.061, -0.037, 1.9, 12.0),
            (0.074, 0.052, 2.8, 10.0),
            (0.033, -0.069, 0.2, 11.0),
            (-0.083, 0.021, 1.4, 9.0),
            (0.0125, -0.0215, 2.5, 18.0),
            (0.104, -0.047, 0.9, 7.0),
            (-0.031, -0.093, 2.2, 8.0),
            (0.057, 0.088, 1.6, 7.0),
            (0.139, 0.019, 0.4, 5.0),
            (-0.027, 0.121, 2.9, 5.0),
        ];
        let mut v = 128.0;
        for (fx, fy, ph, a) in WAVES {
            v += a * (std::f64::consts::TAU * (fx * x + fy * y) + ph).sin();
        }
        // A little non-linearity so the texture has real edges, not just smooth saddles.
        let v = 128.0 + 0.9 * (v - 128.0) + 18.0 * ((v - 128.0) * 0.08).tanh();
        v.clamp(0.0, 255.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::texture;
    use super::*;

    fn tex_img(w: usize, h: usize, map: impl Fn(f64, f64) -> (f64, f64)) -> GrayImage {
        GrayImage::from_fn(w, h, |x, y| {
            let (u, v) = map(x as f64, y as f64);
            texture(u, v)
        })
    }

    #[test]
    fn rgba_to_gray_and_size_checks() {
        let rgba = [
            255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let g = GrayImage::from_rgba8(2, 2, &rgba).unwrap();
        assert!((g.data[0] - 76.245).abs() < 0.01);
        assert!((g.data[1] - 149.685).abs() < 0.01);
        assert!((g.data[3] - 255.0).abs() < 0.01);
        assert!(GrayImage::from_rgba8(4, 4, &rgba).is_none());
        assert!(GrayImage::from_u8(2, 2, &[1, 2, 3]).is_none());
    }

    #[test]
    fn blur_preserves_constant_and_smooths_noise() {
        let flat = GrayImage::from_fn(20, 20, |_, _| 90.0);
        let b = flat.gaussian_blur(1.5);
        assert!(b.data.iter().all(|v| (v - 90.0).abs() < 1e-3));
        let noisy = GrayImage::from_fn(32, 32, |x, y| if (x + y) % 2 == 0 { 0.0 } else { 255.0 });
        let var = |im: &GrayImage| {
            let m = im.data.iter().sum::<f32>() / im.data.len() as f32;
            im.data.iter().map(|v| (v - m).powi(2)).sum::<f32>() / im.data.len() as f32
        };
        assert!(var(&noisy.gaussian_blur(1.0)) < var(&noisy) * 0.05);
    }

    #[test]
    fn pyramid_halves_and_preserves_mean() {
        let img = tex_img(200, 120, |x, y| (x, y));
        let pyr = build_pyramid(&img, 4);
        assert_eq!(
            pyr.len(),
            3,
            "stops when the short side would drop below 24 px"
        );
        assert_eq!((pyr[1].width, pyr[1].height), (100, 60));
        assert_eq!((pyr[2].width, pyr[2].height), (50, 30));
        let mean = |im: &GrayImage| im.data.iter().sum::<f32>() / im.data.len() as f32;
        assert!((mean(&pyr[0]) - mean(&pyr[2])).abs() < 3.0);
        let d = GrayImage::from_fn(4, 4, |x, _| x as f32).downscale_half();
        assert_eq!(d.data, vec![0.5, 2.5, 0.5, 2.5]);
    }

    #[test]
    fn bilinear_sample_interpolates() {
        let img = GrayImage::from_fn(4, 4, |x, y| (x + 10 * y) as f32);
        assert!((img.sample(1.5, 2.25) - (1.5 + 22.5)).abs() < 1e-4);
        assert!(
            (img.sample(-5.0, 0.0) - 0.0).abs() < 1e-6,
            "clamps at the border"
        );
    }

    #[test]
    fn corners_are_distributed_and_flat_images_yield_none() {
        let img = tex_img(320, 240, |x, y| (x, y));
        let pts = detect_corners(&img, None, &CornerParams::default());
        assert!(pts.len() >= 60, "got {}", pts.len());
        // Every quadrant is represented.
        let mut quad = [0; 4];
        for p in &pts {
            assert!(p.x >= 8.0 && p.x < 312.0 && p.y >= 8.0 && p.y < 232.0);
            quad[(p.x >= 160.0) as usize + 2 * (p.y >= 120.0) as usize] += 1;
        }
        assert!(quad.iter().all(|&c| c >= 8), "quadrants {quad:?}");
        // Min distance holds.
        for (i, a) in pts.iter().enumerate() {
            for b in &pts[i + 1..] {
                assert!(a.dist(*b) >= 5.99);
            }
        }
        let flat = GrayImage::from_fn(320, 240, |_, _| 100.0);
        assert!(detect_corners(&flat, None, &CornerParams::default()).is_empty());
        // Region restriction.
        let region = Rect::new(100.0, 60.0, 200.0, 140.0);
        let sub = detect_corners(&img, Some(region), &CornerParams::default());
        assert!(!sub.is_empty());
        assert!(sub
            .iter()
            .all(|p| p.x >= 100.0 && p.x <= 200.0 && p.y >= 60.0 && p.y <= 140.0));
    }

    #[test]
    fn lk_recovers_subpixel_and_large_translations() {
        let a = tex_img(320, 240, |x, y| (x, y));
        let pa = build_pyramid(&a, 3);
        let pts = detect_corners(&a, None, &CornerParams::default());
        for (dx, dy) in [(3.4f64, -2.2f64), (-0.35, 0.6), (12.3, 7.7)] {
            // Content moves by (dx, dy): b(p) = a(p - d).
            let b = tex_img(320, 240, |x, y| (x - dx, y - dy));
            let pb = build_pyramid(&b, 3);
            let res = track_lk_fb(&pa, &pb, &pts, &LkParams::default());
            let ok: Vec<_> = pts
                .iter()
                .zip(&res)
                .filter_map(|(p, r)| r.map(|r| (r.x - p.x, r.y - p.y)))
                .collect();
            assert!(
                ok.len() * 10 >= pts.len() * 7,
                "too few survivors at {dx},{dy}: {}",
                ok.len()
            );
            let mut ex: Vec<f32> = ok.iter().map(|d| d.0).collect();
            let mut ey: Vec<f32> = ok.iter().map(|d| d.1).collect();
            ex.sort_by(|a, b| a.partial_cmp(b).unwrap());
            ey.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let (mx, my) = (ex[ex.len() / 2] as f64, ey[ey.len() / 2] as f64);
            assert!(
                (mx - dx).abs() < 0.1 && (my - dy).abs() < 0.1,
                "({mx},{my}) vs ({dx},{dy})"
            );
        }
    }

    #[test]
    fn lk_handles_flat_images_without_panicking() {
        let flat = GrayImage::from_fn(160, 120, |_, _| 50.0);
        let pf = build_pyramid(&flat, 3);
        let pts = [
            Point::new(80.0, 60.0),
            Point::new(3.0, 3.0),
            Point::new(-5.0, 999.0),
        ];
        assert!(track_lk(&pf, &pf, &pts, &LkParams::default())
            .iter()
            .all(|r| r.is_none()));
        assert!(track_lk_fb(&pf, &pf, &pts, &LkParams::default())
            .iter()
            .all(|r| r.is_none()));
        assert!(track_lk(&[], &pf, &pts, &LkParams::default())
            .iter()
            .all(|r| r.is_none()));
        // Tracking into a flat image fails too (the content vanished).
        let a = tex_img(160, 120, |x, y| (x, y));
        let pa = build_pyramid(&a, 3);
        let ctr = detect_corners(&a, None, &CornerParams::default());
        let res = track_lk_fb(&pa, &pf, &ctr, &LkParams::default());
        assert!(res.iter().filter(|r| r.is_some()).count() * 4 < ctr.len().max(1));
    }

    #[test]
    fn forward_backward_check_rejects_unrelated_frames() {
        let a = tex_img(320, 240, |x, y| (x, y));
        let unrelated = tex_img(320, 240, |x, y| (y * 1.3 + 500.0, x * 0.7 - 300.0));
        let (pa, pu) = (build_pyramid(&a, 3), build_pyramid(&unrelated, 3));
        let pts = detect_corners(&a, None, &CornerParams::default());
        let plain = track_lk(&pa, &pu, &pts, &LkParams::default());
        let checked = track_lk_fb(&pa, &pu, &pts, &LkParams::default());
        let n_plain = plain.iter().filter(|r| r.is_some()).count();
        let n_checked = checked.iter().filter(|r| r.is_some()).count();
        assert!(n_checked <= n_plain);
        assert!(
            n_checked * 2 < pts.len(),
            "checked {n_checked} of {}",
            pts.len()
        );
    }

    fn pairs_for(model: &Similarity, n: usize) -> (Vec<Point>, Vec<Point>) {
        let mut rng = XorShift::new(7);
        let mut src = Vec::new();
        let mut dst = Vec::new();
        for _ in 0..n {
            let (x, y) = (rng.below(300) as f64 + 10.0, rng.below(220) as f64 + 10.0);
            let (u, v) = model.apply(x, y);
            src.push(Point::new(x as f32, y as f32));
            dst.push(Point::new(u as f32, v as f32));
        }
        (src, dst)
    }

    #[test]
    fn ransac_recovers_similarity_with_outliers() {
        let truth = Similarity::from_parts(1.04, 0.035, 5.5, -3.25);
        let (src, mut dst) = pairs_for(&truth, 100);
        let mut rng = XorShift::new(99);
        // 35% gross outliers + inlier noise.
        for (i, d) in dst.iter_mut().enumerate() {
            if i % 3 == 0 {
                *d = Point::new(rng.below(320) as f32, rng.below(240) as f32);
            } else {
                d.x += (rng.below(100) as f32 / 100.0 - 0.5) * 0.3;
                d.y += (rng.below(100) as f32 / 100.0 - 0.5) * 0.3;
            }
        }
        let r = fit_similarity_ransac(&src, &dst, &RansacParams::default()).unwrap();
        assert!((r.model.scale() - 1.04).abs() < 0.003, "{:?}", r.model);
        assert!((r.model.angle() - 0.035).abs() < 0.003);
        assert!((r.model.tx - 5.5).abs() < 0.6 && (r.model.ty + 3.25).abs() < 0.6);
        assert!(r.inlier_count >= 60);
        // Marked outliers are (almost always) excluded.
        let wrongly_kept = (0..100).step_by(3).filter(|&i| r.inliers[i]).count();
        assert!(wrongly_kept <= 2, "{wrongly_kept}");
    }

    #[test]
    fn ransac_translation_prefers_majority_motion() {
        // 70% of points move (5, 2); 30% (the "foreground") move (-9, 11).
        let (src, _) = pairs_for(&Similarity::IDENTITY, 100);
        let dst: Vec<Point> = src
            .iter()
            .enumerate()
            .map(|(i, p)| {
                if i % 10 < 7 {
                    Point::new(p.x + 5.0, p.y + 2.0)
                } else {
                    Point::new(p.x - 9.0, p.y + 11.0)
                }
            })
            .collect();
        let r = fit_translation_ransac(&src, &dst, &RansacParams::default()).unwrap();
        assert!((r.model.tx - 5.0).abs() < 1e-6 && (r.model.ty - 2.0).abs() < 1e-6);
        assert_eq!(r.inlier_count, 70);
        assert_eq!(r.inliers.iter().filter(|&&b| b).count(), 70);
    }

    #[test]
    fn ransac_rejects_degenerate_input() {
        assert!(fit_similarity_ransac(&[], &[], &RansacParams::default()).is_none());
        let p = vec![Point::new(1.0, 1.0); 10];
        // All source points coincide: no valid hypothesis.
        assert!(fit_similarity_ransac(&p, &p, &RansacParams::default()).is_none());
        let two = vec![Point::new(0.0, 0.0), Point::new(10.0, 0.0)];
        assert!(fit_translation_ransac(&two, &two, &RansacParams::default()).is_none());
    }

    #[test]
    fn similarity_algebra() {
        let m = Similarity::from_parts(1.3, 0.4, 7.0, -2.0);
        let inv = m.inverse().unwrap();
        let id = m.then(&inv);
        assert!(
            (id.a - 1.0).abs() < 1e-12
                && id.b.abs() < 1e-12
                && id.tx.abs() < 1e-9
                && id.ty.abs() < 1e-9
        );
        let n = Similarity::from_parts(0.9, -0.2, 1.0, 3.0);
        let (x1, y1) = m.then(&n).apply(4.0, 5.0);
        let (mx, my) = m.apply(4.0, 5.0);
        let (x2, y2) = n.apply(mx, my);
        assert!((x1 - x2).abs() < 1e-9 && (y1 - y2).abs() < 1e-9);
        assert!((m.scale() - 1.3).abs() < 1e-12 && (m.angle() - 0.4).abs() < 1e-12);
    }

    #[test]
    fn smoothing_keeps_trend_and_removes_jitter() {
        let n = 200;
        let pan: Vec<f64> = (0..n).map(|i| 0.7 * i as f64).collect();
        // Linear trend is reproduced exactly, including at the borders.
        let s = gaussian_smooth(&pan, 8.0);
        for (a, b) in s.iter().zip(&pan) {
            assert!((a - b).abs() < 1e-6);
        }
        let jittered: Vec<f64> = pan
            .iter()
            .enumerate()
            .map(|(i, p)| p + 5.0 * (i as f64 * 1.9).sin())
            .collect();
        let s = gaussian_smooth(&jittered, 6.0);
        let dev = |v: &[f64]| {
            (v.iter()
                .zip(&pan)
                .map(|(a, b)| (a - b).powi(2))
                .sum::<f64>()
                / v.len() as f64)
                .sqrt()
        };
        assert!(dev(&s) < dev(&jittered) / 10.0);
        assert_eq!(gaussian_smooth(&jittered, 0.0), jittered);
        assert_eq!(gaussian_smooth(&[1.0, 2.0], 5.0), vec![1.0, 2.0]);
        // Kernel wider than the path must not panic or go out of range.
        let short = gaussian_smooth(&[0.0, 1.0, 2.0, 3.0, 4.0], 50.0);
        assert!(short
            .iter()
            .zip([0.0, 1.0, 2.0, 3.0, 4.0])
            .all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn polyline_simplification_respects_tolerance() {
        let t: Vec<f64> = (0..100).map(|i| i as f64).collect();
        let line: Vec<f64> = t.iter().map(|x| 2.0 * x + 1.0).collect();
        assert_eq!(simplify_polyline(&t, &line, 1e-9), vec![0, 99]);
        let curve: Vec<f64> = t.iter().map(|x| (x * 0.1).sin() * 10.0).collect();
        let keep = simplify_polyline(&t, &curve, 0.05);
        assert!(keep.len() < 60 && keep.len() > 5);
        for w in keep.windows(2) {
            for i in w[0]..=w[1] {
                let f = (t[i] - t[w[0]]) / (t[w[1]] - t[w[0]]);
                let interp = curve[w[0]] + (curve[w[1]] - curve[w[0]]) * f;
                assert!((interp - curve[i]).abs() <= 0.05 + 1e-9);
            }
        }
    }

    #[test]
    fn tracker_follows_global_pan_and_redetects() {
        // Camera pans (2.3, 0.9) px/frame over 60 frames: ~138 px, features leave the frame.
        let (w, h) = (240usize, 180usize);
        let frame = |k: usize| tex_img(w, h, |x, y| (x + 2.3 * k as f64, y + 0.9 * k as f64));
        let cfg = TrackerConfig {
            follow_region: false,
            ..TrackerConfig::default()
        };
        let mut tr = MotionTracker::new(&frame(0), None, cfg).unwrap();
        let mut cum = Similarity::IDENTITY;
        let mut redetects = 0;
        for k in 1..=60 {
            let s = tr.step(&frame(k));
            assert!(!s.lost, "lost at frame {k}");
            redetects += s.redetected as usize;
            cum = cum.then(&s.motion);
        }
        assert!(redetects > 0);
        // Content moves opposite to the window: expect (-138, -54).
        assert!(
            (cum.tx + 138.0).abs() < 1.0 && (cum.ty + 54.0).abs() < 1.0,
            "{cum:?}"
        );
        assert!((cum.scale() - 1.0).abs() < 0.01 && cum.angle().abs() < 0.01);
    }

    #[test]
    fn tracker_recovers_rotation_and_scale() {
        let (w, h) = (240usize, 180usize);
        let centre = (120.0, 90.0);
        let step = Similarity::from_parts(1.005, 0.01, 0.0, 0.0);
        // Frame k = content transformed by step^k about the image centre.
        let frame = |k: usize| {
            let mut m = Similarity::IDENTITY;
            for _ in 0..k {
                m = m.then(&step);
            }
            let inv = m.inverse().unwrap();
            tex_img(w, h, |x, y| {
                let (u, v) = inv.apply(x - centre.0, y - centre.1);
                (u + centre.0, v + centre.1)
            })
        };
        let mut tr = MotionTracker::new(&frame(0), None, TrackerConfig::default()).unwrap();
        let mut ang = 0.0;
        let mut sc = 1.0;
        for k in 1..=12 {
            let s = tr.step(&frame(k));
            assert!(!s.lost);
            ang += s.motion.angle();
            sc *= s.motion.scale();
        }
        assert!((ang - 0.12).abs() < 0.005, "angle {ang}");
        assert!((sc - 1.005f64.powi(12)).abs() < 0.004, "scale {sc}");
    }

    #[test]
    fn region_tracker_ignores_background_outside_region() {
        // Background static; a textured patch inside the region moves (4, 1) px/frame.
        let (w, h) = (240usize, 180usize);
        let bg = |x: f64, y: f64| texture(x * 0.9 + 400.0, y * 1.1 + 700.0);
        let frame = |k: usize| {
            GrayImage::from_fn(w, h, |x, y| {
                let (xf, yf) = (x as f64, y as f64);
                let (cx, cy) = (90.0 + 4.0 * k as f64, 70.0 + k as f64);
                if (xf - cx).abs() < 40.0 && (yf - cy).abs() < 30.0 {
                    texture(xf - 4.0 * k as f64, yf - k as f64)
                } else {
                    bg(xf, yf)
                }
            })
        };
        let region = Rect::new(55.0, 45.0, 125.0, 95.0);
        let cfg = TrackerConfig {
            follow_region: true,
            model: MotionModel::Translation,
            ..TrackerConfig::default()
        };
        let mut tr = MotionTracker::new(&frame(0), Some(region), cfg).unwrap();
        let start = tr.region().centre();
        let mut last = start;
        for k in 1..=10 {
            let s = tr.step(&frame(k));
            assert!(!s.lost, "lost at {k}");
            last = tr.region().centre();
        }
        assert!(
            (last.x - start.x - 40.0).abs() < 1.0 && (last.y - start.y - 10.0).abs() < 1.0,
            "{last:?}"
        );
    }

    #[test]
    fn tracker_reports_missing_features_and_tiny_images() {
        let flat = GrayImage::from_fn(160, 120, |_, _| 10.0);
        assert!(matches!(
            MotionTracker::new(&flat, None, TrackerConfig::default()),
            Err(TrackingError::NoFeatures)
        ));
        let tiny = GrayImage::new(20, 20);
        assert!(matches!(
            MotionTracker::new(&tiny, None, TrackerConfig::default()),
            Err(TrackingError::ImageTooSmall(20, 20))
        ));
        // Losing the content entirely yields an identity step flagged as lost, not a panic.
        let a = tex_img(160, 120, |x, y| (x, y));
        let mut tr = MotionTracker::new(&a, None, TrackerConfig::default()).unwrap();
        let s = tr.step(&flat);
        assert!(s.lost && s.motion == Similarity::IDENTITY);
    }
}
