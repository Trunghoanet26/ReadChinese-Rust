//! Edge TTS ("Read aloud" voices of Microsoft Edge) spoken directly over its websocket, no Python.
//! A port of the `edge-tts` 7.3.1 package: Sec-MS-GEC v2 token, speech.config + SSML messages,
//! mp3 frames collected from the binary messages. Results are cached as mp3 files.

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

const CACHE_MAX: usize = 500;

const BASE_URL: &str = "speech.platform.bing.com/consumer/speech/synthesize/readaloud";
const TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
const CHROMIUM_FULL_VERSION: &str = "143.0.3650.75";
const CHROMIUM_MAJOR_VERSION: &str = "143";
const MAX_CHUNK_BYTES: usize = 4096;

/// Server clock minus local clock (seconds), learned from a 403 response's Date header.
static CLOCK_SKEW: AtomicI64 = AtomicI64::new(0);

#[derive(Default)]
pub struct Tts;

impl Tts {
    /// Returns a data: URL with the mp3.
    pub async fn speak(&self, cache: &Path, text: &str, voice: &str, rate: f64) -> Result<String, String> {
        let pct = ((if rate.is_finite() { rate } else { 1.0 }) * 100.0 - 100.0).round() as i64;
        let rate_str = format!("{}{pct}%", if pct >= 0 { "+" } else { "" });
        let key = format!("{:x}", Sha256::digest(format!("{voice}|{rate_str}|{text}")));
        let out = cache.join(format!("{}.mp3", &key[..40]));
        if !out.exists() {
            let mp3 = synthesize(text, voice, &rate_str).await?;
            tokio::fs::create_dir_all(cache).await.map_err(|e| e.to_string())?;
            let part: PathBuf = out.with_extension("mp3.part");
            tokio::fs::write(&part, &mp3).await.map_err(|e| e.to_string())?;
            tokio::fs::rename(&part, &out).await.map_err(|e| e.to_string())?;
            trim_cache(cache);
        }
        let bytes = tokio::fs::read(&out).await.map_err(|e| e.to_string())?;
        Ok(format!("data:audio/mpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
    }
}

fn trim_cache(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(SystemTime, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= CACHE_MAX {
        return;
    }
    files.sort();
    let extra = files.len() - CACHE_MAX;
    for (_, f) in files.into_iter().take(extra) {
        let _ = std::fs::remove_file(f);
    }
}

// ------------------------------------------------------------------ synthesis

/// Text -> mp3 bytes. Long text is sent in chunks of at most 4096 bytes (one connection each).
pub async fn synthesize(text: &str, voice: &str, rate: &str) -> Result<Vec<u8>, String> {
    let voice = full_voice_name(voice).ok_or_else(|| format!("Invalid Edge voice \"{voice}\""))?;
    let escaped = xml_escape(&remove_incompatible_characters(text));
    let mut mp3 = Vec::new();
    for chunk in split_text_by_byte_length(escaped.as_bytes(), MAX_CHUNK_BYTES) {
        let ssml = mkssml(&voice, rate, &String::from_utf8_lossy(&chunk));
        let audio = match stream_once(&ssml).await {
            // 403: usually the PC clock is off -> learn the skew from the server's Date header and retry once
            Err(Failure::Forbidden(Some(server_time))) => {
                CLOCK_SKEW.store(server_time - now_unix(), Ordering::Relaxed);
                stream_once(&ssml).await
            }
            r => r,
        }
        .map_err(|f| f.explain())?;
        mp3.extend_from_slice(&audio);
    }
    if mp3.is_empty() {
        return Err("Edge TTS returned no audio.".into());
    }
    Ok(mp3)
}

enum Failure {
    Forbidden(Option<i64>), // server Date (unix seconds) if present
    Network(String),
    Protocol(String),
}

impl Failure {
    fn explain(self) -> String {
        match self {
            Failure::Forbidden(_) => "Edge TTS was rejected by Microsoft (403). Common causes: the PC clock is off, a VPN/proxy, \
                 or Microsoft changed its API (then the app needs an update). Use the Windows voice meanwhile."
                .into(),
            Failure::Network(e) => format!("Edge TTS needs Internet for text that is not cached yet — cannot reach Microsoft ({e})."),
            Failure::Protocol(e) => format!("Edge TTS error: {e}"),
        }
    }
}

async fn stream_once(ssml: &str) -> Result<Vec<u8>, Failure> {
    let url = format!(
        "wss://{BASE_URL}/edge/v1?TrustedClientToken={TRUSTED_CLIENT_TOKEN}&ConnectionId={}&Sec-MS-GEC={}&Sec-MS-GEC-Version=2-{CHROMIUM_FULL_VERSION}",
        connect_id(),
        sec_ms_gec(now_unix() + CLOCK_SKEW.load(Ordering::Relaxed)),
    );
    let mut req = url.into_client_request().map_err(|e| Failure::Protocol(e.to_string()))?;
    let ua = format!(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{v}.0.0.0 Safari/537.36 Edg/{v}.0.0.0",
        v = CHROMIUM_MAJOR_VERSION
    );
    let muid = format!("muid={};", uuid::Uuid::new_v4().simple().to_string().to_uppercase());
    for (k, v) in [
        ("Pragma", "no-cache"),
        ("Cache-Control", "no-cache"),
        ("Origin", "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold"),
        ("User-Agent", ua.as_str()),
        ("Accept-Encoding", "gzip, deflate, br, zstd"),
        ("Accept-Language", "en-US,en;q=0.9"),
        ("Cookie", muid.as_str()),
    ] {
        req.headers_mut().insert(k, HeaderValue::from_str(v).map_err(|e| Failure::Protocol(e.to_string()))?);
    }

    let connect = tokio_tungstenite::connect_async(req);
    let (mut ws, _) = match tokio::time::timeout(Duration::from_secs(10), connect).await {
        Err(_) => return Err(Failure::Network("connection timed out".into())),
        Ok(Err(WsError::Http(resp))) if resp.status() == 403 => {
            let date = resp.headers().get("Date").and_then(|d| d.to_str().ok()).and_then(parse_http_date);
            return Err(Failure::Forbidden(date));
        }
        Ok(Err(WsError::Http(resp))) => return Err(Failure::Protocol(format!("HTTP {}", resp.status()))),
        Ok(Err(e)) => return Err(Failure::Network(e.to_string())),
        Ok(Ok(x)) => x,
    };

    let ts = js_date_string(now_unix());
    let config = format!(
        "X-Timestamp:{ts}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n\
         {{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\
         \"sentenceBoundaryEnabled\":\"true\",\"wordBoundaryEnabled\":\"false\"}},\
         \"outputFormat\":\"audio-24khz-48kbitrate-mono-mp3\"}}}}}}}}\r\n"
    );
    let ssml_msg = format!(
        "X-RequestId:{}\r\nContent-Type:application/ssml+xml\r\nX-Timestamp:{ts}Z\r\nPath:ssml\r\n\r\n{ssml}",
        connect_id()
    );
    let net = |e: WsError| Failure::Network(e.to_string());
    ws.send(Message::text(config)).await.map_err(net)?;
    ws.send(Message::text(ssml_msg)).await.map_err(net)?;

    let mut audio = Vec::new();
    loop {
        let msg = match tokio::time::timeout(Duration::from_secs(60), ws.next()).await {
            Err(_) => return Err(Failure::Network("no answer for 60 s".into())),
            Ok(None) => break,
            Ok(Some(Err(e))) => return Err(net(e)),
            Ok(Some(Ok(m))) => m,
        };
        match msg {
            Message::Text(t) => {
                let head = t.as_str().split("\r\n\r\n").next().unwrap_or("");
                if head.lines().any(|l| l.trim() == "Path:turn.end") {
                    break;
                }
            }
            Message::Binary(b) => {
                if b.len() < 2 {
                    return Err(Failure::Protocol("binary message without header length".into()));
                }
                let hl = u16::from_be_bytes([b[0], b[1]]) as usize;
                if hl + 2 > b.len() {
                    return Err(Failure::Protocol("header length larger than the message".into()));
                }
                let head = String::from_utf8_lossy(&b[2..2 + hl]);
                if !head.split("\r\n").any(|l| l == "Path:audio") {
                    return Err(Failure::Protocol("binary message that is not audio".into()));
                }
                audio.extend_from_slice(&b[2 + hl..]);
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    let _ = ws.close(None).await;
    if audio.is_empty() {
        return Err(Failure::Protocol("no audio was received (check the voice name)".into()));
    }
    Ok(audio)
}

// ------------------------------------------------------------------ text helpers

/// "zh-CN-XiaoxiaoNeural" -> "Microsoft Server Speech Text to Speech Voice (zh-CN, XiaoxiaoNeural)".
fn full_voice_name(v: &str) -> Option<String> {
    if v.starts_with("Microsoft Server Speech Text to Speech Voice (") && v.ends_with(')') && v.contains(',') {
        return Some(v.to_string());
    }
    let mut parts = v.splitn(3, '-');
    let (lang, region, name) = (parts.next()?, parts.next()?, parts.next()?);
    let ok = lang.len() >= 2
        && lang.chars().all(|c| c.is_ascii_lowercase())
        && region.len() >= 2
        && region.chars().all(|c| c.is_ascii_uppercase())
        && name.len() > "Neural".len()
        && name.ends_with("Neural")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !ok {
        return None;
    }
    let (region, name) = match name.split_once('-') {
        Some((extra, rest)) => (format!("{region}-{extra}"), rest.to_string()),
        None => (region.to_string(), name.to_string()),
    };
    Some(format!("Microsoft Server Speech Text to Speech Voice ({lang}-{region}, {name})"))
}

/// Control characters the service rejects (e.g. vertical tab from OCR'd PDFs) -> space.
fn remove_incompatible_characters(s: &str) -> String {
    s.chars()
        .map(|c| match c as u32 {
            0..=8 | 11..=12 | 14..=31 => ' ',
            _ => c,
        })
        .collect()
}

/// xml.sax.saxutils.escape
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn mkssml(voice: &str, rate: &str, escaped_text: &str) -> String {
    format!(
        "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='en-US'>\
         <voice name='{voice}'><prosody pitch='+0Hz' rate='{rate}' volume='+0%'>{escaped_text}</prosody></voice></speak>"
    )
}

/// Split into chunks of at most `limit` bytes: at the last newline/space, else at a UTF-8 boundary,
/// never inside an XML entity such as `&amp;`.
fn split_text_by_byte_length(mut text: &[u8], limit: usize) -> Vec<Vec<u8>> {
    let trim = |b: &[u8]| -> Vec<u8> { String::from_utf8_lossy(b).trim().as_bytes().to_vec() };
    let mut out = Vec::new();
    while text.len() > limit {
        let mut split = text[..limit].iter().rposition(|&c| c == b'\n');
        if split.is_none() {
            split = text[..limit].iter().rposition(|&c| c == b' ');
        }
        let mut split = split.unwrap_or_else(|| {
            let mut s = limit;
            while s > 0 && std::str::from_utf8(&text[..s]).is_err() {
                s -= 1;
            }
            s
        });
        // do not cut an entity: move back to its '&' when no ';' follows it before the split
        while split > 0 {
            match text[..split].iter().rposition(|&c| c == b'&') {
                Some(amp) if !text[amp..split].contains(&b';') => split = amp,
                _ => break,
            }
        }
        let chunk = trim(&text[..split]);
        if !chunk.is_empty() {
            out.push(chunk);
        }
        text = &text[split.max(1)..];
    }
    let rest = trim(text);
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

fn connect_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Days since 1970-01-01 -> (year, month, day). (Howard Hinnant's civil_from_days)
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]; // 1970-01-01 was a Thursday
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// "Wed Nov 12 2026 03:40:00 GMT+0000 (Coordinated Universal Time)"
fn js_date_string(t: i64) -> String {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{} {} {:02} {} {:02}:{:02}:{:02} GMT+0000 (Coordinated Universal Time)",
        WEEKDAYS[days.rem_euclid(7) as usize],
        MONTHS[m as usize - 1],
        d,
        y,
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// RFC 7231 date ("Wed, 12 Nov 2026 03:40:00 GMT") -> unix seconds.
fn parse_http_date(s: &str) -> Option<i64> {
    let p: Vec<&str> = s.split_whitespace().collect();
    if p.len() < 5 {
        return None;
    }
    let d: u32 = p[1].parse().ok()?;
    let m = MONTHS.iter().position(|x| *x == p[2])? as u32 + 1;
    let y: i64 = p[3].parse().ok()?;
    let hms: Vec<i64> = p[4].split(':').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    if hms.len() != 3 {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400 + hms[0] * 3600 + hms[1] * 60 + hms[2])
}

// ------------------------------------------------------------------ Sec-MS-GEC v2 token (edge_tts/drm.py)

const WIN_EPOCH: i64 = 11_644_473_600;

const SHA256_INITIAL: [u32; 8] = [0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A, 0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19];
const SHA256_STATE: [u32; 8] = [0xFD9FBEA2, 0x91243CE7, 0xBE499094, 0xCD2E1E0A, 0x6C006F49, 0x04BD35AB, 0xFEB0A7E4, 0x03EFFBD2];
const SHA256_SCHEDULE: [u32; 14] = [
    0xEB24BF9E, 0x1558691F, 0xA0C09EE2, 0x0CFA32BA, 0xA622EE8C, 0xD1DD9491, 0xC0DD25A0, 0xF9E70AB6, 0xF0C31573, 0x7058CE4E,
    0x7188E606, 0x00E5AB33, 0xD258B060, 0x72766564,
];
const K: [u32; 64] = [
    0x428A2F98, 0x71374491, 0xB5C0FBCF, 0xE9B5DBA5, 0x3956C25B, 0x59F111F1, 0x923F82A4, 0xAB1C5ED5, 0xD807AA98, 0x12835B01,
    0x243185BE, 0x550C7DC3, 0x72BE5D74, 0x80DEB1FE, 0x9BDC06A7, 0xC19BF174, 0xE49B69C1, 0xEFBE4786, 0x0FC19DC6, 0x240CA1CC,
    0x2DE92C6F, 0x4A7484AA, 0x5CB0A9DC, 0x76F988DA, 0x983E5152, 0xA831C66D, 0xB00327C8, 0xBF597FC7, 0xC6E00BF3, 0xD5A79147,
    0x06CA6351, 0x14292967, 0x27B70A85, 0x2E1B2138, 0x4D2C6DFC, 0x53380D13, 0x650A7354, 0x766A0ABB, 0x81C2C92E, 0x92722C85,
    0xA2BFE8A1, 0xA81A664B, 0xC24B8B70, 0xC76C51A3, 0xD192E819, 0xD6990624, 0xF40E3585, 0x106AA070, 0x19A4C116, 0x1E376C08,
    0x2748774C, 0x34B0BCB5, 0x391C0CB3, 0x4ED8AA4A, 0x5B9CCA4F, 0x682E6FF3, 0x748F82EE, 0x78A5636F, 0x84C87814, 0x8CC70208,
    0x90BEFFFA, 0xA4506CEB, 0xBEF9A3F7, 0xC67178F2,
];
const SHA256_ADJUSTMENT: u32 = 0xC9DC06A7;
const SIGMA0_ADJUSTMENT: u32 = 0x059C0B80;
const SIGMA1_ADJUSTMENT: u32 = 0x000B92C0;
const CS64_COEFFICIENTS: [(u32, u32); 14] = [
    (0xE82E91B1, 0xB32700D0),
    (0x44DE24C6, 0x88DFF51D),
    (0x3BEB65BC, 0x29514038),
    (0xC89C5933, 0xB328CBDF),
    (0x8D1EC211, 0xD405B4D5),
    (0x314EF5D5, 0x26E55F3C),
    (0xE92491B1, 0x607801C6),
    (0xF0A5B9D5, 0x82B2C1FB),
    (0x06064CF3, 0xFA51A3DA),
    (0xDA79FA60, 0x207CC464),
    (0xBED20560, 0x3C3C74F3),
    (0x89F9C9F8, 0xD2D14B61),
    (0x35FA38C6, 0xBB04A2C1),
    (0xCB49E3C6, 0xC5626167),
];
const CS64_FACTOR: u32 = 0x82893933;
const CS64_ADJUSTMENT0: u32 = 0xF2F90B1A;
const CS64_ADJUSTMENT1: u32 = 0x0B1A0000;

fn small_sigma0(x: u32) -> u32 {
    x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3)
}
fn small_sigma1(x: u32) -> u32 {
    x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10)
}

fn expand_schedule(w: &mut Vec<u32>, first: usize) {
    for i in first..64 {
        let v = small_sigma1(w[i - 2]).wrapping_add(w[i - 7]).wrapping_add(small_sigma0(w[i - 15])).wrapping_add(w[i - 16]);
        w.push(v);
    }
}

/// SHA-256 compression rounds `first..64` (with the given round constants).
fn sha_rounds(state: [u32; 8], w: &[u32], first: usize, k: &[u32; 64]) -> [u32; 8] {
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
    for i in first..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = g ^ (e & (f ^ g));
        let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(k[i]).wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) | (c & (a | b));
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    [a, b, c, d, e, f, g, h]
}

fn add_state(x: [u32; 8], y: [u32; 8]) -> [u32; 8] {
    std::array::from_fn(|i| x[i].wrapping_add(y[i]))
}

/// State after the first 7 bytes (_sha256_init).
fn sha_init(data7: &[u8]) -> [u32; 8] {
    let mut w = vec![0u32; 14];
    w.push(u32::from_be_bytes([0, data7[0], data7[1], data7[2]]));
    w.push(u32::from_be_bytes([data7[3], data7[4], data7[5], data7[6]]));
    for i in 16..30 {
        let mut v = SHA256_SCHEDULE[i - 16];
        if i - 2 >= 14 {
            let s = small_sigma1(w[i - 2]);
            v = v.wrapping_add(if i == 16 { s ^ SIGMA1_ADJUSTMENT } else { s });
        }
        if i - 7 >= 14 {
            v = v.wrapping_add(w[i - 7]);
        }
        if i - 15 >= 14 {
            let s = small_sigma0(w[i - 15]);
            v = v.wrapping_add(if i == 29 { s ^ SIGMA0_ADJUSTMENT } else { s });
        }
        w.push(v);
    }
    w.push(
        small_sigma1(w[28])
            .wrapping_add(w[23])
            .wrapping_add(small_sigma0(w[15]))
            .wrapping_add(w[14])
            .wrapping_add(SHA256_ADJUSTMENT)
            .wrapping_sub(K[14]),
    );
    expand_schedule(&mut w, 31);
    let mut kb = K;
    kb[14] = SHA256_ADJUSTMENT;
    add_state(SHA256_INITIAL, sha_rounds(SHA256_STATE, &w, 14, &kb))
}

fn sha_digest(data: &[u8]) -> [u8; 32] {
    let length = 57 + data.len();
    let mut padded = data[7..].to_vec();
    padded.push(0x80);
    let zeros = (55i64 - length as i64).rem_euclid(64) as usize;
    padded.extend(std::iter::repeat_n(0u8, zeros));
    padded.extend_from_slice(&((length * 8) as u64).to_be_bytes());
    let mut state = sha_init(&data[..7]);
    for block in padded.chunks(64) {
        let mut w: Vec<u32> = block.chunks(4).map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]])).collect();
        expand_schedule(&mut w, 16);
        state = add_state(state, sha_rounds(state, &w, 0, &K));
    }
    let mut out = [0u8; 32];
    for (i, s) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&s.to_be_bytes());
    }
    out
}

