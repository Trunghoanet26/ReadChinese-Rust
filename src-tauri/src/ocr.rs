//! OCR in Rust: PP-OCRv4 detection + direction classifier + recognition (ONNX models run with ONNX Runtime
//! through `ort`). A port of the RapidOCR 1.4 pipeline the Python sidecar used, same models and thresholds,
//! so results match what the app produced before. No Python, no temp files, no IPC.
//!
//! Pipeline: [app pre-processing: invert dark mode, pad 16px] -> limit sides -> letterbox very wide/short
//! images -> DB text detection (bitmap -> components -> min-area boxes -> unclip) -> perspective crop of every
//! box -> 0°/180° classifier -> CTC recognition -> boxes mapped back to the input image.

use image::RgbaImage;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;
use serde::Serialize;
use std::path::Path;
use std::sync::Mutex;

pub const DET_MODEL: &str = "ch_PP-OCRv4_det_infer.onnx";
pub const CLS_MODEL: &str = "ch_ppocr_mobile_v2.0_cls_infer.onnx";
pub const REC_MODEL: &str = "ch_PP-OCRv4_rec_infer.onnx";

// RapidOCR defaults (config.yaml) + the looser thresholds the app used ("TUNE" in ocr_server.py)
const TEXT_SCORE: f32 = 0.3;
const BOX_THRESH: f32 = 0.3;
const UNCLIP_RATIO: f64 = 1.8;
const DET_THRESH: f32 = 0.3;
const DET_LIMIT_SIDE: f64 = 736.0; // limit_type "min"
const MAX_CANDIDATES: usize = 1000;
const MIN_SIZE: f64 = 3.0;
const MAX_SIDE_LEN: usize = 2000;
const MIN_SIDE_LEN: usize = 30;
const MIN_HEIGHT: usize = 30;
const WIDTH_HEIGHT_RATIO: f64 = 8.0;
const CLS_H: usize = 48;
const CLS_W: usize = 192;
const CLS_THRESH: f32 = 0.9;
const REC_H: usize = 48;
const REC_W: usize = 320;
const BATCH: usize = 6;
const PAD: usize = 16; // app pre-processing: border so text touching the selection edge is not missed

#[derive(Debug, Clone, Serialize)]
pub struct Line {
    pub text: String,
    pub score: f32,
    #[serde(rename = "box")]
    pub bbox: Option<Vec<Vec<f64>>>,
}

fn s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ------------------------------------------------------------------ engine

struct Engine {
    det: Session,
    cls: Session,
    rec: Session,
    chars: Vec<String>,
    _env: ort::environment::Environment, // dropped last
}

#[derive(Default)]
pub struct Ocr {
    engine: Mutex<Option<Engine>>,
}

impl Ocr {
    /// Drop the loaded models (e.g. after the thread setting changed); they are loaded again on next use.
    pub fn restart(&self) {
        *self.engine.lock().unwrap() = None;
    }

    /// Load the three models from `dir` once (blocking, ~0.3 s). Call from a blocking thread.
    pub fn ensure(&self, dir: &Path, threads: u32) -> Result<(), String> {
        let mut g = self.engine.lock().unwrap();
        if g.is_some() {
            return Ok(());
        }
        let env = ort::init().with_name("ReadChinese").build().map_err(s)?;
        let load = |name: &str| -> Result<Session, String> {
            let path = dir.join(name);
            if !path.is_file() {
                return Err(format!("OCR model not found: {}", path.display()));
            }
            Session::builder(&env)
                .map_err(s)?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(s)?
                .with_intra_threads(threads.max(1) as usize)
                .map_err(s)?
                .with_inter_threads(1)
                .map_err(s)?
                .commit_from_file(&path)
                .map_err(|e| format!("Cannot load {name}: {e}"))
        };
        let det = load(DET_MODEL)?;
        let cls = load(CLS_MODEL)?;
        let rec = load(REC_MODEL)?;
        // the character dictionary is stored in the recognition model's metadata
        let dict = rec
            .metadata()
            .map_err(s)?
            .custom("character")
            .ok_or("Recognition model has no character list")?;
        let mut chars = vec!["blank".to_string()];
        chars.extend(splitlines(&dict));
        chars.push(" ".into());
        let mut e = Engine { det, cls, rec, chars, _env: env };
        // warm-up so the first real capture is not slow
        let _ = e.run(&Img { w: 160, h: 48, d: vec![255; 160 * 48 * 3] });
        *g = Some(e);
        Ok(())
    }

    pub fn is_loaded(&self) -> bool {
        self.engine.lock().unwrap().is_some()
    }

