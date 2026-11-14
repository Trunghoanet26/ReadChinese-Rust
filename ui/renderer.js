/* global pinyinPro */
const $ = (id) => document.getElementById(id);
const els = {
  app: $('app'), ruby: $('ruby'), input: $('input'), status: $('status'), results: $('results'),
  tone: $('btnTone'), pin: $('btnPin'), main: $('viewMain'), settings: $('viewSettings'),
};

let cfg = {};
let LANGS = {};
let PRESETS = {};
let text = '';
let items = [];     // pinyin-pro result, one entry per character
let overrides = {}; // item index -> reading chosen by the user (polyphonic characters)
let currentJob = 0;

const escapeHtml = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
const langName = (c) => LANGS[c] || c;

// ------------------------------------------------------------ auto-fit window height
const fitObserver = new ResizeObserver(() => {
  window.api.fit(Math.ceil(els.app.getBoundingClientRect().height) + 2); // +2 = body border
});
fitObserver.observe(els.app);

// ------------------------------------------------------------ pinyin
function toneOpt() { return { toneType: cfg.toneType === 'num' ? 'num' : 'symbol' }; }

function computePinyin() {
  if (typeof pinyinPro === 'undefined') {
    showStatus('pinyin-pro failed to load (ui/vendor/pinyin-pro.js missing).', true);
    items = [];
    return;
  }
  // analyse the whole sentence at once so pinyin-pro picks readings from context (银行 / 行不行)
  items = text ? pinyinPro.pinyin(text, { type: 'all', ...toneOpt() }) : [];
}

function renderRuby() {
  els.ruby.innerHTML = items.map((it, i) => {
    if (!it.isZh) return escapeHtml(it.origin).replace(/\n/g, '<br>');
    const py = overrides[i] ?? it.pinyin;
    const poly = it.polyphonic?.length > 1 ? 'poly' : '';
    return `<ruby data-i="${i}" class="${poly}">${escapeHtml(it.origin)}<rt>${escapeHtml(py)}</rt></ruby>`;
  }).join('');
}

function pinyinLine() {
  return items.map((it, i) => (it.isZh ? (overrides[i] ?? it.pinyin) + ' ' : it.origin))
    .join('').replace(/ +([，。！？、；：,.!?;:）)」』])/g, '$1').replace(/ +\n/g, '\n').trim();
}

function setText(t, { keepOverrides = false } = {}) {
  if (t !== text && tts.speaking) stopSpeaking();
  text = t;
  els.input.value = t;
  if (!keepOverrides) overrides = {};
  computePinyin();
  renderRuby();
}

// click a polyphonic character -> cycle through its readings
els.ruby.addEventListener('click', (e) => {
  const r = e.target.closest('ruby');
  if (!r) return;
  const i = Number(r.dataset.i);
  const it = items[i];
  const all = it.polyphonic || [];
  if (all.length < 2) return;
  const cur = overrides[i] ?? it.pinyin;
  overrides[i] = all[(all.indexOf(cur) + 1) % all.length];
  renderRuby();
});

// ------------------------------------------------------------ UI helpers
function showStatus(msg, isErr = false) {
  els.status.hidden = !msg;
  els.status.textContent = msg || '';
  els.status.classList.toggle('err', isErr);
}
function setEditing(on) {
  els.input.hidden = !on;
  els.ruby.hidden = on;
  $('btnEdit').classList.toggle('on', on);
  if (on) els.input.focus(); else setText(els.input.value.trim());
}
function translate() {
  if (!els.input.hidden) setEditing(false);
  if (text) window.api.translate(text);
}
function applyFont() { document.documentElement.style.setProperty('--fs', `${cfg.fontSize}px`); }
function applyTone() { els.tone.textContent = cfg.toneType === 'num' ? 'ni3 hao3' : 'nǐ hǎo'; }
function applyPin() {
  els.pin.classList.toggle('on', !!cfg.pinned);
  els.pin.title = cfg.pinned ? 'Pinned – click to unpin' : 'Pin (keep the window open when clicking elsewhere)';
}

