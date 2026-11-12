//! Edge TTS through the Python `edge-tts` package (pinned version), cached as mp3 files.

use crate::python;
use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::Mutex;

const CACHE_MAX: usize = 500;

#[derive(Default)]
pub struct Tts {
    checked: Mutex<bool>,
}

fn explain(err: &str) -> String {
    if err.contains("403") || err.contains("Invalid response status") {
        return format!(
            "Edge TTS was rejected by Microsoft (403). Common causes: the PC clock is off, a VPN/proxy, or Microsoft \
             changed its API (then a newer edge-tts than the pinned {} is needed). Use the Windows voice meanwhile.",
            python::EDGE_TTS
        );
    }
    let lower = err.to_lowercase();
    if ["getaddrinfo", "clientconnectorerror", "cannot connect", "timed out", "timeout"].iter().any(|s| lower.contains(s)) {
        return "Edge TTS needs Internet for text that is not cached yet — cannot reach Microsoft.".into();
    }
    let last = err.trim().lines().filter(|l| !l.trim().is_empty()).last().unwrap_or("unknown error");
    format!("Edge TTS error: {}", last.chars().take(200).collect::<String>())
}

impl Tts {
    async fn ensure(&self, py: &str, status: &impl Fn(&str)) -> Result<(), String> {
        let mut ok = self.checked.lock().await;
        if *ok { return Ok(()); }
        if !python::run(py, &["-c", "import edge_tts"], Duration::from_secs(30)).await.0 {
            status("Installing edge-tts (first use)…");
            let installed = python::pip_install(py, python::EDGE_TTS).await;
            status("");
            if !installed {
                return Err(format!("Could not install edge-tts. Run: {py} -m pip install {}", python::EDGE_TTS));
            }
        }
        *ok = true;
        Ok(())
    }

    /// Returns a data: URL with the mp3.
    pub async fn speak(&self, py: &str, cache: &Path, text: &str, voice: &str, rate: f64, status: impl Fn(&str)) -> Result<String, String> {
        let pct = ((if rate.is_finite() { rate } else { 1.0 }) * 100.0 - 100.0).round() as i64;
        let rate_str = format!("{}{pct}%", if pct >= 0 { "+" } else { "" });
        let key = format!("{:x}", Sha256::digest(format!("{voice}|{rate_str}|{text}")));
        let out = cache.join(format!("{}.mp3", &key[..40]));
        if !out.exists() {
            self.ensure(py, &status).await?;
            tokio::fs::create_dir_all(cache).await.map_err(|e| e.to_string())?;
            // text goes through a file so very long / special text never ends up on the command line
            let txt = std::env::temp_dir().join(format!("readchinese_tts_{}.txt", &key[..16]));
            tokio::fs::write(&txt, text).await.map_err(|e| e.to_string())?;
            let part: PathBuf = out.with_extension("mp3.part");
            let (ok, err) = python::run(
                py,
                &["-m", "edge_tts", "--voice", voice, &format!("--rate={rate_str}"),
                  "--file", &txt.to_string_lossy(), "--write-media", &part.to_string_lossy()],
                Duration::from_secs(60),
            ).await;
            let _ = tokio::fs::remove_file(&txt).await;
            if !ok || !part.exists() {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(explain(&err));
            }
            tokio::fs::rename(&part, &out).await.map_err(|e| e.to_string())?;
            trim_cache(cache);
        }
        let bytes = tokio::fs::read(&out).await.map_err(|e| e.to_string())?;
        Ok(format!("data:audio/mpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
    }
}

fn trim_cache(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= CACHE_MAX { return; }
    files.sort();
    let extra = files.len() - CACHE_MAX;
    for (_, f) in files.into_iter().take(extra) {
        let _ = std::fs::remove_file(f);
    }
}
