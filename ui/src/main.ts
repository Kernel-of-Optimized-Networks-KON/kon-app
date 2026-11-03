import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { $, bytes, duration, el } from "./util";
import { addViewerPaths, initViewer } from "./viewer";

type Status = "queued" | "processing" | "verifying" | "complete" | "failed" | "cancelled" | "skipped";
type StageName = "compressing" | "verifying" | "writing";

interface FileEntry { id: number; path: string; name: string; format: string; size: number; engine: string | null }
interface Item extends FileEntry {
  status: Status;
  stage?: StageName;
  output?: string;
  outBytes?: number;
  error?: string;
}
interface ImageMeta { width: number; height: number; bitDepth: number; layout: string; hasIcc: boolean; iccBytes: number; notes: string[] }
interface Inspection { size: number; image: ImageMeta | null; error: string | null }
type JobEvent =
  | { kind: "started"; id: number }
  | { kind: "stage"; id: number; stage: StageName }
  | { kind: "done"; id: number; output: string; inBytes: number; outBytes: number }
  | { kind: "failed"; id: number; error: string }
  | { kind: "cancelled"; id: number };
interface Progress { finished: number; total: number; bytesPerSec: number | null; etaSecs: number | null }

const items = new Map<number, Item>();
const knownPaths = new Set<string>();
let selected: number | null = null;
let dest: string | null = null;
let running = false;
let batchTotal = 0;
let progress: Progress | null = null;

// ---------- queue ----------
const STATUS_LABEL: Record<Status, string> = {
  queued: "Queued", processing: "Processing", verifying: "Verifying",
  complete: "Complete", failed: "Failed", cancelled: "Cancelled", skipped: "Unsupported",
};

async function addPaths(paths: string[]) {
  if (running || paths.length === 0) return;
  const entries = await invoke<FileEntry[]>("add_paths", { paths });
  for (const e of entries) {
    if (knownPaths.has(e.path)) continue;
    knownPaths.add(e.path);
    items.set(e.id, { ...e, status: e.engine ? "queued" : "skipped" });
  }
  renderAll();
}

function renderRows() {
  const body = $<HTMLTableSectionElement>("rows");
  body.replaceChildren();
  for (const it of items.values()) {
    const tr = el("tr", it.id === selected ? "sel" : "");
    tr.addEventListener("click", () => select(it.id));
    const name = el("td", "", it.name);
    name.title = it.path;
    tr.append(name, el("td", "", it.format), el("td", "", bytes(it.size)), el("td", "", it.engine ?? "—"));

    const st = el("td");
    st.append(el("span", `badge ${it.status}`, STATUS_LABEL[it.status]));
    if (it.status === "processing" || it.status === "verifying") {
      // The engine exposes stages, not intra-file progress, so show stage steps rather than a fake percentage.
      const steps: StageName[] = ["compressing", "verifying", "writing"];
      const at = steps.indexOf(it.stage ?? "compressing");
      const mini = el("div", "mini");
      steps.forEach((_, i) => mini.append(el("i", i < at ? "done" : i === at ? "on" : "")));
      st.append(mini);
    }
    tr.append(st);

    const res = el("td", "num");
    if (it.status === "complete" && it.outBytes !== undefined) {
      res.append(el("span", "ratio", `${(it.size / it.outBytes).toFixed(2)}×`), document.createTextNode(`  ${bytes(it.outBytes)}`));
    } else if (it.status === "failed" && it.error) {
      res.textContent = "see inspector";
    }
    tr.append(res);
    body.append(tr);
  }
  $("empty").hidden = items.size > 0;
}

function select(id: number) {
  selected = id;
  renderRows();
  void renderInspector();
}

// ---------- inspector ----------
function metaRow(name: string, tag: "available" | "preserved" | "transformed" | "unsupported", detail?: string) {
  const row = el("div", "meta-row");
  const left = el("div", "name", name);
  if (detail) left.append(el("span", "detail", detail));
  row.append(left, el("span", `tag ${tag}`, tag));
  return row;
}

