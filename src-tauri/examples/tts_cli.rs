//! Dev tool: synthesize text with Edge TTS (no Python) and write an mp3.
//!   cargo run --release --example tts_cli -- <voice> <rate e.g. +0%> <out.mp3> <text>
#[allow(dead_code)]
#[path = "../src/tts.rs"]
mod tts;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let t = std::time::Instant::now();
    match tauri::async_runtime::block_on(tts::synthesize(&a[3], &a[0], &a[1])) {
        Ok(mp3) => {
            std::fs::write(&a[2], &mp3).unwrap();
            println!("ok: {} bytes in {} ms", mp3.len(), t.elapsed().as_millis());
        }
        Err(e) => println!("error: {e}"),
    }
}
