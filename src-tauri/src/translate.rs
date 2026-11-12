//! Google Translate (free endpoints with rotation, or the official API) + AI providers with streaming.

use crate::config::{lang_name, Config, Provider};
use crate::AppState;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

#[derive(Clone, Copy)]
enum Parse { Dj, T }

struct Endpoint { name: &'static str, base: &'static str, extra: &'static [(&'static str, &'static str)], parse: Parse }

const GOOGLE_ENDPOINTS: &[Endpoint] = &[
    Endpoint { name: "translate.googleapis.com (gtx)", base: "https://translate.googleapis.com/translate_a/single",
        extra: &[("client", "gtx"), ("dt", "t"), ("dj", "1"), ("ie", "UTF-8"), ("oe", "UTF-8")], parse: Parse::Dj },
    Endpoint { name: "translate.google.com (gtx)", base: "https://translate.google.com/translate_a/single",
        extra: &[("client", "gtx"), ("dt", "t"), ("dj", "1"), ("ie", "UTF-8"), ("oe", "UTF-8")], parse: Parse::Dj },
    Endpoint { name: "clients5.google.com (dict-chrome-ex)", base: "https://clients5.google.com/translate_a/t",
        extra: &[("client", "dict-chrome-ex")], parse: Parse::T },
    Endpoint { name: "translate.googleapis.com (t)", base: "https://translate.googleapis.com/translate_a/t",
        extra: &[("client", "gtx")], parse: Parse::T },
];

fn parse_dj(v: &Value) -> String {
    v["sentences"].as_array().map(|a| a.iter().filter_map(|s| s["trans"].as_str()).collect()).unwrap_or_default()
}
fn parse_t(v: &Value) -> String {
    match v.as_array() {
        Some(a) => a.iter().filter_map(|x| x.as_array().map_or(x.as_str(), |y| y.first().and_then(|z| z.as_str()))).collect(),
        None => parse_dj(v),
    }
}

pub struct Translator {
    client: reqwest::Client,
    job: AtomicU64,
    tasks: Mutex<Vec<tauri::async_runtime::JoinHandle<()>>>,
    google: Mutex<(usize, HashMap<usize, Instant>)>, // (preferred endpoint, cooldown until)
}

impl Default for Translator {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build().expect("http client"),
            job: AtomicU64::new(0),
            tasks: Mutex::new(Vec::new()),
            google: Mutex::new((0, HashMap::new())),
        }
    }
}

