import { invoke } from "@tauri-apps/api/core";
import { ask, open, save } from "@tauri-apps/plugin-dialog";
import { $, bytes, el, sourceHints } from "./util";
import { startCompare } from "./compare";

interface ContainerInfo {
  containerVersion: number; codecId: number; codecName: string;
  width: number; height: number; bitDepth: number; channels: number;
  layout: string; representation: string | null; profile: string | null;
  metaVersion: number; metaLen: number; payloadLen: number;
  headerCrc32: number; pixelCrc32: number; rawPixelBytes: number; previewable: boolean;
}
interface PkonEntry { path: string; name: string; size: number; info: ContainerInfo | null; error: string | null }
type Verify = "pending" | "ok" | "failed" | "n/a";
interface ExportError { kind: "exists" | "other"; message: string }

const entries = new Map<string, PkonEntry & { verify: Verify; verifyError?: string }>();
let selected: string | null = null;
let currentUrl: string | null = null;
let zoom: "fit" | number = "fit";
let decodedOk = false;
let loadToken = 0; // discards results of decodes the user has already moved away from

const hex = (n: number) => "0x" + n.toString(16).toUpperCase().padStart(8, "0");

export async function addViewerPaths(paths: string[]) {
  if (paths.length === 0) return;
  const found = await invoke<PkonEntry[]>("scan_pkon", { paths });
  for (const e of found) {
    if (!entries.has(e.path)) entries.set(e.path, { ...e, verify: e.info?.previewable ? "pending" : "n/a" });
  }
  renderList();
  if (selected === null && entries.size > 0) void select(entries.keys().next().value as string);
}

function renderList() {
  const list = $("v-list");
  list.replaceChildren();
  for (const e of entries.values()) {
    const item = el("div", "v-item" + (e.path === selected ? " sel" : ""));
    item.title = e.path;
    item.append(el("div", "n", e.name));
    const m = el("div", "m");
    m.append(el("span", "", bytes(e.size)));
    if (e.info) {
      m.append(el("span", "", `${e.info.width}×${e.info.height}`), el("span", "", `${e.info.bitDepth}-bit ${e.info.layout}`), el("span", "", e.info.codecName));
    } else {
      m.append(el("span", "bad", "Invalid container"));
    }
    item.append(m);
    item.addEventListener("click", () => void select(e.path));
    list.append(item);
  }
  $("v-empty").hidden = entries.size > 0;
  $("v-count").textContent = entries.size ? `${entries.size} file${entries.size === 1 ? "" : "s"}` : "";
}

function setState(text: string, err = false) {
  const s = $("v-state");
  s.textContent = text;
  s.className = "v-state " + (err ? "err" : "muted");
  s.hidden = false;
  $("v-img").hidden = true;
}

function setStatus(parts: Array<[string, string]>) {
  const st = $("v-status");
  st.replaceChildren();
  parts.forEach(([cls, text], i) => {
    if (i) st.append(document.createTextNode(" · "));
    st.append(el("span", cls, text));
  });
}

function dropImage() {
  const img = $<HTMLImageElement>("v-img");
  img.removeAttribute("src");
  if (currentUrl) { URL.revokeObjectURL(currentUrl); currentUrl = null; }
}

