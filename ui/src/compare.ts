import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { $, el, sourceHints } from "./util";

interface CompareInfo { width: number; height: number; identical: boolean; verdict: string; rendering: string | null; ms: number }
type Mode = "slider" | "side";

let info: CompareInfo | null = null;
let urls: { original: string; compressed: string } | null = null;
let mode: Mode = "slider";
let zoomMode: "fit" | number = "fit";
let s = 1, tx = 0, ty = 0;      // shared view transform (image px -> pane px): both images always use the same one
let split = 0.5;                // slider position, 0..1 of the pane width
let curPkon = "";
let curName = "";
let token = 0;

const stage = () => $("c-stage");

function revoke() {
  if (urls) { URL.revokeObjectURL(urls.original); URL.revokeObjectURL(urls.compressed); urls = null; }
}

async function fetchPng(side: "original" | "compressed") {
  const raw = await invoke<ArrayBuffer | number[]>("compare_image", { side });
  const buf = raw instanceof ArrayBuffer ? raw : Uint8Array.from(raw);
  return URL.createObjectURL(new Blob([buf], { type: "image/png" }));
}

function message(text: string, err = false) {
  stage().replaceChildren(el("div", "v-state " + (err ? "err" : "muted"), text));
}

/** Show the compare view in place of the Viewer. */
function showView() {
  for (const id of ["view-viewer", "actions-viewer", "view-compress", "actions-compress"]) $(id).hidden = true;
  $("view-compare").hidden = false;
}
function backToViewer() {
  closeCompare();
  $("view-compare").hidden = true;
  $("view-viewer").hidden = false; $("actions-viewer").hidden = false;
}

async function pickOriginal(): Promise<string | null> {
  const r = await open({
    multiple: false,
    title: "Select the original image for this .pkon",
    filters: [{ name: "Original image", extensions: ["png", "jpg", "jpeg", "dng"] }],
  });
  return typeof r === "string" ? r : null;
}

/** Compare a `.pkon` selected in the Viewer with its original: the known association, or one the user picks. */
export async function startCompare(pkon: string, name: string, forcePick = false) {
  let source = forcePick ? null : sourceHints.get(pkon) ?? null;
  if (!source) source = await pickOriginal();
  if (!source) return; // cancelled: stay where we are
  curPkon = pkon; curName = name;
  showView();
  const ok = await loadCompare(source, pkon, `${source.split(/[\\/]/).pop()} ↔ ${name}`);
  if (ok) sourceHints.set(pkon, source); // remembered for this session (the .pkon itself stores no source)
}

async function loadCompare(source: string, pkon: string, name: string): Promise<boolean> {
  const my = ++token;
  revoke(); info = null;
  $("c-name").textContent = name;
  $("c-name").title = `Original: ${source} | Decoded: ${pkon}`;
  $("c-verdict").textContent = "";
  $("c-verdict").className = "c-verdict";
  $("c-render").textContent = "";
  message("Loading the original and decoding the .pkon… RAW images take several seconds.");
  try {
    const i = await invoke<CompareInfo>("compare_open", { source, pkon });
    if (my !== token) return false;
    const [original, compressed] = await Promise.all([fetchPng("original"), fetchPng("compressed")]);
    if (my !== token) { URL.revokeObjectURL(original); URL.revokeObjectURL(compressed); return false; }
    info = i; urls = { original, compressed };
    zoomMode = "fit"; split = 0.5; manual = false;
    $("c-verdict").textContent = (i.identical ? "✓ " : "✗ ") + i.verdict;
    $("c-verdict").className = "c-verdict " + (i.identical ? "ok" : "bad");
    $("c-render").textContent = i.rendering ? `Rendering (identical settings on both sides): ${i.rendering}` : "";
    build();
    return true;
  } catch (e) {
    if (my !== token) return false;
    message(`Comparison failed: ${String(e)} — use “Change original…” to select a different file.`, true);
    return false;
  }
}

function imgEl(src: string, alt: string) {
  const im = el("img") as HTMLImageElement;
  im.src = src; im.alt = alt; im.draggable = false;
  im.style.cssText = "position:absolute;left:0;top:0;transform-origin:0 0;max-width:none;";
  return im;
}

let panes: HTMLElement[] = [];
let imgs: HTMLImageElement[] = [];
let clipLayer: HTMLElement | null = null;
let divider: HTMLElement | null = null;