// ------------------------------------------------------------ toolbar
$('btnClose').onclick = () => window.api.hide();
$('btnOcr').onclick = () => window.api.startCapture();
$('btnEdit').onclick = () => setEditing(els.input.hidden);
$('btnTranslate').onclick = translate;
$('btnCopyText').onclick = () => window.api.copy(text);
$('btnCopyPinyin').onclick = () => window.api.copy(pinyinLine());
els.pin.onclick = () => { cfg.pinned = !cfg.pinned; applyPin(); window.api.pin(cfg.pinned); };
els.tone.onclick = async () => {
  cfg = await window.api.setConfig({ toneType: cfg.toneType === 'num' ? 'symbol' : 'num' });
  applyTone();
  setText(text);
};
const changeFont = async (d) => {
  cfg = await window.api.setConfig({ fontSize: Math.max(14, Math.min(48, cfg.fontSize + d)) });
  applyFont();
};
$('btnSmaller').onclick = () => changeFont(-2);
$('btnBigger').onclick = () => changeFont(2);

els.input.addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); translate(); }
});
let typingTimer; // live pinyin while editing
els.input.addEventListener('input', () => {
  clearTimeout(typingTimer);
  typingTimer = setTimeout(() => { text = els.input.value.trim(); overrides = {}; computePinyin(); renderRuby(); }, 250);
});
document.addEventListener('keydown', (e) => {
  if (e.key !== 'Escape') return;
  if (!els.settings.hidden) closeSettings();
  else if (tts.speaking) stopSpeaking();
  else window.api.hide();
});

// ------------------------------------------------------------ translation result cards
function buildCards(cards) {
  els.results.innerHTML = '';
  for (const c of cards) {
    const card = document.createElement('section');
    card.className = 'card result';
    card.dataset.key = c.key;
    const letter = c.kind === 'google' ? 'G' : (c.name || 'A').trim().charAt(0).toUpperCase();
    card.innerHTML = `
      <div class="res-head">
        <span class="logo ${c.kind === 'google' ? 'google' : ''}">${escapeHtml(letter)}</span>
        <span>${escapeHtml(c.name)} · ${escapeHtml(langName(c.lang))}</span>
        ${c.model ? `<span class="model">${escapeHtml(c.model)}</span>` : ''}
        <button class="icon copy" title="Copy">⧉</button>
      </div>
      <div class="out loading"></div>`;
    card.querySelector('.copy').onclick = () => window.api.copy(card.querySelector('.out').textContent);
    els.results.appendChild(card);
  }
}
const outOf = (key) => els.results.querySelector(`.result[data-key="${CSS.escape(key)}"] .out`);

window.api.onTrStart(({ id, cards }) => { currentJob = id; buildCards(cards); });
window.api.onTrDelta(({ id, key, delta }) => {
  if (id !== currentJob) return;
  const out = outOf(key);
  if (!out) return;
  out.classList.add('streaming');
  out.textContent += delta;
});
window.api.onTrDone(({ id, key, text: t, error }) => {
  if (id !== currentJob) return;
  const out = outOf(key);
  if (!out) return;
  out.classList.remove('loading', 'streaming');
  if (error) { out.classList.add('err'); out.textContent = error; } else if (t != null) out.textContent = t;
});

// ------------------------------------------------------------ read aloud (TTS)
const EDGE_VOICES = {
  'zh-CN-XiaoxiaoNeural': 'Xiaoxiao – female, Mandarin (recommended)',
  'zh-CN-XiaoyiNeural': 'Xiaoyi – female, Mandarin',
  'zh-CN-YunxiNeural': 'Yunxi – male, Mandarin',
  'zh-CN-YunjianNeural': 'Yunjian – male, Mandarin',
  'zh-CN-YunyangNeural': 'Yunyang – male, news style',
  'zh-CN-YunxiaNeural': 'Yunxia – young male',
  'zh-CN-liaoning-XiaobeiNeural': 'Xiaobei – female, Northeastern accent',
  'zh-CN-shaanxi-XiaoniNeural': 'Xiaoni – female, Shaanxi accent',
  'zh-TW-HsiaoChenNeural': 'HsiaoChen – female, Taiwan',
  'zh-TW-YunJheNeural': 'YunJhe – male, Taiwan',
  'zh-HK-HiuMaanNeural': 'HiuMaan – female, Cantonese (HK)',
};
const tts = { speaking: false, audio: null, token: 0 };
const speakBtn = $('btnSpeak');