async function renderInspector() {
  const body = $("inspector-body");
  body.className = "";
  body.replaceChildren();
  const it = selected === null ? undefined : items.get(selected);
  if (!it) { body.className = "muted"; body.textContent = "Select a file to see its details."; return; }

  const kv = el("div", "kv");
  const add = (k: string, v: string, mono = false) => kv.append(el("div", "k", k), el("div", "v" + (mono ? " mono" : ""), v));
  add("Name", it.name); add("Format", it.format); add("Size", bytes(it.size), true); add("Engine", it.engine ?? "None");
  body.append(kv);

  if (it.status === "complete" && it.outBytes !== undefined) {
    const c = el("div", "callout");
    c.append(el("b", "", "✓ Verified lossless"),
      el("div", "", `Decoded output matches the original pixel-for-pixel before it was written.`),
      el("div", "", `${bytes(it.size)} → ${bytes(it.outBytes)} (${(it.size / it.outBytes).toFixed(2)}×, ${(100 - (100 * it.outBytes) / it.size).toFixed(1)}% smaller)`),
      el("div", "detail", it.output ?? ""));
    body.append(c);
  } else if (it.status === "failed") {
    const c = el("div", "callout err");
    c.append(el("b", "", "Failed"), el("div", "", it.error ?? "Unknown error"));
    body.append(c);
  }

  body.append(el("h3", "", "Image metadata"));
  const wanted = it.id;
  const loading = el("div", "muted", "Reading…");
  body.append(loading);
  let info: Inspection;
  try { info = await invoke<Inspection>("inspect_file", { path: it.path }); }
  catch (e) { if (selected === wanted) loading.textContent = String(e); return; }
  if (selected !== wanted) return; // selection changed while reading
  loading.remove();

  if (!info.image) {
    body.append(el("div", "muted", info.error ?? "Metadata unavailable."));
    return;
  }
  const m = info.image;
  const eng = it.engine ?? "engine";
  body.append(
    metaRow("Dimensions", "preserved", `${m.width} × ${m.height} px`),
    metaRow("Bit depth", "preserved", `${m.bitDepth}-bit per channel`),
    metaRow("Channels", "preserved", m.layout),
    metaRow("Pixel data", "preserved", `Round-trip verified by ${eng} before writing`),
    metaRow("ICC colour profile", m.hasIcc ? "available" : "unsupported",
      m.hasIcc ? `Present in source (${bytes(m.iccBytes)}) — not stored in the compressed output yet`
               : "None embedded in source"),
  );
  for (const n of m.notes) body.append(metaRow("Representation change", "transformed", n));
  body.append(
    metaRow("Camera / EXIF", "unsupported", "Not read or preserved by the current backend"),
    metaRow("Text & other chunks", "unsupported", "Not read or preserved by the current backend"),
  );
}

// ---------- destination & run controls ----------
function renderControls() {
  const path = $("dest-path");
  path.textContent = dest ?? "Not selected — choose where compressed files will be saved";
  path.className = dest ? "path set" : "path muted";
  path.title = dest ?? "";
  const runnable = [...items.values()].filter((i) => i.engine && (i.status === "queued" || i.status === "failed" || i.status === "cancelled"));
  ($("start") as HTMLButtonElement).disabled = running || !dest || runnable.length === 0;
  $("start").hidden = running;
  $("cancel").hidden = !running;
  for (const id of ["add-files", "add-folder", "clear", "pick-dest"]) ($(id) as HTMLButtonElement).disabled = running;
  renderOverall();
}

function renderOverall() {
  const fill = $("overall-bar");
  const label = $("overall-label");
  const stats = $("overall-stats");
  const summary = $("summary");
  if (batchTotal === 0) {
    label.textContent = "Idle"; stats.textContent = ""; fill.style.width = "0"; fill.className = "fill";
    summary.textContent = "";
    return;
  }
  const finished = progress?.finished ?? 0;
  fill.style.width = `${(100 * finished) / batchTotal}%`;
  label.textContent = running ? `Processing ${finished} / ${batchTotal}` : `Finished ${finished} / ${batchTotal}`;
  const parts: string[] = [];
  if (running && progress?.bytesPerSec) parts.push(`${bytes(progress.bytesPerSec)}/s`);
  if (running && progress?.etaSecs != null) parts.push(`ETA ${duration(progress.etaSecs)}`);
  stats.textContent = parts.join(" · ");

  if (!running) {
    const done = [...items.values()].filter((i) => i.status === "complete" && i.outBytes !== undefined);
    const failed = [...items.values()].filter((i) => i.status === "failed").length;
    fill.className = failed > 0 && done.length === 0 ? "fill failed" : "fill done";
    const inB = done.reduce((s, i) => s + i.size, 0);
    const outB = done.reduce((s, i) => s + (i.outBytes ?? 0), 0);
    summary.replaceChildren();
    if (done.length) {
      summary.append(el("b", "", `✓ ${done.length} verified lossless`),
        document.createTextNode(` · ${bytes(inB)} → ${bytes(outB)} (${(inB / outB).toFixed(2)}×)`));
    }
    if (failed) {
      if (done.length) summary.append(document.createTextNode(" · "));
      summary.append(el("span", "bad", `${failed} failed`));
    }
  } else {
    fill.className = "fill";
    summary.textContent = "";
  }
}

