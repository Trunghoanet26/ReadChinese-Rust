// Screen-selection overlay. The screenshot is fetched as raw RGBA pixels from the in-memory
// `shot` protocol (no JPEG encode, no temp file) and painted on two canvases.
const bg = document.getElementById('bg');
const lit = document.getElementById('lit');
const sel = document.getElementById('sel');
const size = document.getElementById('size');
const hint = document.getElementById('hint');
let start = null, rect = null, active = false, pending = null, raf = 0;

function reset() {
  active = false; start = null; rect = null; pending = null;
  sel.style.display = 'none'; size.style.display = 'none'; hint.style.display = '';
  lit.style.clipPath = 'inset(0 100% 100% 0)';
  document.body.classList.remove('plain');
}

function clearCanvases() {
  for (const c of [bg, lit]) { c.width = 1; c.height = 1; } // frees the pixel memory
}

window.api.onCaptureImage(async ({ url, width, height }) => {
  reset();
  try {
    const buf = await (await fetch(url, { cache: 'no-store' })).arrayBuffer();
    bg.width = lit.width = width;
    bg.height = lit.height = height;
    bg.getContext('2d').putImageData(new ImageData(new Uint8ClampedArray(buf), width, height), 0, 0);
    lit.getContext('2d').drawImage(bg, 0, 0);
  } catch (err) {
    console.error('capture image', err);
    window.api.captureDone(null);
    return;
  }
  // wait two frames so the image is surely painted before the window is shown
  requestAnimationFrame(() => requestAnimationFrame(() => { active = true; window.api.captureReady(); }));
});
window.api.onCaptureReset(() => { reset(); clearCanvases(); });

const finish = (r) => {
  if (!active) return;
  active = false;
  cancelAnimationFrame(raf);
  // Show only the plain screenshot (no mask, no frame): it looks exactly like the real desktop, so nothing
  // visibly changes in the moment before the window is hidden. (Hiding the content instead would flash
  // the black window background for a frame.)
  document.body.classList.add('plain');
  // CSS px -> physical pixels of the screenshot
  const k = window.devicePixelRatio || 1;
  window.api.captureDone(r && { x: r.x * k, y: r.y * k, width: r.width * k, height: r.height * k });
};
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') finish(null); });
document.addEventListener('contextmenu', (e) => { e.preventDefault(); finish(null); });

document.addEventListener('mousedown', (e) => {
  if (e.button !== 0 || !active) return;
  start = { x: e.clientX, y: e.clientY };
  hint.style.display = 'none';
});
function draw() {
  raf = 0;
  if (!pending) return;
  const { x, y, w, h } = pending;
  lit.style.clipPath = `inset(${y}px ${innerWidth - x - w}px ${innerHeight - y - h}px ${x}px)`;
  sel.style.transform = `translate(${x - 1.5}px, ${y - 1.5}px)`;
  sel.style.width = `${w + 3}px`;
  sel.style.height = `${h + 3}px`;
  sel.style.display = 'block';
  size.style.transform = `translate(${x}px, ${Math.max(0, y - 22)}px)`;
  size.style.display = 'block';
  size.textContent = `${w} × ${h}`;
}
// coalesce mouse moves per animation frame -> smooth, no jank
document.addEventListener('mousemove', (e) => {
  if (!start) return;
  const x = Math.min(start.x, e.clientX), y = Math.min(start.y, e.clientY);
  const w = Math.abs(e.clientX - start.x), h = Math.abs(e.clientY - start.y);
  rect = { x, y, width: w, height: h };
  pending = { x, y, w, h };
  if (!raf) raf = requestAnimationFrame(draw);
});
document.addEventListener('mouseup', (e) => {
  if (e.button !== 0 || !start) return;
  start = null;
  finish(rect);
});
