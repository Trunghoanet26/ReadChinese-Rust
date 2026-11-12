//! Dev tool: run the Rust OCR on image files and print one JSON object per image.
//!   cargo run --release --example ocr_cli -- <models dir> <threads> <image>...
#[path = "../src/ocr.rs"]
mod ocr;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (dir, threads) = (std::path::PathBuf::from(&args[0]), args[1].parse().unwrap_or(2));
    let t = std::time::Instant::now();
    let engine = ocr::Ocr::default();
    engine.ensure(&dir, threads).expect("load models");
    eprintln!("models loaded in {} ms", t.elapsed().as_millis());
    for path in &args[2..] {
        let img = image::open(path).expect("open image").to_rgba8();
        let t = std::time::Instant::now();
        let lines = engine.recognize(&img).expect("ocr");
        let ms = t.elapsed().as_millis();
        let text = ocr::join_lines(&lines, false);
        println!("{}", serde_json::json!({ "path": path, "ms": ms, "text": text, "lines": lines }));
    }
}