function renderAll() { renderRows(); renderControls(); }

// ---------- events from Rust ----------
void listen<JobEvent>("job-event", ({ payload: ev }) => {
  const it = items.get(ev.id);
  if (!it) return;
  switch (ev.kind) {
    case "started": it.status = "processing"; it.stage = "compressing"; break;
    case "stage":
      it.stage = ev.stage;
      it.status = ev.stage === "verifying" ? "verifying" : "processing";
      break;
    case "done": it.status = "complete"; it.output = ev.output; it.outBytes = ev.outBytes; break;
    case "failed": it.status = "failed"; it.error = ev.error; break;
    case "cancelled": it.status = "cancelled"; break;
  }
  renderRows();
  if (selected === ev.id && (ev.kind === "done" || ev.kind === "failed")) void renderInspector();
});
void listen<Progress>("batch-progress", ({ payload }) => { progress = payload; renderOverall(); });
void listen("batch-finished", () => { running = false; renderControls(); });

// ---------- actions ----------
async function pick(directory: boolean) {
  const r = await open({ multiple: !directory, directory });
  if (!r) return;
  await addPaths(Array.isArray(r) ? r : [r]);
}
$("add-files").addEventListener("click", () => void pick(false));
$("add-folder").addEventListener("click", () => void pick(true));
$("clear").addEventListener("click", () => {
  items.clear(); knownPaths.clear(); selected = null; batchTotal = 0; progress = null;
  renderAll(); void renderInspector();
});
$("pick-dest").addEventListener("click", async () => {
  const r = await open({ directory: true, multiple: false });
  if (typeof r === "string") { dest = r; renderControls(); }
});
$("start").addEventListener("click", async () => {
  if (!dest) return;
  const todo = [...items.values()].filter((i) => i.engine && (i.status === "queued" || i.status === "failed" || i.status === "cancelled"));
  for (const i of todo) { i.status = "queued"; i.error = undefined; }
  batchTotal = todo.length; progress = { finished: 0, total: todo.length, bytesPerSec: null, etaSecs: null };
  running = true;
  renderAll();
  try {
    await invoke("start_batch", { items: todo.map((i) => ({ id: i.id, path: i.path })), dest });
  } catch (e) {
    running = false; batchTotal = 0;
    renderAll();
    $("summary").textContent = String(e);
  }
});
$("cancel").addEventListener("click", () => void invoke("cancel_batch"));

// Drag & drop: Tauri delivers real filesystem paths (folders included) rather than browser File objects.
const zone = $("dropzone");
const vzone = $("viewer-drop");
void getCurrentWebview().onDragDropEvent((e) => {
  const t = e.payload.type;
  const z = activeTab === "viewer" ? vzone : zone;
  if (t === "enter" || t === "over") z.classList.add("active");
  else if (t === "leave") { zone.classList.remove("active"); vzone.classList.remove("active"); }
  else if (t === "drop") {
    zone.classList.remove("active"); vzone.classList.remove("active");
    // Route by the visible workspace; each keeps its own state.
    if (activeTab === "viewer") void addViewerPaths(e.payload.paths);
    else void addPaths(e.payload.paths);
  }
});

renderAll();

// ---------- workspaces ----------
// Both views stay mounted (just hidden), so switching tabs never discards queue or viewer state.
type Tab = "compress" | "viewer";
let activeTab: Tab = "compress";
function showTab(t: Tab) {
  activeTab = t;
  for (const name of ["compress", "viewer"] as Tab[]) {
    $("view-" + name).hidden = name !== t;
    $("actions-" + name).hidden = name !== t;
    $("tab-" + name).classList.toggle("active", name === t);
    $("tab-" + name).setAttribute("aria-selected", String(name === t));
  }
}
$("tab-compress").addEventListener("click", () => showTab("compress"));
$("tab-viewer").addEventListener("click", () => showTab("viewer"));
initViewer();
showTab("compress");