impl Translator {
    async fn google_official(&self, text: &str, tl: &str, key: &str) -> Result<String, String> {
        // key in a header, not in the URL (URLs end up in logs/proxies)
        let res = self.client
            .post("https://translation.googleapis.com/language/translate/v2")
            .header("X-Goog-Api-Key", key)
            .json(&json!({ "q": text, "target": tl, "format": "text" }))
            .timeout(Duration::from_secs(15))
            .send().await.map_err(|e| e.to_string())?;
        let status = res.status();
        let data: Value = res.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(format!("Google API {}: {}", status.as_u16(), data["error"]["message"].as_str().unwrap_or("")));
        }
        Ok(data["data"]["translations"].as_array().map(|a| {
            a.iter().filter_map(|t| t["translatedText"].as_str()).collect::<Vec<_>>().join("\n")
        }).unwrap_or_default())
    }

    pub async fn google(&self, text: &str, tl: &str, key: &str) -> Result<String, String> {
        if !key.is_empty() {
            return self.google_official(text, tl, key).await;
        }
        let n = GOOGLE_ENDPOINTS.len();
        let preferred = self.google.lock().unwrap().0;
        let mut errors = Vec::new();
        for k in 0..n {
            let i = (preferred + k) % n;
            let ep = &GOOGLE_ENDPOINTS[i];
            if self.google.lock().unwrap().1.get(&i).is_some_and(|t| *t > Instant::now()) {
                errors.push(format!("{}: resting after 429", ep.name));
                continue;
            }
            let mut params: Vec<(&str, &str)> = ep.extra.to_vec();
            params.extend([("sl", "auto"), ("tl", tl), ("q", text)]);
            let url = reqwest::Url::parse_with_params(ep.base, &params).map_err(|e| e.to_string())?;
            let res = self.client.get(url)
                .header(USER_AGENT, BROWSER_UA)
                .header(ACCEPT, "application/json, text/plain, */*")
                .header(ACCEPT_LANGUAGE, "en-US,en;q=0.9")
                .timeout(Duration::from_secs(8))
                .send().await;
            let res = match res {
                Ok(r) => r,
                Err(e) => { errors.push(format!("{}: {}", ep.name, if e.is_timeout() { "timeout".into() } else { e.to_string() })); continue; }
            };
            let s = res.status().as_u16();
            if s == 429 || s == 403 || s >= 500 {
                let rest = if s == 429 { 600 } else { 60 };
                self.google.lock().unwrap().1.insert(i, Instant::now() + Duration::from_secs(rest));
                errors.push(format!("{}: HTTP {s}", ep.name));
                continue;
            }
            if !res.status().is_success() { errors.push(format!("{}: HTTP {s}", ep.name)); continue; }
            let v: Value = match res.json().await { Ok(v) => v, Err(e) => { errors.push(format!("{}: {e}", ep.name)); continue; } };
            let out = match ep.parse { Parse::Dj => parse_dj(&v), Parse::T => parse_t(&v) };
            if out.is_empty() { errors.push(format!("{}: empty response", ep.name)); continue; }
            let mut g = self.google.lock().unwrap();
            g.0 = i;
            g.1.remove(&i);
            return Ok(out);
        }
        let blocked = errors.iter().any(|e| e.contains("HTTP 429") || e.contains("HTTP 403") || e.contains("resting"));
        Err(format!("{}\n{}", if blocked {
            "Google is temporarily rate-limiting this network/IP (429). Try again in a few minutes, turn off VPN/proxy, or add a Google Cloud API key in Settings."
        } else {
            "Cannot reach Google Translate (check your network/proxy)."
        }, errors.join("\n")))
    }

    pub async fn ai_test(&self, p: &Provider) -> Value {
        let base = ai_base(p);
        if base.is_empty() { return json!({ "ok": false, "error": "Enter a Base URL." }); }
        if p.api_key.is_empty() && p.kind == "anthropic" { return json!({ "ok": false, "error": "Enter an API key." }); }
        let url = if p.kind == "anthropic" { format!("{base}/v1/models?limit=100") } else { format!("{base}/models") };
        let res = match self.client.get(&url).headers(ai_headers(p)).timeout(Duration::from_secs(10)).send().await {
            Ok(r) => r,
            Err(e) => return json!({ "ok": false, "error": format!("Cannot connect to {base}: {e}") }),
        };
        let status = res.status().as_u16();
        let body = res.text().await.unwrap_or_default();
        if status == 401 || status == 403 { return json!({ "ok": false, "error": format!("Invalid API key ({status}).") }); }
        if !(200..300).contains(&status) {
            return json!({ "ok": false, "error": format!("HTTP {status}: {}", body.chars().take(200).collect::<String>()) });
        }
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let list = v["data"].as_array().or(v["models"].as_array()).cloned().unwrap_or_default();
        let mut models: Vec<String> = list.iter()
            .filter_map(|m| m["id"].as_str().or(m["name"].as_str()))
            .map(|m| m.trim_start_matches("models/").to_string())
            .collect();
        models.sort();
        json!({ "ok": true, "models": models })
    }

    async fn ai_translate(&self, p: &Provider, text: &str, target: &str, mut on_delta: impl FnMut(&str)) -> Result<(), String> {
        if p.model.is_empty() {
            return Err(format!("{}: no model selected. Open Settings and press \"Test\" to load models.", p.name));
        }
        if p.api_key.is_empty() && p.kind == "anthropic" {
            return Err(format!("{}: missing API key (Settings).", p.name));
        }
        let base = ai_base(p);
        let system = format!(
            "You are a professional translator. Translate the user's text (usually Chinese) into natural, \
             context-appropriate {}. Keep line breaks. Output only the translation: no explanations, no pinyin, no quotes.",
            lang_name(target)
        );
        let anthropic = p.kind == "anthropic";
        let (url, body) = if anthropic {
            (format!("{base}/v1/messages"), json!({
                "model": p.model, "max_tokens": 4096, "stream": true, "system": system,
                "messages": [{ "role": "user", "content": text }],
            }))
        } else {
            (format!("{base}/chat/completions"), json!({
                "model": p.model, "stream": true,
                "messages": [{ "role": "system", "content": system }, { "role": "user", "content": text }],
            }))
        };
        let res = self.client.post(url).headers(ai_headers(p)).json(&body).send().await
            .map_err(|e| format!("{}: {e}", p.name))?;
        let status = res.status().as_u16();
        if !(200..300).contains(&status) {
            let body: String = res.text().await.unwrap_or_default().chars().take(300).collect();
            return Err(ai_http_error(p, status, &body));
        }
        // Server-Sent Events: handle every "data:" line
        let mut stream = res.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk.map_err(|e| format!("{}: {e}", p.name))?);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let raw: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&raw);
                let Some(payload) = line.trim().strip_prefix("data:") else { continue };
                let payload = payload.trim();
                if payload == "[DONE]" { return Ok(()); }
                let Ok(ev) = serde_json::from_str::<Value>(payload) else { continue };
                if anthropic {
                    match ev["type"].as_str() {
                        Some("content_block_delta") => if let Some(t) = ev["delta"]["text"].as_str() { on_delta(t) },
                        Some("error") => return Err(format!("{}: {}", p.name, ev["error"]["message"].as_str().unwrap_or("stream error"))),
                        Some("message_stop") => return Ok(()),
                        _ => {}
                    }
                } else {
                    if !ev["error"].is_null() {
                        return Err(format!("{}: {}", p.name, ev["error"]["message"].as_str().map(String::from).unwrap_or(ev["error"].to_string())));
                    }
                    if let Some(t) = ev["choices"][0]["delta"]["content"].as_str() { on_delta(t) }
                }
            }
        }
        Ok(())
    }
}

