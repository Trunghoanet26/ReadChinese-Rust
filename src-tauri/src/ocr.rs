//! Persistent Python RapidOCR process (sidecar), one JSON request/response per line on stdin/stdout.
//! Images are sent inline (base64 PNG) -> nothing is written to disk.

use crate::python;
use base64::Engine as _;
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

#[derive(Deserialize, Debug, Clone)]
pub struct Line {
    #[serde(default)]
    pub text: String,
    #[serde(rename = "box", default)]
    pub bbox: Option<Vec<Vec<f64>>>,
}

#[derive(Deserialize)]
struct Reply {
    id: Option<u64>,
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    ready: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    lines: Vec<Line>,
}

struct Proc {
    _child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    stderr_tail: Arc<StdMutex<String>>,
    seq: u64,
}

#[derive(Default)]
pub struct Ocr {
    proc: Mutex<Option<Proc>>,
}

impl Ocr {
    pub async fn restart(&self) {
        *self.proc.lock().await = None; // kill_on_drop ends the old process
    }

    async fn spawn(python: &str, script: &PathBuf, threads: u32) -> Result<Proc, String> {
        let t = threads.to_string();
        let mut child = python::command(python)
            .arg("-u")
            .arg(script)
            .env("OCR_THREADS", &t)
            .env("OMP_NUM_THREADS", &t)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Cannot start Python (\"{python}\"): {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let mut stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?).lines();
        // drain stderr continuously (onnxruntime logs a lot; a full pipe would block Python)
        let stderr_tail = Arc::new(StdMutex::new(String::new()));
        if let Some(mut err) = child.stderr.take() {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = err.read(&mut buf).await {
                    if n == 0 { break; }
                    let mut t = tail.lock().unwrap();
                    t.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if t.len() > 4000 {
                        let mut cut = t.len() - 2000;
                        while !t.is_char_boundary(cut) { cut += 1; }
                        *t = t[cut..].to_string();
                    }
                }
            });
        }
        // wait for {"id":0, "ready":true} (model loading can take a while on first run)
        let first = tokio::time::timeout(Duration::from_secs(180), async {
            while let Ok(Some(line)) = stdout.next_line().await {
                if let Ok(r) = serde_json::from_str::<Reply>(&line) {
                    if r.id == Some(0) { return Some(r); }
                }
            }
            None
        })
        .await
        .map_err(|_| "OCR start timed out".to_string())?;
        match first {
            Some(r) if r.ready => Ok(Proc { _child: child, stdin, stdout, stderr_tail, seq: 1 }),
            Some(r) => Err(r.error.unwrap_or_else(|| "OCR init failed".into())),
            None => Err(format!("OCR process exited. {}", stderr_tail.lock().unwrap())),
        }
    }

    /// Start the process; if RapidOCR is missing install the pinned version once, then retry.
    pub async fn ensure(&self, python: &str, script: &PathBuf, threads: u32, status: impl Fn(&str)) -> Result<(), String> {
        let mut guard = self.proc.lock().await;
        if guard.is_some() { return Ok(()); }
        match Self::spawn(python, script, threads).await {
            Ok(p) => { *guard = Some(p); Ok(()) }
            Err(e) if e.contains("RapidOCR") => {
                status("First run: installing RapidOCR (1–3 min)…");
                if !python::pip_install(python, python::RAPIDOCR).await {
                    return Err(format!("Could not install RapidOCR. Run: {python} -m pip install {}", python::RAPIDOCR));
                }
                status("RapidOCR installed, recognizing…");
                *guard = Some(Self::spawn(python, script, threads).await?);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub async fn recognize(&self, png: &[u8]) -> Result<Vec<Line>, String> {
        let mut guard = self.proc.lock().await;
        let p = guard.as_mut().ok_or("OCR is not running")?;
        let id = p.seq;
        p.seq += 1;
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);
        let req = format!("{{\"id\":{id},\"b64\":\"{b64}\"}}\n");
        let res: Result<Vec<Line>, String> = async {
            p.stdin.write_all(req.as_bytes()).await.map_err(|e| e.to_string())?;
            p.stdin.flush().await.map_err(|e| e.to_string())?;
            loop {
                let line = tokio::time::timeout(Duration::from_secs(60), p.stdout.next_line())
                    .await
                    .map_err(|_| "OCR timed out".to_string())?
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("OCR process exited. {}", p.stderr_tail.lock().unwrap()))?;
                let Ok(r) = serde_json::from_str::<Reply>(&line) else { continue };
                if r.id != Some(id) { continue; }
                return if r.ok { Ok(r.lines) } else { Err(r.error.unwrap_or_else(|| "OCR failed".into())) };
            }
        }
        .await;
        if res.is_err() {
            *guard = None; // broken process -> restart next time
        }
        res
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