async function select(path: string) {
  selected = path;
  const e = entries.get(path);
  const token = ++loadToken;
  renderList();
  decodedOk = false;
  ($("v-export") as HTMLButtonElement).disabled = true;
  ($("v-compare") as HTMLButtonElement).disabled = true;
  dropImage();
  $("v-dims").textContent = "";
  setStatus([]);
  if (!e) return;
  renderInfo();

  if (!e.info) {
    setState(`Cannot read this container:\n${e.error ?? "unknown error"}`, true);
    setStatus([["bad", "Invalid .pkon header"]]);
    return;
  }
  if (!e.info.previewable) {
    setState(`${e.info.codecName} cannot be previewed.`);
    setStatus([["", "Preview and export unavailable"]]);
    return;
  }

  setState("Decoding…");
  const t0 = performance.now();
  try {
    const raw = await invoke<ArrayBuffer | number[]>("decode_pkon", { path, source: sourceHints.get(path) ?? null });
    const tIpc = performance.now();
    // Raw bytes normally; a JSON number array if the IPC fast path is unavailable.
    const buf = raw instanceof ArrayBuffer ? raw : Uint8Array.from(raw);
    if (token !== loadToken) return;
    const url = URL.createObjectURL(new Blob([buf], { type: "image/png" }));
    const img = $<HTMLImageElement>("v-img");
    img.onload = () => {
      if (token !== loadToken) return;
      applyZoom();
      console.info(`[kon-viewer] backend+IPC ${Math.round(tIpc - t0)} ms, browser PNG decode/render ${Math.round(performance.now() - tIpc)} ms`);
    };
    img.onerror = () => {
      if (token !== loadToken) return;
      e.verify = "failed";
      e.verifyError = "the browser could not display the preview image";
      setState("Preview image could not be displayed by the webview.", true);
      setStatus([["bad", "Render failed"]]);
      renderInfo();
    };
    img.src = url;
    currentUrl = url;
    img.hidden = false;
    $("v-state").hidden = true;
    e.verify = "ok";
    decodedOk = true;
    ($("v-export") as HTMLButtonElement).disabled = false;
    ($("v-compare") as HTMLButtonElement).disabled = false;
    $("v-dims").textContent = `${e.info.width} × ${e.info.height} px`;
    const note = e.info.codecName === "ULISA" || e.info.codecName === "LISA" ? await invoke<string>("render_note").catch(() => "") : "";
    setStatus([["ok", "✓ Decoded and integrity-checked"], ["", `${Math.round(performance.now() - t0)} ms`], ...(note ? [["", note] as [string, string]] : [])]);
  } catch (err) {
    if (token !== loadToken) return;
    e.verify = "failed";
    e.verifyError = String(err);
    setState(`Decoding failed:\n${String(err)}`, true);
    setStatus([["bad", "Decode failed — nothing is shown"]]);
  }
  renderInfo();
}

function applyZoom() {
  const img = $<HTMLImageElement>("v-img");
  $("v-canvas").classList.toggle("fit", zoom === "fit");
  if (zoom === "fit") {
    img.style.width = ""; img.style.height = "";
  } else {
    img.style.width = `${img.naturalWidth * zoom}px`;
    img.style.height = `${img.naturalHeight * zoom}px`;
  }
}

function renderInfo() {
  const box = $("v-info");
  box.replaceChildren();
  box.className = "";
  const e = selected ? entries.get(selected) : undefined;
  if (!e) { box.className = "muted"; box.textContent = "Select a file to inspect its container."; return; }

  const section = (title: string) => box.append(el("h3", "", title));
  let kv: HTMLElement | null = null;
  const start = () => { kv = el("div", "kv"); box.append(kv); };
  const row = (k: string, v: string, mono = false) => {
    kv!.append(el("div", "k", k), el("div", "v" + (mono ? " mono" : ""), v));
  };
  // A field the container does not store is shown as such, never as an empty string.
  const na = (k: string, why: string) => kv!.append(el("div", "k", k), el("div", "v na", `Not available — ${why}`));

  section("File");
  start();
  row("Name", e.name);
  row("Compressed size", bytes(e.size) + ` (${e.size.toLocaleString()} bytes)`);
  na("Original filename", "not stored in .pkon");
  na("Original file size", "not stored in .pkon");

  const i = e.info;
  if (!i) {
    section("Container");
    start();
    row("Status", "Invalid header");
    row("Reason", e.error ?? "unknown");
    return;
  }

  section("Image");
  start();
  row("Dimensions", `${i.width} × ${i.height} px`);
  row("Bit depth", `${i.bitDepth}-bit per channel`);
  row("Layout", `${i.layout} (${i.channels} channel${i.channels === 1 ? "" : "s"})`);
  if (i.representation) row("Colour representation", i.representation);

  section("Encoding");
  start();
  row("Codec", `${i.codecName} (id ${i.codecId})`);
  row("Container version", String(i.containerVersion));
  if (i.profile) row("Model profile", i.profile);
  row("Metadata block", `${i.metaLen} B (codec parameters, version ${i.metaVersion})`);
  row("Payload", bytes(i.payloadLen));

  section("Compression");
  start();
  row("Uncompressed pixels", bytes(i.rawPixelBytes) + " (computed from dimensions)");
  row("Ratio vs. raw pixels", `${(i.rawPixelBytes / e.size).toFixed(2)}×  (${(100 - (100 * e.size) / i.rawPixelBytes).toFixed(1)}% smaller)`);
  na("Ratio vs. original file", "original file size is not stored");

  section("Integrity");
  start();
  row("Header CRC32", `${hex(i.headerCrc32)} — valid`, true);
  row("Pixel CRC32", hex(i.pixelCrc32), true);
  const vtext: Record<Verify, string> = {
    pending: "Not verified yet (decode to check)",
    ok: "✓ Decoded pixels match the stored CRC32",
    failed: `✗ ${e.verifyError ?? "failed"}`,
    "n/a": "Not checked",
  };
  row("Pixel verification", vtext[e.verify]);
  if (e.verify === "ok") kv!.lastElementChild!.classList.add("good");
  else if (e.verify === "failed") kv!.lastElementChild!.classList.add("bad");

  section("Embedded metadata");
  start();
  na("ICC / EXIF / text", "the .pkon container has no field for them");
}

