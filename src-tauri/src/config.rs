//! Settings: JSON file without secrets + API keys in Windows Credential Manager.
//! Every change from the UI goes through `ConfigPatch` (unknown keys rejected) and `validate`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

pub const LANGS: &[(&str, &str)] = &[
    ("vi", "Vietnamese"), ("en", "English"), ("zh-CN", "Simplified Chinese"), ("zh-TW", "Traditional Chinese"),
    ("ja", "Japanese"), ("ko", "Korean"), ("th", "Thai"), ("id", "Indonesian"), ("ms", "Malay"), ("fr", "French"),
    ("de", "German"), ("es", "Spanish"), ("pt", "Portuguese"), ("it", "Italian"), ("ru", "Russian"),
    ("ar", "Arabic"), ("hi", "Hindi"),
];

pub fn langs_json() -> Value {
    let mut m = serde_json::Map::new();
    for (k, v) in LANGS {
        m.insert((*k).into(), json!(v));
    }
    Value::Object(m)
}

pub fn lang_name(code: &str) -> String {
    LANGS.iter().find(|(k, _)| *k == code).map(|(_, v)| v.to_string()).unwrap_or_else(|| code.to_string())
}

pub fn ai_presets() -> Value {
    json!({
        "deepseek":   { "name": "DeepSeek", "type": "openai", "baseUrl": "https://api.deepseek.com", "model": "deepseek-flash" },
        "openai":     { "name": "OpenAI", "type": "openai", "baseUrl": "https://api.openai.com/v1", "model": "" },
        "anthropic":  { "name": "Claude", "type": "anthropic", "baseUrl": "https://api.anthropic.com", "model": "" },
        "gemini":     { "name": "Gemini", "type": "openai", "baseUrl": "https://generativelanguage.googleapis.com/v1beta/openai", "model": "" },
        "openrouter": { "name": "OpenRouter", "type": "openai", "baseUrl": "https://openrouter.ai/api/v1", "model": "" },
        "ollama":     { "name": "Ollama (local)", "type": "openai", "baseUrl": "http://localhost:11434/v1", "model": "" },
        "custom":     { "name": "Custom", "type": "openai", "baseUrl": "", "model": "" }
    })
}

#[derive(Clone, Serialize, Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Provider {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub enabled: bool,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub config_version: u32,
    pub hotkey_ocr: String,
    pub hotkey_clipboard: String,
    pub hotkey_show: String,
    pub merge_lines: bool,
    pub tone_type: String,
    pub font_size: u32,
    pub pinned: bool,
    pub google_enabled: bool,
    pub google_target: String,
    pub google_api_key: String,
    pub ai_enabled: bool,
    pub ai_target: String,
    pub ai_providers: Vec<Provider>,
    pub tts_engine: String,
    pub tts_win_voice: String,
    pub tts_edge_voice: String,
    pub tts_rate: f64,
    pub auto_speak: bool,
    pub ocr_threads: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: 2,
            hotkey_ocr: "Alt+Q".into(),
            hotkey_clipboard: "Alt+E".into(),
            hotkey_show: "Alt+W".into(),
            merge_lines: true,
            tone_type: "symbol".into(),
            font_size: 22,
            pinned: false,
            google_enabled: true,
            google_target: "en".into(),
            google_api_key: String::new(),
            ai_enabled: true,
            ai_target: "vi".into(),
            ai_providers: vec![],
            tts_engine: "windows".into(),
            tts_win_voice: String::new(),
            tts_edge_voice: "zh-CN-XiaoxiaoNeural".into(),
            tts_rate: 1.0,
            auto_speak: false,
            ocr_threads: 2,
        }
    }
}

/// What the settings UI may change. Unknown fields are an error, so the renderer cannot
/// slip in anything that is not a user setting.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigPatch {
    hotkey_ocr: Option<String>,
    hotkey_clipboard: Option<String>,
    hotkey_show: Option<String>,
    merge_lines: Option<bool>,
    tone_type: Option<String>,
    font_size: Option<u32>,
    pinned: Option<bool>,
    google_enabled: Option<bool>,
    google_target: Option<String>,
    google_api_key: Option<String>,
    ai_enabled: Option<bool>,
    ai_target: Option<String>,
    ai_providers: Option<Vec<Provider>>,
    tts_engine: Option<String>,
    tts_win_voice: Option<String>,
    tts_edge_voice: Option<String>,
    tts_rate: Option<f64>,
    auto_speak: Option<bool>,
    ocr_threads: Option<u32>,
}

impl Config {
    pub fn apply(&self, p: ConfigPatch) -> Result<Config, String> {
        let mut c = self.clone();
        macro_rules! take { ($($f:ident),*) => { $( if let Some(v) = p.$f { c.$f = v; } )* } }
        take!(hotkey_ocr, hotkey_clipboard, hotkey_show, merge_lines, tone_type, font_size, pinned,
              google_enabled, google_target, google_api_key, ai_enabled, ai_target, ai_providers, tts_engine,
              tts_win_voice, tts_edge_voice, tts_rate, auto_speak, ocr_threads);
        c.validate()?;
        Ok(c)
    }