fn rol16(v: u32) -> u32 {
    v.rotate_left(16)
}

fn cs64_digest(data: &[u8], digest: &[u8; 32]) -> (u32, u32) {
    let key_a = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]) | 1;
    let key_b = u32::from_le_bytes([digest[4], digest[5], digest[6], digest[7]]) | 1;
    let factors = [key_a.wrapping_mul(CS64_FACTOR), key_b.wrapping_mul(CS64_FACTOR)];
    let mut terms: Vec<u32> = CS64_COEFFICIENTS.iter().enumerate().map(|(i, (c, _))| c.wrapping_mul(factors[i % 2])).collect();
    let mut offsets: Vec<u32> = CS64_COEFFICIENTS.iter().map(|(_, o)| *o).collect();
    // little-endian words of b"\0" + data, zero-padded to an even number of words
    let mut bytes = vec![0u8];
    bytes.extend_from_slice(data);
    while bytes.len() % 4 != 0 {
        bytes.push(0);
    }
    let mut words: Vec<u32> = bytes.chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    if words.len() % 2 == 1 {
        words.push(0);
    }
    for (i, &word) in words.iter().enumerate() {
        let mut term = word.wrapping_mul(if i % 2 == 0 { key_a } else { key_b });
        let mut offset = rol16(word).wrapping_mul(if i % 2 == 0 { 0xEF0569FB } else { 0xC31713DB });
        if i == 0 {
            term = term.wrapping_add(CS64_ADJUSTMENT0.wrapping_mul(factors[0]));
            offset = offset.wrapping_add(CS64_ADJUSTMENT1);
        }
        terms.push(term);
        offsets.push(offset);
    }
    let (mut swap_acc, mut swap_last, mut state, mut acc) = (0u32, 0u32, 0u32, 0u32);
    for i in (0..terms.len()).step_by(2) {
        let (x, y) = (terms[i], terms[i + 1]);
        let t = rol16(offsets[i].wrapping_add(x));
        let q = t.wrapping_mul(0x76F50F34);
        swap_acc = swap_acc.wrapping_add(rol16(q).wrapping_mul(0xC3EFEA97).wrapping_add(q));
        let t = rol16(offsets[i + 1].wrapping_add(y));
        let q = t.wrapping_mul(0x1881CE3C);
        swap_last = rol16(q).wrapping_mul(0x35BD1EC9).wrapping_add(q);
        swap_acc = swap_acc.wrapping_add(swap_last);

        state = state.wrapping_mul(key_a).wrapping_add(x);
        state = rol16(state).wrapping_mul(0xCF98B111);
        state = rol16(state).wrapping_mul(0x87085B9F);
        state = rol16(state).wrapping_mul(0x12CEB96D);
        state = rol16(state).wrapping_mul(0x257E1D83);
        acc = acc.wrapping_add(state);
        let mut t = state.wrapping_mul(key_b).wrapping_add(y);
        t = rol16(t).wrapping_mul(0xA27416F5);
        t = rol16(t).wrapping_mul(0xD38396FF);
        state = rol16(rol16(t).wrapping_mul(0x7C932B89)).wrapping_mul(0xBFA49F69);
        acc = acc.wrapping_add(state);
    }
    (swap_last ^ state, swap_acc ^ acc)
}