async function doExport() {
  if (!selected || !decodedOk) return;
  const e = entries.get(selected)!;
  const stem = e.name.replace(/\.pkon$/i, "");
  const dest = await save({ defaultPath: `${stem}.png`, filters: [{ name: "PNG image", extensions: ["png"] }] });
  if (!dest) return;
  const btn = $("v-export") as HTMLButtonElement;
  btn.disabled = true;
  setStatus([["", "Exporting…"]]);
  try {
    let overwrite = false;
    for (;;) {
      try {
        const n = await invoke<number>("export_pkon_png", { path: e.path, dest, overwrite, source: sourceHints.get(e.path) ?? null });
        setStatus([["ok", `✓ Exported ${bytes(n)}`], ["", dest]]);
        break;
      } catch (err) {
        const x = err as ExportError;
        if (x && x.kind === "exists" && !overwrite) {
          const yes = await ask(`${dest}\n\nalready exists. Replace it?`, { title: "Replace existing file?", kind: "warning", okLabel: "Replace", cancelLabel: "Keep existing" });
          if (!yes) { setStatus([["", "Export cancelled — existing file untouched"]]); break; }
          overwrite = true;
          continue;
        }
        setStatus([["bad", `Export failed: ${x?.message ?? String(err)}`]]);
        break;
      }
    }
  } finally {
    btn.disabled = !decodedOk;
  }
}

export function initViewer() {
  $("v-open-files").addEventListener("click", async () => {
    const r = await open({ multiple: true, filters: [{ name: "PKON", extensions: ["pkon"] }] });
    if (r) await addViewerPaths(Array.isArray(r) ? r : [r]);
  });
  $("v-open-folder").addEventListener("click", async () => {
    const r = await open({ directory: true, multiple: false });
    if (typeof r === "string") await addViewerPaths([r]);
  });
  $("v-clear").addEventListener("click", () => {
    entries.clear(); selected = null; ++loadToken; decodedOk = false;
    dropImage();
    ($("v-export") as HTMLButtonElement).disabled = true;
    ($("v-compare") as HTMLButtonElement).disabled = true;
    $("v-dims").textContent = ""; setStatus([]);
    setState("Select a file to preview it.");
    renderList(); renderInfo();
  });
  $("v-sample").addEventListener("click", async () => {
    try {
      // The sample original is generated locally next to the sample .pkon, so Compare finds it automatically.
      const png = await invoke<string>("create_sample");
      const pk = await invoke<string>("create_sample_pkon");
      sourceHints.set(pk, png);
      await addViewerPaths([pk]);
    }
    catch (err) { setState(`Could not create the sample: ${String(err)}`, true); }
  });
  $("v-compare").addEventListener("click", () => {
    const e = selected ? entries.get(selected) : undefined;
    if (!e || !decodedOk) return;
    void startCompare(e.path, e.name);
  });
  $("v-export").addEventListener("click", () => void doExport());
  $("v-zoom").addEventListener("click", (ev) => {
    const b = (ev.target as HTMLElement).closest("button");
    if (!b) return;
    const z = b.dataset.zoom!;
    zoom = z === "fit" ? "fit" : Number(z);
    $("v-zoom").querySelectorAll("button").forEach((x) => x.classList.toggle("on", x === b));
    applyZoom();
  });
  renderList();
  renderInfo();
}
