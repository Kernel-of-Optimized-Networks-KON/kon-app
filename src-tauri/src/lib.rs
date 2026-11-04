//! Tauri shell. Owns no compression logic: it translates between the webview and kon-app's `JobRunner`.
//! Commands are `async` so Tauri runs them off the UI thread; each batch runs on its own thread.
use kon_app::{
    engine::{detect, Pkon},
    ingest,
    job::{Event, Job, JobRunner, Stage},
    platform, viewer,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Default)]
struct AppState {
    next_id: AtomicU64,
    /// Cancel flag of the batch currently running, if any.
    active: Mutex<Option<Arc<AtomicBool>>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileEntry {
    id: u64,
    path: String,
    name: String,
    format: String,
    size: u64,
    /// `None` = no engine can take this file yet.
    engine: Option<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Inspection {
    size: u64,
    /// Present only if the engine could decode the file.
    image: Option<ImageMeta>,
    /// Why `image` is absent.
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImageMeta {
    width: u32,
    height: u32,
    bit_depth: u8,
    layout: String,
    has_icc: bool,
    icc_bytes: usize,
    notes: Vec<String>,
}

/// Expand dropped/picked files and folders into queue entries.
#[tauri::command]
async fn add_paths(
    paths: Vec<String>,
    state: State<'_, AppState>,
) -> Result<Vec<FileEntry>, String> {
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    Ok(ingest::scan(&paths)
        .into_iter()
        .map(|p| {
            let detected = detect(&p);
            FileEntry {
                id: state.next_id.fetch_add(1, Ordering::Relaxed),
                name: p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                format: match &detected {
                    Ok(f) => f.name().to_string(),
                    Err(_) => p
                        .extension()
                        .map(|e| e.to_string_lossy().to_uppercase())
                        .unwrap_or_else(|| "—".into()),
                },
                size: std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0),
                engine: detected.as_ref().ok().map(|f| f.engine()),
                path: p.to_string_lossy().into_owned(),
            }
        })
        .collect())
}

#[tauri::command]
async fn inspect_file(path: String) -> Result<Inspection, String> {
    let p = PathBuf::from(path);
    let size = std::fs::metadata(&p).map_err(|e| e.to_string())?.len();
    if !Pkon::accepts(&p) {
        return Ok(Inspection {
            size,
            image: None,
            error: Some(detect(&p).err().unwrap_or_else(|| "Unsupported format.".into())),
        });
    }
    Ok(match Pkon::inspect(&p) {
        Ok(i) => Inspection {
            size,
            image: Some(ImageMeta {
                width: i.width,
                height: i.height,
                bit_depth: i.bit_depth,
                layout: i.layout,
                has_icc: i.has_icc,
                icc_bytes: i.icc_bytes,
                notes: i.notes,
            }),
            error: None,
        },
        Err(e) => Inspection {
            size,
            image: None,
            error: Some(e),
        },
    })
}

#[derive(Deserialize)]
struct BatchItem {
    id: u64,
    path: String,
}

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum JobEvent {
    Started {
        id: u64,
    },
    Stage {
        id: u64,
        stage: &'static str,
    },
    #[serde(rename_all = "camelCase")]
    Done {
        id: u64,
        output: String,
        in_bytes: u64,
        out_bytes: u64,
    },
    Failed {
        id: u64,
        error: String,
    },
    Cancelled {
        id: u64,
    },
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct BatchProgress {
    finished: usize,
    total: usize,
    /// Input bytes completed per second; `None` until at least one file finished.
    bytes_per_sec: Option<f64>,
    eta_secs: Option<f64>,
}

#[tauri::command]
async fn start_batch(
    items: Vec<BatchItem>,
    dest: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let dest = PathBuf::from(dest);
    if !dest.is_dir() {
        return Err(format!("Output folder does not exist: {}", dest.display()));
    }
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut active = state.active.lock().unwrap();
        if active.is_some() {
            return Err("A batch is already running.".into());
        }
        *active = Some(cancel.clone());
    }
    std::thread::spawn(move || run_batch(app, items, dest, cancel));
    Ok(())
}

#[tauri::command]
fn cancel_batch(state: State<'_, AppState>) {
    if let Some(c) = state.active.lock().unwrap().as_ref() {
        c.store(true, Ordering::Relaxed);
    }
}

fn run_batch(app: AppHandle, items: Vec<BatchItem>, dest: PathBuf, cancel: Arc<AtomicBool>) {
    let runner = JobRunner::spawn(Arc::new(Pkon));
    let sizes: HashMap<u64, u64> = items
        .iter()
        .map(|i| {
            (
                i.id,
                std::fs::metadata(&i.path).map(|m| m.len()).unwrap_or(0),
            )
        })
        .collect();
    let total = items.len();
    let total_bytes: u64 = sizes.values().sum();
    for it in items {
        runner.submit(Job {
            id: it.id,
            input: PathBuf::from(it.path),
            dest_dir: dest.clone(),
        });
    }

    let start = Instant::now();
    let (mut finished, mut done_bytes) = (0usize, 0u64);
    let mut cancel_sent = false;
    while finished < total {
        if cancel.load(Ordering::Relaxed) && !cancel_sent {
            runner.cancel_pending();
            cancel_sent = true;
        }
        let ev = match runner.events.recv_timeout(Duration::from_millis(100)) {
            Ok(ev) => ev,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break, // worker gone
        };
        let (out, terminal_id) = match ev {
            Event::Started { id } => (JobEvent::Started { id }, None),
            Event::Stage { id, stage } => (
                JobEvent::Stage {
                    id,
                    stage: match stage {
                        Stage::Compressing => "compressing",
                        Stage::Verifying => "verifying",
                        Stage::Writing => "writing",
                    },
                },
                None,
            ),
            Event::Done {
                id,
                output,
                in_bytes,
                out_bytes,
            } => (
                JobEvent::Done {
                    id,
                    output: output.to_string_lossy().into_owned(),
                    in_bytes,
                    out_bytes,
                },
                Some(id),
            ),
            Event::Failed { id, error } => (JobEvent::Failed { id, error }, Some(id)),
            Event::Cancelled { id } => (JobEvent::Cancelled { id }, Some(id)),
        };
        let _ = app.emit("job-event", out);
        if let Some(id) = terminal_id {
            finished += 1;
            done_bytes += sizes.get(&id).copied().unwrap_or(0);
            let secs = start.elapsed().as_secs_f64();
            let bps = (secs > 0.0 && done_bytes > 0).then(|| done_bytes as f64 / secs);
            let _ = app.emit(
                "batch-progress",
                BatchProgress {
                    finished,
                    total,
                    bytes_per_sec: bps,
                    eta_secs: bps.map(|b| total_bytes.saturating_sub(done_bytes) as f64 / b),
                },
            );
        }
    }
    runner.shutdown();
    if let Some(state) = app.try_state::<AppState>() {
        *state.active.lock().unwrap() = None;
    }
    let _ = app.emit("batch-finished", ());
}

// ------------------------------------------------------------------ PKON Viewer
// Independent of the compression workflow: separate state, no JobRunner involvement.

/// One decoded image kept so inspector/export interactions never decode twice. Keyed by path + size + mtime so an
/// edited file is re-decoded. Single entry: a decoded image can be up to PKON's 256 MiB decode limit.
struct Decoded {
    key: (PathBuf, u64, Option<std::time::SystemTime>, Option<PathBuf>),
    image: Arc<viewer::Canonical>,
    preview_png: Arc<Vec<u8>>,
    note: String,
}

#[derive(Default)]
struct ViewerCache(Mutex<Option<Decoded>>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PkonEntry {
    path: String,
    name: String,
    size: u64,
    /// Header summary if the container header is valid, otherwise why not.
    info: Option<ContainerInfo>,
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ContainerInfo {
    container_version: u8,
    codec_id: u8,
    codec_name: &'static str,
    width: u32,
    height: u32,
    bit_depth: u8,
    channels: u8,
    layout: String,
    representation: Option<String>,
    profile: Option<String>,
    meta_version: u8,
    meta_len: u16,
    payload_len: u32,
    header_crc32: u32,
    pixel_crc32: u32,
    raw_pixel_bytes: u64,
    previewable: bool,
}

impl From<viewer::ContainerInfo> for ContainerInfo {
    fn from(i: viewer::ContainerInfo) -> Self {
        Self {
            container_version: i.container_version,
            codec_id: i.codec_id,
            codec_name: i.codec_name,
            width: i.width,
            height: i.height,
            bit_depth: i.bit_depth,
            channels: i.channels,
            layout: i.layout,
            representation: i.representation,
            profile: i.profile,
            meta_version: i.meta_version,
            meta_len: i.meta_len,
            payload_len: i.payload_len,
            header_crc32: i.header_crc32,
            pixel_crc32: i.pixel_crc32,
            raw_pixel_bytes: i.raw_pixel_bytes,
            previewable: i.previewable,
        }
    }
}

/// Discover `.pkon` files in dropped/picked files and folders and read each header (prefix only; no pixel decode).
#[tauri::command]
async fn scan_pkon(paths: Vec<String>) -> Result<Vec<PkonEntry>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        viewer::scan_pkon(&paths)
            .into_iter()
            .map(|p| {
                let (info, error) = match viewer::inspect_file(&p) {
                    Ok(i) => (Some(i.into()), None),
                    Err(e) => (None, Some(e)),
                };
                PkonEntry {
                    name: p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    size: std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0),
                    path: p.to_string_lossy().into_owned(),
                    info,
                    error,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

/// Decode (or reuse the cached decode of) a file. Runs on a blocking thread.
async fn decode_cached(
    path: String,
    source: Option<String>,
    cache: &ViewerCache,
) -> Result<(Arc<viewer::Canonical>, Arc<Vec<u8>>), String> {
    let p = PathBuf::from(path);
    let meta = std::fs::metadata(&p).map_err(|e| e.to_string())?;
    let src = source.map(PathBuf::from);
    let key = (p.clone(), meta.len(), meta.modified().ok(), src.clone());
    if let Some(d) = cache.0.lock().unwrap().as_ref() {
        if d.key == key {
            return Ok((d.image.clone(), d.preview_png.clone()));
        }
    }
    let (image, png, note) = tauri::async_runtime::spawn_blocking(move || {
        let (img, note) = viewer::decode_file_with(&p, src.as_deref())?;
        let t = std::time::Instant::now();
        let png = viewer::encode_png(&img, true)?;
        eprintln!("[kon-viewer] preview PNG encode {:?}, {} bytes", t.elapsed(), png.len());
        Ok::<_, String>((Arc::new(img), Arc::new(png), note))
    })
    .await
    .map_err(|e| e.to_string())??;
    *cache.0.lock().unwrap() = Some(Decoded {
        key,
        image: image.clone(),
        preview_png: png.clone(),
        note,
    });
    Ok((image, png))
}

/// Decode for preview. Returns the PNG bytes as a raw binary IPC response (no JSON array overhead).
/// `Ok` means PKON's decoder accepted every integrity check, including the pixel CRC32.
#[tauri::command]
async fn decode_pkon(
    path: String,
    source: Option<String>,
    cache: State<'_, ViewerCache>,
) -> Result<tauri::ipc::Response, String> {
    let (_, png) = decode_cached(path, source, &cache).await?;
    Ok(tauri::ipc::Response::new(png.as_ref().clone()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportError {
    /// `exists` = destination exists and `overwrite` was not set; the UI must ask the user.
    kind: &'static str,
    message: String,
}

/// Export the decoded original as PNG. Never replaces an existing file unless `overwrite` is true, which the UI sets
/// only after the user explicitly confirms.
/// How the most recently decoded viewer image was rendered (RAW pipeline description).
#[tauri::command]
fn render_note(cache: State<'_, ViewerCache>) -> String {
    cache.0.lock().unwrap().as_ref().map(|d| d.note.clone()).unwrap_or_default()
}

#[tauri::command]
async fn export_pkon_png(
    path: String,
    dest: String,
    overwrite: bool,
    source: Option<String>,
    cache: State<'_, ViewerCache>,
) -> Result<u64, ExportError> {
    let other = |m: String| ExportError {
        kind: "other",
        message: m,
    };
    let dest = PathBuf::from(dest);
    if dest.is_dir() {
        return Err(other("Choose a file name, not a folder.".into()));
    }
    if dest.exists() && !overwrite {
        return Err(ExportError {
            kind: "exists",
            message: format!("{} already exists.", dest.display()),
        });
    }
    let (image, _) = decode_cached(path, source, &cache).await.map_err(other)?;
    tauri::async_runtime::spawn_blocking(move || {
        let png = viewer::encode_png(&image, false)?;
        platform::write_file(&dest, &png, overwrite)
            .map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
        Ok::<_, String>(png.len() as u64)
    })
    .await
    .map_err(|e| other(e.to_string()))?
    .map_err(other)
}

/// Both sides of a comparison, as preview PNGs, kept for the compare view (single entry).
#[derive(Default)]
struct CompareCache(Mutex<Option<(Arc<Vec<u8>>, Arc<Vec<u8>>)>>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompareInfo {
    width: u32,
    height: u32,
    identical: bool,
    verdict: String,
    rendering: Option<String>,
    ms: u64,
}

/// Load the original and decode the real `.pkon`, compare the underlying data, and stage both previews.
#[tauri::command]
async fn compare_open(
    source: String,
    pkon: String,
    cache: State<'_, CompareCache>,
) -> Result<CompareInfo, String> {
    let r = tauri::async_runtime::spawn_blocking(move || {
        let t = Instant::now();
        let c = viewer::compare_files(std::path::Path::new(&source), std::path::Path::new(&pkon))?;
        let (a, b) = std::thread::scope(|s| {
            let h = s.spawn(|| viewer::encode_png(&c.original, true));
            let b = viewer::encode_png(&c.decoded, true);
            (h.join().unwrap(), b)
        });
        let (a, b) = (a?, b?);
        eprintln!("[kon-compare] load+decode+compare+encode {:?}", t.elapsed());
        let (w, h) = (c.decoded.spec.width, c.decoded.spec.height);
        if (c.original.spec.width, c.original.spec.height) != (w, h) {
            return Err(format!("the selected original is {}×{} but the decoded .pkon is {w}×{h}: it is not this image's original", c.original.spec.width, c.original.spec.height));
        }
        Ok::<_, String>((
            CompareInfo { width: w, height: h, identical: c.identical, verdict: c.verdict, rendering: c.rendering, ms: t.elapsed().as_millis() as u64 },
            (Arc::new(a), Arc::new(b)),
        ))
    })
    .await
    .map_err(|e| e.to_string())??;
    *cache.0.lock().unwrap() = Some(r.1);
    Ok(r.0)
}

/// PNG bytes of one staged side: `original` or `compressed`.
#[tauri::command]
fn compare_image(side: String, cache: State<'_, CompareCache>) -> Result<tauri::ipc::Response, String> {
    let g = cache.0.lock().unwrap();
    let (a, b) = g.as_ref().ok_or("no comparison is loaded")?;
    let png = if side == "original" { a } else { b };
    Ok(tauri::ipc::Response::new(png.as_ref().clone()))
}

/// Write the bundled sample DNG (a real RICOH GR III Bayer RAW, byte-for-byte) and return its path.
#[tauri::command]
fn create_sample() -> Result<String, String> {
    let dir = std::env::temp_dir().join("kon-sample");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("kon-sample.dng");
    let bytes = viewer::SAMPLE_DNG;
    // Rewrite only if missing or different, so repeated clicks stay fast.
    if std::fs::read(&path).map(|b| b != bytes).unwrap_or(true) {
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    }
    Ok(path.to_string_lossy().into_owned())
}

/// Compress the built-in sample with the real engine into a `.pkon` for the Viewer demo (fully offline).
#[tauri::command]
async fn create_sample_pkon() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let png = create_sample()?;
        let bytes = kon_app::engine::Engine::compress(&Pkon, std::path::Path::new(&png))?;
        let out = std::path::Path::new(&png).with_extension("pkon");
        std::fs::write(&out, bytes).map_err(|e| e.to_string())?;
        Ok::<_, String>(out.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())?
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .manage(ViewerCache::default())
        .manage(CompareCache::default())
        .invoke_handler(tauri::generate_handler![
            add_paths,
            inspect_file,
            start_batch,
            cancel_batch,
            scan_pkon,
            decode_pkon,
            export_pkon_png,
            compare_open,
            compare_image,
            create_sample,
            create_sample_pkon,
            render_note
        ])
        .run(tauri::generate_context!())
        .expect("error while running KON");
}