fn hex_upper(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

/// Sec-MS-GEC v2: "<CS64 16 hex>-<SHA256 64 hex>" of the 5-minute Windows file-time tick + client token.
fn sec_ms_gec(unix_secs: i64) -> String {
    let mut secs = unix_secs + WIN_EPOCH;
    secs -= secs.rem_euclid(300);
    let data = format!("{}{TRUSTED_CLIENT_TOKEN}", secs as i128 * 10_000_000);
    let digest = sha_digest(data.as_bytes());
    let (c0, c1) = cs64_digest(data.as_bytes(), &digest);
    let mut cs = c0.to_le_bytes().to_vec();
    cs.extend_from_slice(&c1.to_le_bytes());
    format!("{}-{}", hex_upper(&cs), hex_upper(&digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_matches_edge_tts() {
        // vectors from edge_tts 7.3.1 generate_sec_ms_gec_v2()
        assert_eq!(sec_ms_gec(0), "52BC7A17B50DFEC9-28C4394775C8751CD2F6859C030C06DB1489ACAB3B3A7651831FC26895177ED6");
        assert_eq!(sec_ms_gec(1_700_000_000), "2CD23A2CA0B2595A-F3C846C370E05F9CC83ACC5F995C5F5D87A4D0F14CE8936A0F902E64099E38A2");
        assert_eq!(sec_ms_gec(1_762_900_000), "F3570521C09CF8CD-00ED28C93A3AA48AD4DC1BF966684E46D09DD429F19209B62C64A30C98D6A3AB");
        assert_eq!(sec_ms_gec(1_794_000_123), "6EA6AF6A0550F520-17E5A544A1EFE811EA5248AFBD9780637964201A6D3A15ADE53A30AB9D066A4F");
    }

    #[test]
    fn voice_names() {
        assert_eq!(full_voice_name("zh-CN-XiaoxiaoNeural").unwrap(), "Microsoft Server Speech Text to Speech Voice (zh-CN, XiaoxiaoNeural)");
        assert_eq!(
            full_voice_name("zh-CN-liaoning-XiaobeiNeural").unwrap(),
            "Microsoft Server Speech Text to Speech Voice (zh-CN-liaoning, XiaobeiNeural)"
        );
        assert!(full_voice_name("zh-CN-Xiao'x Neural").is_none());
    }

    #[test]
    fn dates() {
        assert_eq!(js_date_string(0), "Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal Time)");
        assert_eq!(js_date_string(1_794_000_123), "Fri Nov 06 2026 21:22:03 GMT+0000 (Coordinated Universal Time)");
        assert_eq!(parse_http_date("Fri, 06 Nov 2026 21:22:03 GMT"), Some(1_794_000_123));
    }

    #[test]
    fn split() {
        let t = "a".repeat(10) + " &amp; " + &"b".repeat(10);
        for c in split_text_by_byte_length(t.as_bytes(), 14) {
            assert!(c.len() <= 14);
        }
        assert_eq!(split_text_by_byte_length("你好世界".as_bytes(), 7), vec!["你好".as_bytes().to_vec(), "世界".as_bytes().to_vec()]);
    }
}
