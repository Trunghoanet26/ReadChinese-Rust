// Same `window.api` surface as the Electron preload.js, implemented on Tauri's invoke/listen,
// so renderer.js and capture.js work almost unchanged.
(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const on = (channel) => (cb) => { listen(channel, (e) => cb(e.payload)); };
  const fire = (cmd, args) => { invoke(cmd, args).catch((e) => console.error(cmd, e)); };

  window.api = {
    // config & data
    getConfig: () => invoke('get_config'),
    setConfig: (patch) => invoke('set_config', { patch }),
    getLangs: () => invoke('get_langs'),
    aiPresets: () => invoke('ai_presets'),
    aiNewId: () => invoke('ai_new_id'),
    aiTest: (provider) => invoke('ai_test', { provider }),
    captureBackend: () => invoke('capture_backend'),

    // actions
    translate: (text) => fire('translate', { text }),
    startCapture: () => fire('start_capture'),
    hide: () => fire('hide'),
    pin: (v) => fire('pin', { v: !!v }),
    fit: (h) => fire('fit', { h }),
    copy: (text) => fire('copy', { text: String(text ?? '') }),
    ttsEdge: ({ text, voice, rate }) => invoke('tts_edge', { text, voice, rate }),

    // events from the Rust side
    onOcrStart: on('ocr:start'),
    onOcrResult: on('ocr:result'),
    onOcrError: on('ocr:error'),
    onOcrStatus: on('ocr:status'),
    onInputSet: on('input:set'),
    onTrStart: on('tr:start'),
    onTrDelta: on('tr:delta'),
    onTrDone: on('tr:done'),

    // capture overlay window
    onCaptureImage: on('capture:image'),
    captureDone: (rect) => invoke('capture_done', { rect }),
    captureReady: () => fire('capture_ready'),
    onCaptureReset: on('capture:reset'),
  };
})();
