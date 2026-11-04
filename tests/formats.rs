//! Format coverage through the real PKON adapter and the real JobRunner (no fakes).
//! Fixtures: KON_TEST_FIXTURES (small PNG/JPEG/invalid files) and KON_TEST_DNG_DIR (real DNG corpus). A missing
//! fixture is a hard failure, never a silent skip.
use kon_app::engine::{detect, Format, Pkon};
use kon_app::job::{Event, Job, JobRunner};
use kon_app::viewer;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn fixtures() -> PathBuf {
    PathBuf::from(std::env::var("KON_TEST_FIXTURES").expect("set KON_TEST_FIXTURES"))
}
fn dng(name: &str) -> PathBuf {
    let p =
        PathBuf::from(std::env::var("KON_TEST_DNG_DIR").expect("set KON_TEST_DNG_DIR")).join(name);
    assert!(p.exists(), "missing DNG fixture {}", p.display());
    p
}
fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kon-fmt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
fn run(inputs: &[PathBuf], dest: &Path) -> Vec<Event> {
    let r = JobRunner::spawn(Arc::new(Pkon));
    for (i, p) in inputs.iter().enumerate() {
        r.submit(Job {
            id: i as u64,
            input: p.clone(),
            dest_dir: dest.to_path_buf(),
        });
    }
    let mut term = Vec::new();
    while term.len() < inputs.len() {
        match r.events.recv().unwrap() {
            e @ (Event::Done { .. } | Event::Failed { .. } | Event::Cancelled { .. }) => {
                term.push(e)
            }
            _ => {}
        }
    }
    r.shutdown();
    term
}
fn sig(p: &Path) -> (u64, Vec<u8>) {
    let b = std::fs::read(p).unwrap();
    (b.len() as u64, b)
}

/// Q: does detection follow file content (not extension) and give an accurate reason for everything else?
#[test]
fn detection_is_by_content_with_accurate_reasons() {
    let f = fixtures();
    assert_eq!(detect(&f.join("rgb.png")), Ok(Format::Png));
    assert_eq!(detect(&f.join("prog.jpg")), Ok(Format::Jpeg));
    assert_eq!(detect(&f.join("odd.JPEG")), Ok(Format::Jpeg));
    assert_eq!(detect(&dng("Nikon__D80__DSC_0143.dng")), Ok(Format::Dng));
    let tif = detect(&f.join("plain.tif")).unwrap_err();
    assert!(tif.contains("not a DNG"), "{tif}");
    assert!(detect(&f.join("fake.dng"))
        .unwrap_err()
        .contains("TIFF/DNG header"));
    assert!(detect(&f.join("fake.png"))
        .unwrap_err()
        .contains("PNG, JPEG and DNG"));
    assert!(detect(&f.join("note.txt")).unwrap_err().contains(".txt"));
    // JPEG bytes with a .png name are routed by content.
    let d = scratch("misnamed");
    std::fs::copy(f.join("prog.jpg"), d.join("looks_like.png")).unwrap();
    assert_eq!(detect(&d.join("looks_like.png")), Ok(Format::Jpeg));
}

/// Q: for PNG and every JPEG subtype we ship fixtures for, is the .pkon an ELLIE container that decodes to exactly the
/// pixels the source decodes to, with the source untouched and the extension/name correct?
#[test]
fn png_and_jpeg_roundtrip_exactly_through_runner_and_viewer() {
    let f = fixtures();
    let inputs: Vec<PathBuf> = ["rgb.png", "prog.jpg", "gray.jpg", "odd.JPEG"]
        .iter()
        .map(|n| f.join(n))
        .collect();
    let before: Vec<_> = inputs.iter().map(|p| sig(p)).collect();
    let d = scratch("std");
    let evs = run(&inputs, &d);
    for (i, ev) in evs.iter().enumerate() {
        let Event::Done {
            output, out_bytes, ..
        } = ev
        else {
            panic!("{}: {ev:?}", inputs[i].display())
        };
        assert_eq!(output.extension().unwrap(), "pkon");
        assert_eq!(output.file_stem(), inputs[i].file_stem());
        let info = viewer::inspect_file(output).unwrap();
        assert_eq!(info.codec_name, "ELLIE");
        let back = viewer::decode_file(output).unwrap();
        let orig = pkon::canon::Canonical::load(&inputs[i]).unwrap();
        assert_eq!(pkon::canon::first_mismatch(&orig, &back), None);
        println!(
            "{:<10} {:>8} -> {:>8} bytes ({})",
            inputs[i].file_name().unwrap().to_string_lossy(),
            before[i].0,
            out_bytes,
            info.layout
        );
    }
    let after: Vec<_> = inputs.iter().map(|p| sig(p)).collect();
    assert_eq!(before, after, "source files must be untouched");
}