const zhVoices = () => speechSynthesis.getVoices().filter((v) => /^zh|^cmn|^yue/i.test(v.lang));
const clearReading = () => els.ruby.querySelectorAll('ruby.reading').forEach((r) => r.classList.remove('reading'));

function setSpeaking(on) {
  tts.speaking = on;
  speakBtn.classList.toggle('on', on);
  speakBtn.title = on ? 'Stop' : 'Read aloud (select part of the text to read only that part)';
  if (!on) clearReading();
}

function stopSpeaking() {
  tts.token++;
  speechSynthesis.cancel();
  if (tts.audio) { tts.audio.pause(); tts.audio = null; }
  setSpeaking(false);
}

// Chinese characters selected with the mouse (if any), plus the index of the first one
function selectedPart() {
  const sel = window.getSelection();
  if (!sel || sel.isCollapsed || !els.ruby.contains(sel.anchorNode)) return null;
  const range = sel.getRangeAt(0);
  const idx = [...els.ruby.querySelectorAll('ruby')].filter((r) => range.intersectsNode(r)).map((r) => Number(r.dataset.i));
  if (!idx.length) return null;
  const from = idx[0], to = idx[idx.length - 1];
  return { text: items.slice(from, to + 1).map((it) => it.origin).join(''), startItem: from };
}

// character offset in the spoken string -> highlight the matching characters
function highlightRange(startItem, charIndex, charLength) {
  clearReading();
  let pos = 0;
  const end = charIndex + Math.max(1, charLength);
  for (let i = startItem; i < items.length; i++) {
    const len = items[i].origin.length;
    if (pos + len > charIndex && pos < end) els.ruby.querySelector(`ruby[data-i="${i}"]`)?.classList.add('reading');
    pos += len;
    if (pos >= end) break;
  }
}

async function speak(textToRead, startItem = 0, engine = cfg.ttsEngine) {
  stopSpeaking();
  const token = tts.token;
  if (!textToRead.trim()) return;
  setSpeaking(true);
  const voices = zhVoices();

  if (engine !== 'edge' && voices.length) {
    const u = new SpeechSynthesisUtterance(textToRead);
    u.voice = voices.find((v) => v.name === cfg.ttsWinVoice) || voices.find((v) => /zh-CN/i.test(v.lang)) || voices[0];
    u.lang = u.voice.lang;
    u.rate = Number(cfg.ttsRate) || 1;
    u.onboundary = (e) => { if (token === tts.token) highlightRange(startItem, e.charIndex, e.charLength || 1); };
    u.onend = u.onerror = () => { if (token === tts.token) setSpeaking(false); };
    speechSynthesis.speak(u);
    return;
  }

  if (engine !== 'edge') showStatus('No Chinese Windows voice installed → using Edge TTS (see Settings).');
  const r = await window.api.ttsEdge({ text: textToRead, voice: cfg.ttsEdgeVoice, rate: cfg.ttsRate });
  if (token !== tts.token) return; // stopped or replaced meanwhile
  if (!r.ok) {
    setSpeaking(false);
    if (voices.length) { // Edge failed but a Windows voice exists -> use it
      showStatus(`${r.error} → Using the Windows voice instead.`, true);
      speak(textToRead, startItem, 'windows');
    } else showStatus(r.error, true);
    return;
  }
  tts.audio = new Audio(r.url);
  tts.audio.onended = tts.audio.onerror = () => { if (token === tts.token) { tts.audio = null; setSpeaking(false); } };
  tts.audio.play().catch(() => setSpeaking(false));
}

speakBtn.onclick = () => {
  if (tts.speaking) { stopSpeaking(); return; }
  const part = selectedPart();
  if (part) speak(part.text, part.startItem); else speak(text, 0);
};

// ------------------------------------------------------------ settings
let draftProviders = [];

function fillLangSelect(sel, value) {
  sel.innerHTML = Object.keys(LANGS).map((c) => `<option value="${c}">${escapeHtml(LANGS[c])}</option>`).join('');
  sel.value = value;
}

function fillWinVoices() {
  const voices = zhVoices();
  const sel = $('sWinVoice');
  sel.innerHTML = voices.map((v) => `<option value="${escapeHtml(v.name)}">${escapeHtml(v.name)} (${escapeHtml(v.lang)})</option>`).join('');
  if (cfg.ttsWinVoice) sel.value = cfg.ttsWinVoice;
  $('winVoiceHint').textContent = voices.length ? ''
    : 'No Chinese voice found. Windows Settings → Time & language → Language & region → add "中文(中华人民共和国)" '
      + 'with "Text-to-speech", then restart the app.';
}
speechSynthesis.onvoiceschanged = () => { if (!els.settings.hidden) fillWinVoices(); };