function build() {
  if (!info || !urls) return;
  const st = stage();
  st.replaceChildren();
  panes = []; imgs = []; clipLayer = null; divider = null;
  const chip = (t: string, right = false) => el("div", "c-chip" + (right ? " r" : ""), t);
  if (mode === "slider") {
    const pane = el("div", "c-pane");
    const under = imgEl(urls.compressed, "Decoded PKON");
    const over = imgEl(urls.original, "Original");
    clipLayer = el("div", "c-clip");
    clipLayer.append(over);
    divider = el("div", "c-divider");
    divider.append(el("div", "c-grip", "◂ ▸"));
    pane.append(under, clipLayer, divider, chip("Original"), chip("Decoded PKON", true));
    panes = [pane]; imgs = [under, over];
    st.append(pane);
    divider.addEventListener("pointerdown", (ev) => {
      ev.stopPropagation();
      divider!.setPointerCapture(ev.pointerId);
      const move = (m: PointerEvent) => {
        const r = pane.getBoundingClientRect();
        split = Math.min(1, Math.max(0, (m.clientX - r.left) / r.width));
        applySplit();
      };
      const up = () => { divider!.removeEventListener("pointermove", move); divider!.removeEventListener("pointerup", up); };
      divider!.addEventListener("pointermove", move);
      divider!.addEventListener("pointerup", up);
    });
  } else {
    const a = el("div", "c-pane"), b = el("div", "c-pane");
    const ia = imgEl(urls.original, "Original"), ib = imgEl(urls.compressed, "Decoded PKON");
    a.append(ia, chip("Original")); b.append(ib, chip("Decoded PKON"));
    panes = [a, b]; imgs = [ia, ib];
    st.append(a, b);
  }
  for (const p of panes) attachNav(p);
  layout();
}

function paneSize() {
  const r = panes[0].getBoundingClientRect();
  return { pw: r.width, ph: r.height };
}
function fitScale() {
  const { pw, ph } = paneSize();
  return Math.min(pw / info!.width, ph / info!.height);
}
function clampPan() {
  // Keep the image from being dragged completely out of view.
  const { pw, ph } = paneSize();
  const w = info!.width * s, h = info!.height * s;
  tx = w <= pw ? (pw - w) / 2 : Math.min(0, Math.max(pw - w, tx));
  ty = h <= ph ? (ph - h) / 2 : Math.min(0, Math.max(ph - h, ty));
}
function layout() {
  if (!info || panes.length === 0) return;
  if (zoomMode === "fit") { s = fitScale(); } else if (typeof zoomMode === "number") { s = zoomMode; }
  if (!manual) {
    const { pw, ph } = paneSize();
    tx = (pw - info.width * s) / 2; ty = (ph - info.height * s) / 2;
  }
  apply();
}
function apply() {
  clampPan();
  for (const im of imgs) {
    im.style.width = `${info!.width}px`; im.style.height = `${info!.height}px`;
    im.style.transform = `translate(${tx}px, ${ty}px) scale(${s})`;
    im.style.imageRendering = s >= 3 ? "pixelated" : "auto";
  }
  applySplit();
  const pct = Math.round(s * 100);
  $("c-zoomlabel").textContent = `${pct}%`;
}
function applySplit() {
  if (!clipLayer || !divider) return;
  clipLayer.style.clipPath = `inset(0 ${(1 - split) * 100}% 0 0)`;
  divider.style.left = `${split * 100}%`;
}

function attachNav(pane: HTMLElement) {
  pane.addEventListener("wheel", (ev) => {
    ev.preventDefault();
    const r = pane.getBoundingClientRect();
    const cx = ev.clientX - r.left, cy = ev.clientY - r.top;
    const next = Math.min(32, Math.max(fitScale() * 0.5, s * (ev.deltaY < 0 ? 1.18 : 1 / 1.18)));
    tx = cx - ((cx - tx) * next) / s; ty = cy - ((cy - ty) * next) / s;
    s = next; zoomMode = s as number; manual = true;
    apply();
  }, { passive: false });
  pane.addEventListener("pointerdown", (ev) => {
    if ((ev.target as HTMLElement).closest(".c-divider")) return;
    pane.setPointerCapture(ev.pointerId);
    let lx = ev.clientX, ly = ev.clientY;
    pane.classList.add("grab");
    const move = (m: PointerEvent) => { tx += m.clientX - lx; ty += m.clientY - ly; lx = m.clientX; ly = m.clientY; manual = true; apply(); };
    const up = () => { pane.classList.remove("grab"); pane.removeEventListener("pointermove", move); pane.removeEventListener("pointerup", up); };
    pane.addEventListener("pointermove", move);
    pane.addEventListener("pointerup", up);
  });
  pane.addEventListener("dblclick", () => { zoomMode = "fit"; manual = false; layout(); });
}
let manual = false;

export function initCompare() {
  $("c-back").addEventListener("click", backToViewer);
  $("c-change").addEventListener("click", () => { if (curPkon) void startCompare(curPkon, curName, true); });
  $("c-mode").addEventListener("click", (ev) => {
    const b = (ev.target as HTMLElement).closest("button");
    if (!b || !info) return;
    mode = b.dataset.mode as Mode;
    $("c-mode").querySelectorAll("button").forEach((x) => x.classList.toggle("on", x === b));
    build();
  });
  $("c-zoom").addEventListener("click", (ev) => {
    const b = (ev.target as HTMLElement).closest("button");
    if (!b || !info) return;
    const z = b.dataset.zoom!;
    zoomMode = z === "fit" ? "fit" : Number(z); manual = false;
    layout();
  });
  window.addEventListener("resize", () => { if (!manual) layout(); else apply(); });
}

/** Release the staged images when leaving the compare view. */
export function closeCompare() { token++; revoke(); info = null; stage().replaceChildren(); }