/// Q: are unsupported/broken inputs refused with a specific message, and nothing written?
#[test]
fn unsupported_and_broken_inputs_fail_cleanly() {
    let f = fixtures();
    let d = scratch("bad");
    for (name, needle) in [
        ("fake.png", "PNG, JPEG and DNG"),
        ("fake.dng", "TIFF/DNG header"),
        ("plain.tif", "not a DNG"),
    ] {
        let ev = run(&[f.join(name)], &d).remove(0);
        let Event::Failed { error, .. } = ev else {
            panic!("{name} should fail, got {ev:?}")
        };
        assert!(error.contains(needle), "{name}: {error}");
        println!("{name}: {error}");
    }
    assert_eq!(
        std::fs::read_dir(&d).unwrap().count(),
        0,
        "failed jobs must write nothing"
    );
}

/// Q: does a real Bayer DNG compress through the app, verify, and decode (by the viewer's decoder) to exactly the stored
/// sensor samples, with ULISA codec 7, and does the viewer refuse to fake a preview?
#[test]
fn bayer_dng_roundtrips_exactly() {
    let src = dng("Nikon__D80__DSC_0143.dng");
    let before = sig(&src);
    let d = scratch("dng");
    let Event::Done {
        output,
        in_bytes,
        out_bytes,
        ..
    } = run(&[src.clone()], &d).remove(0)
    else {
        panic!("DNG job failed")
    };
    assert_eq!(output.extension().unwrap(), "pkon");
    let info = viewer::inspect_file(&output).unwrap();
    assert_eq!(info.codec_name, "ULISA");
    assert!(info.previewable);
    let (_, dec) = pkon::ulisa::decode(&std::fs::read(&output).unwrap()).unwrap();
    let pkon::pkon::Decoded::Raw(raw) = dec else {
        panic!("not RAW")
    };
    let ing = pkon::dng::ingest_dng(&std::fs::read(&src).unwrap()).unwrap();
    assert_eq!(raw.samples, ing.samples);
    assert_eq!(raw.metadata, ing.metadata);
    assert_eq!(raw.bit_depth, ing.bit_depth);
    let preview = viewer::decode_file(&output).unwrap();
    assert_eq!(
        (preview.spec.width as usize, preview.spec.height as usize),
        (ing.metadata.width, ing.metadata.height)
    );
    assert_eq!(before, sig(&src));
    println!(
        "D80 {in_bytes} -> {out_bytes} ({:.2}x), {}x{} {}-bit",
        in_bytes as f64 / out_bytes as f64,
        info.width,
        info.height,
        info.bit_depth
    );
}

/// Q: does a mixed PNG+JPEG+DNG+unsupported batch finish with each file handled by its own engine?
#[test]
fn mixed_batch_uses_the_right_engine_per_file() {
    let f = fixtures();
    let inputs = vec![
        f.join("rgb.png"),
        dng("Nikon__D80__DSC_0143.dng"),
        f.join("prog.jpg"),
        f.join("note.txt"),
    ];
    let d = scratch("mixed");
    let evs = run(&inputs, &d);
    let mut codecs = Vec::new();
    for (p, ev) in inputs.iter().zip(&evs) {
        match ev {
            Event::Done { output, .. } => {
                codecs.push(viewer::inspect_file(output).unwrap().codec_name)
            }
            Event::Failed { .. } => codecs.push("failed"),
            e => panic!("{}: {e:?}", p.display()),
        }
    }
    assert_eq!(codecs, ["ELLIE", "ULISA", "ELLIE", "failed"]);
}

/// Q: does cancellation still stop queued DNG/JPEG work without writing output?
#[test]
fn cancel_pending_skips_queued_jobs() {
    let f = fixtures();
    let d = scratch("cancel");
    let r = JobRunner::spawn(Arc::new(Pkon));
    r.cancel_pending();
    for i in 0..3 {
        r.submit(Job {
            id: i,
            input: f.join("prog.jpg"),
            dest_dir: d.clone(),
        });
    }
    for _ in 0..3 {
        assert!(matches!(r.events.recv().unwrap(), Event::Cancelled { .. }));
    }
    r.shutdown();
    assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
}

/// Q: what happens to a CMYK JPEG? Observed: the JPEG decoder converts it to RGB, and KON stores those decoded pixels
/// exactly (verified). Pinned so a change in that behaviour is noticed.
#[test]
fn cmyk_jpeg_is_stored_as_decoder_rgb() {
    let d = scratch("cmyk");
    let Event::Done { output, .. } = run(&[fixtures().join("cmyk.jpg")], &d).remove(0) else {
        panic!("failed")
    };
    assert_eq!(viewer::inspect_file(&output).unwrap().layout, "rgb");
}