    pub fn validate(&mut self) -> Result<(), String> {
        for hk in [&self.hotkey_ocr, &self.hotkey_clipboard, &self.hotkey_show] {
            if !hk.is_empty() && hk.parse::<tauri_plugin_global_shortcut::Shortcut>().is_err() {
                return Err(format!("Invalid hotkey: {hk}"));
            }
        }
        for t in [&self.google_target, &self.ai_target] {
            if !LANGS.iter().any(|(k, _)| k == t) {
                return Err(format!("Unknown language: {t}"));
            }
        }
        if !matches!(self.tone_type.as_str(), "symbol" | "num") {
            self.tone_type = "symbol".into();
        }
        if !matches!(self.tts_engine.as_str(), "windows" | "edge") {
            self.tts_engine = "windows".into();
        }
        if !valid_edge_voice(&self.tts_edge_voice) {
            return Err(format!("Invalid Edge voice: {}", self.tts_edge_voice));
        }
        self.font_size = self.font_size.clamp(14, 48);
        self.ocr_threads = self.ocr_threads.clamp(1, 16);
        self.tts_rate = if self.tts_rate.is_finite() { self.tts_rate.clamp(0.5, 1.5) } else { 1.0 };
        for p in &mut self.ai_providers {
            if p.id.is_empty() || !p.id.chars().all(|c| c.is_ascii_alphanumeric()) {
                p.id = new_id();
            }
            if !matches!(p.kind.as_str(), "openai" | "anthropic") {
                p.kind = "openai".into();
            }
            p.base_url = p.base_url.trim().to_string();
            if !p.base_url.is_empty() && !(p.base_url.starts_with("https://") || p.base_url.starts_with("http://")) {
                return Err(format!("{}: Base URL must start with https:// or http://", p.name));
            }
        }
        Ok(())
    }
}

/// e.g. zh-CN-XiaoxiaoNeural, zh-CN-liaoning-XiaobeiNeural
pub fn valid_edge_voice(v: &str) -> bool {
    v.len() < 64 && v.ends_with("Neural") && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

// ------------------------------------------------------------------ secrets (Windows Credential Manager)
const SERVICE: &str = "ReadChinese";

fn secret_get(name: &str) -> String {
    keyring::Entry::new(SERVICE, name).and_then(|e| e.get_password()).unwrap_or_default()
}

fn secret_set(name: &str, value: &str) {
    if let Ok(e) = keyring::Entry::new(SERVICE, name) {
        let _ = if value.is_empty() { e.delete_credential() } else { e.set_password(value) };
    }
}

fn provider_secret(id: &str) -> String {
    format!("provider:{id}")
}

// ------------------------------------------------------------------ load / save
pub struct Store {
    pub path: PathBuf,
}

impl Store {
    /// Load the settings; on first run import the Electron version's config.json (keys move to the vault).
    pub fn load(&self) -> Config {
        let parse = |s: String| serde_json::from_str::<Config>(s.trim_start_matches('\u{feff}')).ok(); // tolerate a BOM
        let mut c: Config = match std::fs::read_to_string(&self.path) {
            Ok(s) => parse(s).unwrap_or_else(|| {
                // never silently overwrite a file we cannot read: keep a copy, start from defaults
                let _ = std::fs::copy(&self.path, self.path.with_extension("json.broken"));
                Config::default()
            }),
            // first run: import the Electron version's settings if present
            Err(_) => std::env::var_os("APPDATA")
                .map(|d| PathBuf::from(d).join("ReadChinese").join("config.json"))
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(parse)
                .unwrap_or_default(),
        };
        let _ = c.validate();
        // keys from the vault; a key still present in the file (legacy import) wins and is moved on save
        if c.google_api_key.is_empty() {
            c.google_api_key = secret_get("google");
        }
        for p in &mut c.ai_providers {
            if p.api_key.is_empty() {
                p.api_key = secret_get(&provider_secret(&p.id));
            }
        }
        self.save(&c, None);
        c
    }

    /// Write the file WITHOUT keys; keys go to Credential Manager. `old` = previous config, used to
    /// delete the keys of removed providers.
    pub fn save(&self, c: &Config, old: Option<&Config>) {
        secret_set("google", &c.google_api_key);
        for p in &c.ai_providers {
            secret_set(&provider_secret(&p.id), &p.api_key);
        }
        if let Some(old) = old {
            for p in &old.ai_providers {
                if !c.ai_providers.iter().any(|x| x.id == p.id) {
                    secret_set(&provider_secret(&p.id), "");
                }
            }
        }
        let mut disk = c.clone();
        disk.google_api_key.clear();
        for p in &mut disk.ai_providers {
            p.api_key.clear();
        }
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(&disk).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }
}