    /// Recognize text in a screenshot crop (blocking). Boxes are in the crop's pixel coordinates.
    pub fn recognize(&self, rgba: &RgbaImage) -> Result<Vec<Line>, String> {
        let mut g = self.engine.lock().unwrap();
        let e = g.as_mut().ok_or("OCR is not loaded")?;
        let orig = Img::from_rgba(rgba);
        if orig.w == 0 || orig.h == 0 {
            return Ok(vec![]);
        }
        // dark background (dark mode) -> invert to dark text on light background: the model reads it much better
        let mut img = orig.clone();
        if img.gray_mean() < 110.0 {
            img.d.iter_mut().for_each(|v| *v = 255 - *v);
        }
        let color = img.edge_median();
        let padded = img.pad(PAD, PAD, PAD, PAD, color);
        let mut lines = e.run(&padded)?;
        let mut off = PAD as f64;
        if lines.is_empty() {
            lines = e.run(&orig)?; // fallback: the original image
            off = 0.0;
        }
        for l in &mut lines {
            if let Some(b) = &mut l.bbox {
                for p in b.iter_mut() {
                    p[0] -= off;
                    p[1] -= off;
                }
            }
        }
        Ok(lines)
    }
}

/// Python's str.splitlines() (the dictionary was split with it).
fn splitlines(t: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut it = t.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                out.push(std::mem::take(&mut cur));
            }
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
                out.push(std::mem::take(&mut cur))
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl Engine {
    /// RapidOCR.__call__ for one BGR image.
    fn run(&mut self, raw: &Img) -> Result<Vec<Line>, String> {
        let (raw_w, raw_h) = (raw.w as f64, raw.h as f64);
        // limit the image size (sizes rounded to multiples of 32, like RapidOCR)
        let mut img = raw.clone();
        let (mut ratio_w, mut ratio_h) = (1.0f64, 1.0f64);
        if img.w.max(img.h) > MAX_SIDE_LEN {
            let r = MAX_SIDE_LEN as f64 / img.w.max(img.h) as f64;
            let (nw, nh) = (round32(img.w as f64 * r), round32(img.h as f64 * r));
            if nw == 0 || nh == 0 {
                return Ok(vec![]);
            }
            ratio_w *= img.w as f64 / nw as f64;
            ratio_h *= img.h as f64 / nh as f64;
            img = img.resize(nw, nh);
        }
        if img.w.min(img.h) < MIN_SIDE_LEN {
            let r = MIN_SIDE_LEN as f64 / img.w.min(img.h) as f64;
            let (nw, nh) = (round32(img.w as f64 * r), round32(img.h as f64 * r));
            if nw == 0 || nh == 0 {
                return Ok(vec![]);
            }
            ratio_w *= img.w as f64 / nw as f64;
            ratio_h *= img.h as f64 / nh as f64;
            img = img.resize(nw, nh);
        }
        // letterbox short / very wide images (black bars top and bottom)
        let mut top = 0usize;
        if img.h <= MIN_HEIGHT || img.w as f64 / img.h as f64 > WIDTH_HEIGHT_RATIO {
            let new_h = (((img.w as f64 / WIDTH_HEIGHT_RATIO) as usize).max(MIN_HEIGHT) * 2) as f64;
            top = ((new_h - img.h as f64).abs() / 2.0) as usize;
            img = img.pad(top, top, 0, 0, [0, 0, 0]);
        }

        let trace = std::env::var_os("OCR_TRACE").is_some();
        let t0 = std::time::Instant::now();
        let boxes = self.detect(&img)?;
        if boxes.is_empty() {
            return Ok(vec![]);
        }
        let t1 = t0.elapsed();
        let mut crops: Vec<Img> = boxes.iter().map(|b| crop_rotated(&img, b)).collect();
        let t2 = t0.elapsed();
        self.classify(&mut crops)?;
        let t3 = t0.elapsed();
        let texts = self.recognize_crops(&crops)?;
        if trace {
            eprintln!("ocr {}x{}: det {:?} crop {:?} cls {:?} rec {:?} ({} boxes)", img.w, img.h, t1, t2 - t1, t3 - t2, t0.elapsed() - t3, boxes.len());
        }

        let mut out = Vec::new();
        for (b, (text, score)) in boxes.iter().zip(texts) {
            if score < TEXT_SCORE {
                continue;
            }
            let pts = b
                .iter()
                .map(|p| {
                    let x = (p[0] as f64 * ratio_w).clamp(0.0, raw_w);
                    let y = ((p[1] as f64 - top as f64) * ratio_h).clamp(0.0, raw_h);
                    vec![x, y]
                })
                .collect();
            out.push(Line { text, score, bbox: Some(pts) });
        }
        Ok(out)
    }

    // ---------------------------------------------------------- detection (DBNet)
    fn detect(&mut self, img: &Img) -> Result<Vec<[[f32; 2]; 4]>, String> {
        let (w, h) = (img.w, img.h);
        let min = w.min(h) as f64;
        let ratio = if min < DET_LIMIT_SIDE { DET_LIMIT_SIDE / min } else { 1.0 };
        let (rw, rh) = (round32(w as f64 * ratio), round32(h as f64 * ratio));
        if rw == 0 || rh == 0 {
            return Ok(vec![]);
        }
        let resized = img.resize(rw, rh);
        let mut input = vec![0f32; 3 * rw * rh];
        resized.write_chw(&mut input, rw);
        let tensor = Tensor::from_array(([1usize, 3, rh, rw], input)).map_err(s)?;
        let t0 = std::time::Instant::now();
        let outputs = self.det.run(ort::inputs![tensor]).map_err(s)?;
        if std::env::var_os("OCR_TRACE").is_some() {
            eprintln!("  det model {rw}x{rh}: {:?}", t0.elapsed());
        }
        let (shape, pred) = outputs[0].try_extract_tensor::<f32>().map_err(s)?;
        let (ph, pw) = (shape[shape.len() - 2] as usize, shape[shape.len() - 1] as usize);
        let boxes = db_postprocess(&pred[..ph * pw], pw, ph, w, h);

        // filter_tag_det_res: order clockwise, clip to the image, drop tiny boxes
        let mut out = Vec::new();
        for b in boxes {
            let mut b = order_points_clockwise(b);
            for p in b.iter_mut() {
                p[0] = p[0].clamp(0.0, (w - 1) as f32).trunc();
                p[1] = p[1].clamp(0.0, (h - 1) as f32).trunc();
            }
            let rect_w = dist(b[0], b[1]) as i64;
            let rect_h = dist(b[0], b[3]) as i64;
            if rect_w <= 3 || rect_h <= 3 {
                continue;
            }
            out.push(b);
        }
        Ok(out)
    }

    // ---------------------------------------------------------- direction classifier (0° / 180°)
    fn classify(&mut self, crops: &mut [Img]) -> Result<(), String> {
        let order = argsort_by_ratio(crops);
        for chunk in order.chunks(BATCH) {
            let n = chunk.len();
            let mut input = vec![0f32; n * 3 * CLS_H * CLS_W];
            for (k, &i) in chunk.iter().enumerate() {
                let c = &crops[i];
                let ratio = c.w as f64 / c.h as f64;
                let rw = ((CLS_H as f64 * ratio).ceil() as usize).clamp(1, CLS_W);
                c.resize(rw, CLS_H).write_chw(&mut input[k * 3 * CLS_H * CLS_W..], CLS_W);
            }
            let tensor = Tensor::from_array(([n, 3, CLS_H, CLS_W], input)).map_err(s)?;
            let outputs = self.cls.run(ort::inputs![tensor]).map_err(s)?;
            let (shape, prob) = outputs[0].try_extract_tensor::<f32>().map_err(s)?;
            let labels = shape[shape.len() - 1] as usize;
            for (k, &i) in chunk.iter().enumerate() {
                let row = &prob[k * labels..(k + 1) * labels];
                let (idx, p) = argmax(row);
                if idx == 1 && p > CLS_THRESH {
                    crops[i] = crops[i].rotate180();
                }
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------- recognition (CTC)
    fn recognize_crops(&mut self, crops: &[Img]) -> Result<Vec<(String, f32)>, String> {
        let mut res = vec![(String::new(), 0f32); crops.len()];
        let order = argsort_by_ratio(crops);
        for chunk in order.chunks(BATCH) {
            let n = chunk.len();
            let mut max_ratio = REC_W as f64 / REC_H as f64;
            for &i in chunk {
                max_ratio = max_ratio.max(crops[i].w as f64 / crops[i].h as f64);
            }
            let img_w = (REC_H as f64 * max_ratio) as usize;
            let mut input = vec![0f32; n * 3 * REC_H * img_w];
            for (k, &i) in chunk.iter().enumerate() {
                let c = &crops[i];
                let ratio = c.w as f64 / c.h as f64;
                let rw = ((REC_H as f64 * ratio).ceil() as usize).clamp(1, img_w);
                c.resize(rw, REC_H).write_chw(&mut input[k * 3 * REC_H * img_w..], img_w);
            }
            let tensor = Tensor::from_array(([n, 3, REC_H, img_w], input)).map_err(s)?;
            let outputs = self.rec.run(ort::inputs![tensor]).map_err(s)?;
            let (shape, pred) = outputs[0].try_extract_tensor::<f32>().map_err(s)?;
            let (steps, classes) = (shape[1] as usize, shape[2] as usize);
            for (k, &i) in chunk.iter().enumerate() {
                let mut text = String::new();
                let mut confs = Vec::new();
                let mut prev = usize::MAX;
                for t in 0..steps {
                    let row = &pred[(k * steps + t) * classes..(k * steps + t + 1) * classes];
                    let (idx, p) = argmax(row);
                    if idx != prev && idx != 0 {
                        if let Some(ch) = self.chars.get(idx) {
                            text.push_str(ch);
                        }
                        confs.push(p);
                    }
                    prev = idx;
                }
                let score = if confs.is_empty() { 0.0 } else { confs.iter().sum::<f32>() / confs.len() as f32 };
                res[i] = (text, score);
            }
        }
        Ok(res)
    }
}

fn argmax(v: &[f32]) -> (usize, f32) {
    let mut best = (0usize, f32::NEG_INFINITY);
    for (i, &x) in v.iter().enumerate() {
        if x > best.1 {
            best = (i, x);
        }
    }
    best
}

fn argsort_by_ratio(crops: &[Img]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..crops.len()).collect();
    idx.sort_by(|&a, &b| {
        let ra = crops[a].w as f64 / crops[a].h as f64;
        let rb = crops[b].w as f64 / crops[b].h as f64;
        ra.total_cmp(&rb)
    });
    idx
}

/// round(x / 32) * 32 with Python's round-half-to-even, after int() truncation of x.
fn round32(x: f64) -> usize {
    (round_even(x.trunc() / 32.0) * 32.0).max(0.0) as usize
}

/// Python round() / np.round(): halves go to the even neighbour.
fn round_even(x: f64) -> f64 {
    x.round_ties_even()
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

// ------------------------------------------------------------------ DB post-processing

/// Bitmap -> connected text regions -> min-area rectangles -> unclip -> boxes in `dest` coordinates.
fn db_postprocess(pred: &[f32], pw: usize, ph: usize, dest_w: usize, dest_h: usize) -> Vec<[[f32; 2]; 4]> {
    let bin: Vec<bool> = pred.iter().map(|&p| p > DET_THRESH).collect();
    // dilate with a 2x2 kernel (OpenCV anchor (1,1): looks at x-1..x, y-1..y)
    let mut mask = vec![false; pw * ph];
    for y in 0..ph {
        for x in 0..pw {
            let i = y * pw + x;
            mask[i] = bin[i]
                || (x > 0 && bin[i - 1])
                || (y > 0 && bin[i - pw])
                || (x > 0 && y > 0 && bin[i - pw - 1]);
        }
    }

    let mut boxes = Vec::new();
    let mut seen = vec![false; pw * ph];
    let mut stack = Vec::new();
    let mut pts: Vec<(usize, usize)> = Vec::new();
    let mut regions = 0usize;
    for start in 0..pw * ph {
        if !mask[start] || seen[start] {
            continue;
        }
        regions += 1;
        if regions > MAX_CANDIDATES {
            break;
        }
        // flood fill (8-connectivity, like OpenCV's findContours for the outer border)
        pts.clear();
        seen[start] = true;
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % pw, i / pw);
            pts.push((x, y));
            let (x0, x1) = (x.saturating_sub(1), (x + 1).min(pw - 1));
            let (y0, y1) = (y.saturating_sub(1), (y + 1).min(ph - 1));
            for ny in y0..=y1 {
                for nx in x0..=x1 {
                    let j = ny * pw + nx;
                    if mask[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        // hull candidates: left-most and right-most pixel of every row
        let ymin = pts.iter().map(|p| p.1).min().unwrap();
        let ymax = pts.iter().map(|p| p.1).max().unwrap();
        let mut ext = vec![(usize::MAX, 0usize); ymax - ymin + 1];
        for &(x, y) in &pts {
            let e = &mut ext[y - ymin];
            e.0 = e.0.min(x);
            e.1 = e.1.max(x);
        }
        let mut cand = Vec::with_capacity(ext.len() * 2);
        for (k, &(a, b)) in ext.iter().enumerate() {
            let y = (ymin + k) as f64;
            cand.push([a as f64, y]);
            if b != a {
                cand.push([b as f64, y]);
            }
        }
        let Some(rect) = min_area_rect(&cand) else { continue };
        let (bx, sside) = mini_box(rect);
        if sside < MIN_SIZE {
            continue;
        }
        let score = box_score_fast(pred, pw, ph, &bx);
        if score < BOX_THRESH {
            continue;
        }
        // unclip: grow by area * ratio / perimeter on every side (= minAreaRect of pyclipper's round offset)
        let area = polygon_area(&bx);
        let perim: f64 = (0..4).map(|k| dist64(bx[k], bx[(k + 1) % 4])).sum();
        if perim <= 0.0 {
            continue;
        }
        let d = area * UNCLIP_RATIO / perim;
        let (bw, bh) = (dist64(bx[0], bx[1]), dist64(bx[0], bx[3]));
        if bw <= 0.0 || bh <= 0.0 {
            continue;
        }
        let u = [(bx[1][0] - bx[0][0]) / bw, (bx[1][1] - bx[0][1]) / bw];
        let v = [(bx[3][0] - bx[0][0]) / bh, (bx[3][1] - bx[0][1]) / bh];
        let grow = |p: [f64; 2], su: f64, sv: f64| [p[0] + d * (su * u[0] + sv * v[0]), p[1] + d * (su * u[1] + sv * v[1])];
        let grown = [grow(bx[0], -1.0, -1.0), grow(bx[1], 1.0, -1.0), grow(bx[2], 1.0, 1.0), grow(bx[3], -1.0, 1.0)];
        let (gb, gside) = mini_box(Rect { pts: grown, w: bw + 2.0 * d, h: bh + 2.0 * d });
        if gside < MIN_SIZE + 2.0 {
            continue;
        }
        let mut out = [[0f32; 2]; 4];
        for k in 0..4 {
            out[k][0] = round_even(gb[k][0] / pw as f64 * dest_w as f64).clamp(0.0, dest_w as f64) as i64 as f32;
            out[k][1] = round_even(gb[k][1] / ph as f64 * dest_h as f64).clamp(0.0, dest_h as f64) as i64 as f32;
        }
        boxes.push(out);
    }
    boxes
}

fn dist64(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn polygon_area(p: &[[f64; 2]; 4]) -> f64 {
    let mut a = 0.0;
    for k in 0..4 {
        let (p0, p1) = (p[k], p[(k + 1) % 4]);
        a += p0[0] * p1[1] - p1[0] * p0[1];
    }
    a.abs() / 2.0
}

#[derive(Clone, Copy)]
struct Rect {
    pts: [[f64; 2]; 4],
    w: f64,
    h: f64,
}

/// Convex hull (Andrew's monotone chain), counter-clockwise, no collinear points.
fn convex_hull(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut p = points.to_vec();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    p.dedup();
    if p.len() < 3 {
        return p;
    }
    let cross = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut h: Vec<[f64; 2]> = Vec::with_capacity(p.len() * 2);
    for &q in &p {
        while h.len() >= 2 && cross(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
            h.pop();
        }
        h.push(q);
    }
    let lower = h.len() + 1;
    for &q in p.iter().rev().skip(1) {
        while h.len() >= lower && cross(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
            h.pop();
        }
        h.push(q);
    }
    h.pop();
    h
}

/// cv2.minAreaRect + cv2.boxPoints (rotating calipers over the hull edges).
fn min_area_rect(points: &[[f64; 2]]) -> Option<Rect> {
    let hull = convex_hull(points);
    if hull.len() < 3 {
        return None; // a point or a line: width 0 -> dropped by min_size anyway
    }
    let n = hull.len();
    let mut best: Option<(f64, Rect)> = None;
    for i in 0..n {
        let (a, b) = (hull[i], hull[(i + 1) % n]);
        let len = dist64(a, b);
        if len == 0.0 {
            continue;
        }
        let u = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
        let v = [-u[1], u[0]];
        let (mut umin, mut umax, mut vmin, mut vmax) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for p in &hull {
            let pu = p[0] * u[0] + p[1] * u[1];
            let pv = p[0] * v[0] + p[1] * v[1];
            umin = umin.min(pu);
            umax = umax.max(pu);
            vmin = vmin.min(pv);
            vmax = vmax.max(pv);
        }
        let area = (umax - umin) * (vmax - vmin);
        if best.as_ref().is_none_or(|(ba, _)| area < *ba) {
            let at = |su: f64, sv: f64| [su * u[0] + sv * v[0], su * u[1] + sv * v[1]];
            let pts = [at(umin, vmin), at(umax, vmin), at(umax, vmax), at(umin, vmax)];
            best = Some((area, Rect { pts, w: umax - umin, h: vmax - vmin }));
        }
    }
    best.map(|(_, r)| r)
}

/// get_mini_boxes: corners ordered top-left, top-right, bottom-right, bottom-left; plus the short side.
fn mini_box(r: Rect) -> ([[f64; 2]; 4], f64) {
    let mut p = r.pts;
    p.sort_by(|a, b| a[0].total_cmp(&b[0])); // stable, like Python's sorted()
    let (i1, i4) = if p[1][1] > p[0][1] { (0, 1) } else { (1, 0) };
    let (i2, i3) = if p[3][1] > p[2][1] { (2, 3) } else { (3, 2) };
    ([p[i1], p[i2], p[i3], p[i4]], r.w.min(r.h))
}

/// Mean of the probability map inside the box (cv2.fillPoly mask + cv2.mean).
fn box_score_fast(pred: &[f32], w: usize, h: usize, bx: &[[f64; 2]; 4]) -> f32 {
    let xs = bx.iter().map(|p| p[0]);
    let ys = bx.iter().map(|p| p[1]);
    let clampi = |v: f64, hi: usize| (v as i64).clamp(0, hi as i64 - 1) as usize;
    let xmin = clampi(xs.clone().fold(f64::MAX, f64::min).floor(), w);
    let xmax = clampi(xs.fold(f64::MIN, f64::max).ceil(), w);
    let ymin = clampi(ys.clone().fold(f64::MAX, f64::min).floor(), h);
    let ymax = clampi(ys.fold(f64::MIN, f64::max).ceil(), h);
    // polygon relative to (xmin, ymin), truncated to integers like astype(np.int32)
    let poly: Vec<[f64; 2]> = bx
        .iter()
        .map(|p| [(p[0] - xmin as f64).trunc(), (p[1] - ymin as f64).trunc()])
        .collect();
    // orientation of the polygon, so "inside" is the same side of every edge
    let mut area2 = 0.0;
    for k in 0..4 {
        let (a, b) = (poly[k], poly[(k + 1) % 4]);
        area2 += a[0] * b[1] - b[0] * a[1];
    }
    let sign = if area2 >= 0.0 { 1.0 } else { -1.0 };
    let (mut sum, mut cnt) = (0f64, 0usize);
    for y in 0..=(ymax - ymin) {
        for x in 0..=(xmax - xmin) {
            let (px, py) = (x as f64, y as f64);
            let inside = (0..4).all(|k| {
                let (a, b) = (poly[k], poly[(k + 1) % 4]);
                sign * ((b[0] - a[0]) * (py - a[1]) - (b[1] - a[1]) * (px - a[0])) >= -1e-9
            });
            if inside {
                sum += pred[(y + ymin) * w + x + xmin] as f64;
                cnt += 1;
            }
        }
    }
    if cnt == 0 { 0.0 } else { (sum / cnt as f64) as f32 }
}

/// imutils order_points_clockwise: [top-left, top-right, bottom-right, bottom-left].
fn order_points_clockwise(b: [[f32; 2]; 4]) -> [[f32; 2]; 4] {
    let mut p = b;
    p.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let (mut l, mut r) = ([p[0], p[1]], [p[2], p[3]]);
    l.sort_by(|a, b| a[1].total_cmp(&b[1]));
    r.sort_by(|a, b| a[1].total_cmp(&b[1]));
    [l[0], r[0], r[1], l[1]]
}

/// get_rotate_crop_image: perspective-warp the box to an upright rectangle (bicubic, replicate border);
/// tall results (height >= 1.5 x width) are rotated 90° counter-clockwise (vertical text).
fn crop_rotated(img: &Img, b: &[[f32; 2]; 4]) -> Img {
    let p: Vec<[f64; 2]> = b.iter().map(|q| [q[0] as f64, q[1] as f64]).collect();
    let cw = dist64(p[0], p[1]).max(dist64(p[2], p[3])) as usize;
    let ch = dist64(p[0], p[3]).max(dist64(p[1], p[2])) as usize;
    let (cw, ch) = (cw.max(1), ch.max(1));
    let dst = [[0.0, 0.0], [cw as f64, 0.0], [cw as f64, ch as f64], [0.0, ch as f64]];
    let hm = homography(&dst, &[p[0], p[1], p[2], p[3]]);
    let mut out = Img { w: cw, h: ch, d: vec![0; cw * ch * 3] };
    for y in 0..ch {
        for x in 0..cw {
            let (fx, fy) = (x as f64, y as f64);
            let z = hm[6] * fx + hm[7] * fy + hm[8];
            let sx = (hm[0] * fx + hm[1] * fy + hm[2]) / z;
            let sy = (hm[3] * fx + hm[4] * fy + hm[5]) / z;
            let px = img.bicubic(sx, sy);
            let o = (y * cw + x) * 3;
            out.d[o..o + 3].copy_from_slice(&px);
        }
    }
    if ch as f64 / cw as f64 >= 1.5 {
        out = out.rotate90_ccw();
    }
    out
}

/// 3x3 homography (row-major) mapping the 4 `from` points onto the 4 `to` points.
fn homography(from: &[[f64; 2]; 4], to: &[[f64; 2]; 4]) -> [f64; 9] {
    let mut a = [[0f64; 9]; 8];
    for k in 0..4 {
        let ([x, y], [u, v]) = (from[k], to[k]);
        a[2 * k] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        a[2 * k + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    // Gaussian elimination with partial pivoting
    for c in 0..8 {
        let piv = (c..8).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs())).unwrap();
        a.swap(c, piv);
        let d = a[c][c];
        if d.abs() < 1e-12 {
            return [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        }
        for k in c..9 {
            a[c][k] /= d;
        }
        for r in 0..8 {
            if r != c {
                let f = a[r][c];
                if f != 0.0 {
                    for k in c..9 {
                        a[r][k] -= f * a[c][k];
                    }
                }
            }
        }
    }
    [a[0][8], a[1][8], a[2][8], a[3][8], a[4][8], a[5][8], a[6][8], a[7][8], 1.0]
}

// ------------------------------------------------------------------ BGR image helpers (OpenCV-compatible)

#[derive(Clone)]
struct Img {
    w: usize,
    h: usize,
    d: Vec<u8>, // BGR, row-major (the channel order the PP-OCR models were trained on)
}

impl Img {
    fn from_rgba(src: &RgbaImage) -> Img {
        let (w, h) = (src.width() as usize, src.height() as usize);
        let mut d = Vec::with_capacity(w * h * 3);
        for p in src.as_raw().chunks_exact(4) {
            d.extend_from_slice(&[p[2], p[1], p[0]]);
        }
        Img { w, h, d }
    }

    /// Mean of cv2.cvtColor(BGR2GRAY).
    fn gray_mean(&self) -> f64 {
        let mut sum = 0u64;
        for p in self.d.chunks_exact(3) {
            sum += ((p[2] as u32 * 4899 + p[1] as u32 * 9617 + p[0] as u32 * 1868 + 8192) >> 14) as u64;
        }
        sum as f64 / (self.w * self.h).max(1) as f64
    }

    /// Per-channel median of the border pixels (np.median of top, bottom, left, right edges).
    fn edge_median(&self) -> [u8; 3] {
        let mut ch: [Vec<u8>; 3] = Default::default();
        let mut add = |x: usize, y: usize| {
            let i = (y * self.w + x) * 3;
            for c in 0..3 {
                ch[c].push(self.d[i + c]);
            }
        };
        for x in 0..self.w {
            add(x, 0);
        }
        for x in 0..self.w {
            add(x, self.h - 1);
        }
        for y in 0..self.h {
            add(0, y);
        }
        for y in 0..self.h {
            add(self.w - 1, y);
        }
        let mut out = [0u8; 3];
        for c in 0..3 {
            let v = &mut ch[c];
            v.sort_unstable();
            let n = v.len();
            out[c] = if n % 2 == 1 { v[n / 2] } else { ((v[n / 2 - 1] as f64 + v[n / 2] as f64) / 2.0) as u8 };
        }
        out
    }

    fn pad(&self, top: usize, bottom: usize, left: usize, right: usize, color: [u8; 3]) -> Img {
        let (w, h) = (self.w + left + right, self.h + top + bottom);
        let mut d = Vec::with_capacity(w * h * 3);
        for _ in 0..w * h {
            d.extend_from_slice(&color);
        }
        for y in 0..self.h {
            let o = ((y + top) * w + left) * 3;
            d[o..o + self.w * 3].copy_from_slice(&self.d[y * self.w * 3..(y + 1) * self.w * 3]);
        }
        Img { w, h, d }
    }

    /// cv2.resize INTER_LINEAR (half-pixel centres, no anti-aliasing, clamped borders).
    fn resize(&self, dw: usize, dh: usize) -> Img {
        if dw == self.w && dh == self.h {
            return self.clone();
        }
        let axis = |dst: usize, src: usize| -> Vec<(usize, usize, f32)> {
            let f = src as f64 / dst as f64;
            (0..dst)
                .map(|i| {
                    let sx = (i as f64 + 0.5) * f - 0.5;
                    let mut x0 = sx.floor();
                    let mut a = sx - x0;
                    if x0 < 0.0 {
                        x0 = 0.0;
                        a = 0.0;
                    }
                    if x0 >= (src - 1) as f64 {
                        x0 = (src - 1) as f64;
                        a = 0.0;
                    }
                    let x0 = x0 as usize;
                    (x0, (x0 + 1).min(src - 1), a as f32)
                })
                .collect()
        };
        let xs = axis(dw, self.w);
        let ys = axis(dh, self.h);
        let mut d = vec![0u8; dw * dh * 3];
        for (y, &(y0, y1, ay)) in ys.iter().enumerate() {
            let (r0, r1) = (y0 * self.w * 3, y1 * self.w * 3);
            for (x, &(x0, x1, ax)) in xs.iter().enumerate() {
                for c in 0..3 {
                    let p00 = self.d[r0 + x0 * 3 + c] as f32;
                    let p01 = self.d[r0 + x1 * 3 + c] as f32;
                    let p10 = self.d[r1 + x0 * 3 + c] as f32;
                    let p11 = self.d[r1 + x1 * 3 + c] as f32;
                    let top = p00 + (p01 - p00) * ax;
                    let bot = p10 + (p11 - p10) * ax;
                    d[(y * dw + x) * 3 + c] = (top + (bot - top) * ay).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        Img { w: dw, h: dh, d }
    }

    /// Write (x/255 - 0.5) / 0.5 as CHW floats into `out`, rows `stride` wide (the rest stays 0 = padding).
    fn write_chw(&self, out: &mut [f32], stride: usize) {
        let plane = stride * self.h;
        for y in 0..self.h {
            for x in 0..self.w {
                let i = (y * self.w + x) * 3;
                for c in 0..3 {
                    out[c * plane + y * stride + x] = (self.d[i + c] as f32 / 255.0 - 0.5) / 0.5;
                }
            }
        }
    }

    /// Bicubic sample (OpenCV: a = -0.75, 1/32 px sub-pixel steps, BORDER_REPLICATE).
    fn bicubic(&self, sx: f64, sy: f64) -> [u8; 3] {
        let (qx, qy) = ((sx * 32.0).round() as i64, (sy * 32.0).round() as i64);
        let (x0, y0) = (qx.div_euclid(32), qy.div_euclid(32));
        let (fx, fy) = (qx.rem_euclid(32) as f64 / 32.0, qy.rem_euclid(32) as f64 / 32.0);
        let wts = |t: f64| -> [f64; 4] {
            const A: f64 = -0.75;
            let c0 = ((A * (t + 1.0) - 5.0 * A) * (t + 1.0) + 8.0 * A) * (t + 1.0) - 4.0 * A;
            let c1 = ((A + 2.0) * t - (A + 3.0)) * t * t + 1.0;
            let c2 = ((A + 2.0) * (1.0 - t) - (A + 3.0)) * (1.0 - t) * (1.0 - t) + 1.0;
            [c0, c1, c2, 1.0 - c0 - c1 - c2]
        };
        let (wx, wy) = (wts(fx), wts(fy));
        let mut acc = [0f64; 3];
        for (j, wyj) in wy.iter().enumerate() {
            let yy = (y0 - 1 + j as i64).clamp(0, self.h as i64 - 1) as usize;
            for (i, wxi) in wx.iter().enumerate() {
                let xx = (x0 - 1 + i as i64).clamp(0, self.w as i64 - 1) as usize;
                let o = (yy * self.w + xx) * 3;
                let wgt = wyj * wxi;
                for c in 0..3 {
                    acc[c] += self.d[o + c] as f64 * wgt;
                }
            }
        }
        [0, 1, 2].map(|c| acc[c].round().clamp(0.0, 255.0) as u8)
    }

    /// np.rot90 (counter-clockwise).
    fn rotate90_ccw(&self) -> Img {
        let (w, h) = (self.h, self.w);
        let mut d = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let s = (x * self.w + (self.w - 1 - y)) * 3;
                let o = (y * w + x) * 3;
                d[o..o + 3].copy_from_slice(&self.d[s..s + 3]);
            }
        }
        Img { w, h, d }
    }

    fn rotate180(&self) -> Img {
        let mut d = Vec::with_capacity(self.d.len());
        for p in self.d.chunks_exact(3).rev() {
            d.extend_from_slice(p);
        }
        Img { w: self.w, h: self.h, d }
    }
}

// ------------------------------------------------------------------ reading order (port of main.js)
fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3000..=0x303F | 0x3400..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

struct Row { top: f64, bottom: f64, items: Vec<(f64, String)> }

fn order_lines(lines: &[Line]) -> Vec<(String, bool)> {
    let mut boxes: Vec<(f64, f64, f64, String)> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.text.trim().is_empty())
        .map(|(i, l)| match l.bbox.as_ref().filter(|b| !b.is_empty()) {
            Some(pts) => {
                let ys = pts.iter().filter_map(|p| p.get(1).copied());
                let xs = pts.iter().filter_map(|p| p.first().copied());
                (ys.clone().fold(f64::MAX, f64::min), ys.fold(f64::MIN, f64::max), xs.fold(f64::MAX, f64::min), l.text.trim().to_string())
            }
            None => (i as f64 * 1000.0, i as f64 * 1000.0 + 1.0, 0.0, l.text.trim().to_string()),
        })
        .collect();
    boxes.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut rows: Vec<Row> = Vec::new();
    for (top, bottom, left, text) in boxes {
        let h = bottom - top;
        let row = rows.iter_mut().find(|r| r.bottom.min(bottom) - r.top.max(top) > 0.5 * h.min(r.bottom - r.top));
        match row {
            Some(r) => { r.items.push((left, text)); r.top = r.top.min(top); r.bottom = r.bottom.max(bottom); }
            None => rows.push(Row { top, bottom, items: vec![(left, text)] }),
        }
    }
    rows.sort_by(|a, b| a.top.total_cmp(&b.top));
    let mut out = Vec::new();
    for i in 0..rows.len() {
        let gap = if i > 0 { rows[i].top - rows[i - 1].bottom } else { 0.0 };
        let r = &mut rows[i];
        r.items.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut text = String::new();
        for (_, t) in &r.items {
            let alnum_join = text.chars().last().is_some_and(|c| c.is_ascii_alphanumeric())
                && t.chars().next().is_some_and(|c| c.is_ascii_alphanumeric());
            if alnum_join { text.push(' '); }
            text.push_str(t);
        }
        out.push((text, i > 0 && gap > 0.8 * (r.bottom - r.top)));
    }
    out
}

pub fn join_lines(lines: &[Line], merge: bool) -> String {
    let rows = order_lines(lines);
    if !merge {
        return rows.into_iter().map(|(t, _)| t).collect::<Vec<_>>().join("\n");
    }
    let mut out = String::new();
    for (t, new_para) in rows {
        if out.is_empty() {
            out = t;
        } else if new_para || out.ends_with(['。', '！', '？', '!', '?', '…']) {
            out.push('\n');
            out.push_str(&t);
        } else if out.chars().last().is_some_and(is_cjk) && t.chars().next().is_some_and(is_cjk) {
            out.push_str(&t); // Chinese line wrapped mid-sentence -> join without space
        } else {
            out.push(' ');
            out.push_str(&t);
        }
    }
    out
}
