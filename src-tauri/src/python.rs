//! Running the Python sidecar. Packages are pinned to exact versions and never auto-upgraded.

use std::time::Duration;
use tokio::io::AsyncReadExt;

pub const RAPIDOCR: &str = "rapidocr_onnxruntime==1.4.4";
pub const EDGE_TTS: &str = "edge-tts==7.3.1";

pub fn command(python: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(python);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c.env("PYTHONIOENCODING", "utf-8").kill_on_drop(true);
    c
}

/// Run `python args...`; returns (success, tail of stderr).
pub async fn run(python: &str, args: &[&str], timeout: Duration) -> (bool, String) {
    let mut child = match command(python)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (false, format!("Cannot start Python (\"{python}\"): {e}")),
    };
    let mut stderr = child.stderr.take();
    let work = async {
        let mut err = Vec::new();
        if let Some(s) = stderr.as_mut() {
            let _ = s.read_to_end(&mut err).await;
        }
        let ok = child.wait().await.map(|s| s.success()).unwrap_or(false);
        (ok, err)
    };
    match tokio::time::timeout(timeout, work).await {
        Ok((ok, err)) => {
            let s = String::from_utf8_lossy(&err);
            let tail: String = s.chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
            (ok, tail)
        }
        Err(_) => (false, "timed out".into()),
    }
}

/// `pip install <pinned spec>` (no -U: what gets installed is exactly what the app was tested with)
pub async fn pip_install(python: &str, spec: &str) -> bool {
    run(python, &["-m", "pip", "install", "--disable-pip-version-check", spec], Duration::from_secs(600)).await.0
}