fn ai_base(p: &Provider) -> String {
    let fallback = if p.kind == "anthropic" { "https://api.anthropic.com" } else { "https://api.openai.com/v1" };
    let mut b = if p.base_url.trim().is_empty() { fallback.to_string() } else { p.base_url.trim().to_string() };
    while b.ends_with('/') { b.pop(); }
    let strip = |b: &mut String, s: &str| if b.ends_with(s) { b.truncate(b.len() - s.len()) };
    if p.kind == "anthropic" {
        strip(&mut b, "/v1/messages"); strip(&mut b, "/messages"); strip(&mut b, "/v1");
    } else {
        strip(&mut b, "/chat/completions");
    }
    b
}

fn ai_headers(p: &Provider) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if p.kind == "anthropic" {
        if let Ok(v) = HeaderValue::from_str(&p.api_key) { h.insert("x-api-key", v); }
        h.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    } else if !p.api_key.is_empty() {
        if let Ok(mut v) = HeaderValue::from_str(&format!("Bearer {}", p.api_key)) {
            v.set_sensitive(true);
            h.insert(AUTHORIZATION, v);
        }
    }
    h
}

fn ai_http_error(p: &Provider, status: u16, body: &str) -> String {
    let who = if p.name.is_empty() { "AI" } else { &p.name };
    match status {
        401 | 403 => format!("{who}: invalid API key ({status}). Check it in Settings."),
        402 => format!("{who}: insufficient account balance (402)."),
        429 => format!("{who}: rate limited or out of quota (429)."),
        404 => format!("{who}: model \"{}\" or endpoint not found (404). Use \"Test\" in Settings to pick a valid model. {body}", p.model),
        400 if body.to_lowercase().contains("model") => format!("{who}: model \"{}\" not accepted (400). {body}", p.model),
        _ => format!("{who} HTTP {status}: {body}"),
    }
}

/// Start Google + every enabled AI provider in parallel; results stream to the main window.
pub fn translate_all(app: &AppHandle, text: String) {
    let state = app.state::<AppState>();
    let cfg: Config = state.config.lock().unwrap().clone();
    let tr = &state.translator;
    let id = tr.job.fetch_add(1, Ordering::SeqCst) + 1;
    for t in tr.tasks.lock().unwrap().drain(..) { t.abort(); }

    let mut cards = Vec::new();
    if cfg.google_enabled {
        cards.push(json!({ "key": "google", "kind": "google", "name": "Google", "lang": cfg.google_target }));
    }
    let providers: Vec<Provider> = if cfg.ai_enabled { cfg.ai_providers.iter().filter(|p| p.enabled).cloned().collect() } else { vec![] };
    for p in &providers {
        cards.push(json!({ "key": p.id, "kind": "ai", "name": p.name, "model": p.model, "lang": cfg.ai_target }));
    }
    let _ = app.emit_to("main", "tr:start", json!({ "id": id, "cards": cards }));

    let mut handles = Vec::new();
    if cfg.google_enabled {
        let (app, text, tl, key) = (app.clone(), text.clone(), cfg.google_target.clone(), cfg.google_api_key.clone());
        handles.push(tauri::async_runtime::spawn(async move {
            let st = app.state::<AppState>();
            let msg = match st.translator.google(&text, &tl, &key).await {
                Ok(t) => json!({ "id": id, "key": "google", "text": t }),
                Err(e) => json!({ "id": id, "key": "google", "error": e }),
            };
            let _ = app.emit_to("main", "tr:done", msg);
        }));
    }
    for p in providers {
        let (app, text, target) = (app.clone(), text.clone(), cfg.ai_target.clone());
        handles.push(tauri::async_runtime::spawn(async move {
            let st = app.state::<AppState>();
            let key = p.id.clone();
            let emit_app = app.clone();
            let res = st.translator.ai_translate(&p, &text, &target, |d| {
                let _ = emit_app.emit_to("main", "tr:delta", json!({ "id": id, "key": key, "delta": d }));
            }).await;
            let msg = match res {
                Ok(()) => json!({ "id": id, "key": p.id }),
                Err(e) => json!({ "id": id, "key": p.id, "error": e }),
            };
            let _ = app.emit_to("main", "tr:done", msg);
        }));
    }
    *tr.tasks.lock().unwrap() = handles;
}