function toggleSections() {
  $('googleBody').classList.toggle('off', !$('sGoogleOn').checked);
  $('aiBody').classList.toggle('off', !$('sAiOn').checked);
  const edge = $('sTtsEngine').value === 'edge';
  $('rowEdgeVoice').hidden = !edge;
  $('rowWinVoice').hidden = edge;
}

function renderProviders() {
  const box = $('providers');
  box.innerHTML = '';
  draftProviders.forEach((p) => {
    const el = $('tplProvider').content.firstElementChild.cloneNode(true);
    const q = (s) => el.querySelector(s);
    const dl = document.createElement('datalist');
    dl.id = `dl-${p.id}`;
    el.appendChild(dl);
    q('.p-model').setAttribute('list', dl.id);

    q('.p-on').checked = !!p.enabled;
    q('.p-name').value = p.name || '';
    q('.p-type').value = p.type || 'openai';
    q('.p-base').value = p.baseUrl || '';
    q('.p-key').value = p.apiKey || '';
    q('.p-model').value = p.model || '';
    el.classList.toggle('disabled', !p.enabled);

    q('.p-on').onchange = (e) => { p.enabled = e.target.checked; el.classList.toggle('disabled', !p.enabled); };
    q('.p-name').oninput = (e) => { p.name = e.target.value; };
    q('.p-base').oninput = (e) => { p.baseUrl = e.target.value.trim(); };
    q('.p-key').oninput = (e) => { p.apiKey = e.target.value.trim(); };
    q('.p-model').oninput = (e) => { p.model = e.target.value.trim(); };
    q('.p-type').onchange = (e) => {
      p.type = e.target.value;
      // switch Base URL to the matching default when it is empty or still a default
      const defaults = Object.values(PRESETS).map((x) => x.baseUrl);
      if (!p.baseUrl || defaults.includes(p.baseUrl)) {
        p.baseUrl = p.type === 'anthropic' ? PRESETS.anthropic.baseUrl : PRESETS.openai.baseUrl;
        q('.p-base').value = p.baseUrl;
      }
    };
    q('.p-del').onclick = () => { draftProviders = draftProviders.filter((x) => x !== p); renderProviders(); };
    q('.p-test').onclick = async () => {
      const msg = q('.p-msg');
      msg.classList.remove('ok');
      msg.textContent = 'Testing…';
      const r = await window.api.aiTest({ ...p });
      if (!r.ok) { msg.textContent = r.error; return; }
      dl.innerHTML = r.models.map((m) => `<option value="${escapeHtml(m)}">`).join('');
      if (!p.model && r.models.length) { p.model = r.models[0]; q('.p-model').value = p.model; }
      msg.classList.add('ok');
      msg.textContent = r.models.length
        ? `Connected. ${r.models.length} models available – click the Model box to pick one.`
        : 'Connected (the server did not return a model list – type the model name).';
    };
    box.appendChild(el);
  });
}

function openSettings() {
  fillLangSelect($('sGoogleTarget'), cfg.googleTarget || 'en');
  fillLangSelect($('sAiTarget'), cfg.aiTarget || 'vi');
  $('sGoogleOn').checked = cfg.googleEnabled !== false;
  $('sAiOn').checked = cfg.aiEnabled !== false;
  $('sGoogleKey').value = cfg.googleApiKey || '';
  draftProviders = (cfg.aiProviders || []).map((p) => ({ ...p }));
  renderProviders();
  $('sAddPreset').innerHTML = Object.entries(PRESETS).map(([k, v]) => `<option value="${k}">${escapeHtml(v.name)}</option>`).join('');

  $('sTtsEngine').value = cfg.ttsEngine || 'windows';
  fillWinVoices();
  $('sEdgeVoice').innerHTML = Object.entries(EDGE_VOICES).map(([k, v]) => `<option value="${k}">${escapeHtml(v)}</option>`).join('');
  $('sEdgeVoice').value = cfg.ttsEdgeVoice || 'zh-CN-XiaoxiaoNeural';
  $('sRate').value = cfg.ttsRate ?? 1;
  $('rateVal').textContent = `${Number($('sRate').value).toFixed(1)}×`;
  $('sAutoSpeak').checked = !!cfg.autoSpeak;

  $('sHkOcr').value = cfg.hotkeyOcr || '';
  $('sHkClip').value = cfg.hotkeyClipboard || '';
  $('sHkShow').value = cfg.hotkeyShow || '';
  $('sMerge').checked = !!cfg.mergeLines;
  $('sThreads').value = cfg.ocrThreads || 2;
  window.api.captureBackend().then((b) => { $('backendInfo').textContent = `Screen capture: ${b}`; });

  toggleSections();
  $('sError').textContent = '';
  els.main.hidden = true;
  els.settings.hidden = false;
  document.scrollingElement.scrollTop = 0;
}
function closeSettings() {
  els.settings.hidden = true;
  els.main.hidden = false;
}

$('btnSettings').onclick = () => (els.settings.hidden ? openSettings() : closeSettings());
$('sGoogleOn').onchange = toggleSections;
$('sAiOn').onchange = toggleSections;
$('sTtsEngine').onchange = toggleSections;
$('sRate').oninput = () => { $('rateVal').textContent = `${Number($('sRate').value).toFixed(1)}×`; };
$('btnAddProvider').onclick = async () => {
  const preset = PRESETS[$('sAddPreset').value];
  draftProviders.push({ id: await window.api.aiNewId(), ...preset, apiKey: '', enabled: true });
  renderProviders();
  $('providers').lastElementChild?.scrollIntoView({ block: 'nearest' });
};
$('sCancel').onclick = closeSettings;
$('sSave').onclick = async () => {
  const patch = {
    googleEnabled: $('sGoogleOn').checked,
    googleTarget: $('sGoogleTarget').value,
    googleApiKey: $('sGoogleKey').value.trim(),
    aiEnabled: $('sAiOn').checked,
    aiTarget: $('sAiTarget').value,
    aiProviders: draftProviders.map((p) => ({ ...p, name: (p.name || '').trim() || 'AI' })),
    ttsEngine: $('sTtsEngine').value,
    ttsWinVoice: $('sWinVoice').value || cfg.ttsWinVoice || '',
    ttsEdgeVoice: $('sEdgeVoice').value,
    ttsRate: Number($('sRate').value),
    autoSpeak: $('sAutoSpeak').checked,
    hotkeyOcr: $('sHkOcr').value.trim(),
    hotkeyClipboard: $('sHkClip').value.trim(),
    hotkeyShow: $('sHkShow').value.trim(),
    mergeLines: $('sMerge').checked,
    ocrThreads: Math.max(1, Math.min(16, parseInt($('sThreads').value, 10) || 2)),
  };
  const retranslate = ['googleEnabled', 'googleTarget', 'aiEnabled', 'aiTarget']
    .some((k) => patch[k] !== cfg[k]) || JSON.stringify(patch.aiProviders) !== JSON.stringify(cfg.aiProviders);
  try {
    cfg = await window.api.setConfig(patch);
  } catch (err) { // rejected by the Rust-side validation, or saved but an API key could not be stored
    $('sError').textContent = String(err);
    cfg = await window.api.getConfig(); // stay in sync with what Rust actually holds
    return;
  }
  closeSettings();
  if (retranslate && text) window.api.translate(text);
};

// ------------------------------------------------------------ events from the main process
window.api.onOcrStart(() => {
  showStatus('Recognizing…');
  setEditing(false);
  setText('');
  els.results.innerHTML = '';
});
window.api.onOcrResult(({ text: t, autoSpeak }) => {
  showStatus(t ? '' : 'No text found in the selected area.', !t);
  setText(t);
  if (t && autoSpeak) speak(t, 0);
});
window.api.onOcrError((msg) => showStatus(`OCR error: ${msg}`, true));
window.api.onOcrStatus((msg) => showStatus(msg));
window.api.onInputSet((t) => { showStatus(''); setText(t); });

// ------------------------------------------------------------ init
(async () => {
  [cfg, LANGS, PRESETS] = await Promise.all([window.api.getConfig(), window.api.getLangs(), window.api.aiPresets()]);
  applyFont(); applyTone(); applyPin();
})();
