//! RAW preview: demosaic is display-only; decoded Bayer samples stay bit-identical.
use kon_app::viewer;
use pkon::lisa::{CfaPattern, ColorFilter as C, ImageMetadata, SampleLayout};
use pkon::pkon::{Decoded, RawImage};
use pkon::ulisa::{self, Engine, Source};

fn raw_with(pattern: [C; 4], w: usize, h: usize, rgb: [u16; 3]) -> RawImage {
    let cfa = CfaPattern { width: 2, height: 2, entries: pattern.to_vec(), origin_x: 0, origin_y: 0 };
    let metadata = ImageMetadata::with_cfa(w, h, SampleLayout::BayerCfa, cfa.clone());
    let samples = (0..h * w)
        .map(|i| match cfa.color_at(i % w, i / w) { C::Red => rgb[0], C::Blue => rgb[2], _ => rgb[1] })
        .collect();
    RawImage { samples, metadata, bit_depth: 12 }
}

/// A scene that is pure red must come out red for every Bayer arrangement (wrong CFA handling would swap R/B).
#[test]
fn all_bayer_arrangements_render_the_right_colour() {
    use C::{Blue as B, Green as G, Red as R};
    for pat in [[R, G, G, B], [G, R, B, G], [G, B, R, G], [B, G, G, R]] {
        let raw = raw_with(pat, 16, 12, [4000, 100, 100]);
        let img = viewer::demosaic_preview(&raw).unwrap();
        // Gray-world WB neutralises a uniform colour cast, so test with a red-ish half instead: left red, right blue.
        let _ = img;
        let mut r = raw_with(pat, 16, 12, [4000, 100, 100]);
        let b = raw_with(pat, 16, 12, [100, 100, 4000]);
        for y in 0..12 { for x in 8..16 { r.samples[y * 16 + x] = b.samples[y * 16 + x]; } }
        let img = viewer::demosaic_preview(&r).unwrap();
        let px = |x: u32, y: u32| (img.sample(x, y, 0), img.sample(x, y, 1), img.sample(x, y, 2));
        let (rl, bl) = (px(3, 5), px(12, 5));
        assert!(rl.0 > rl.2 + 40, "{pat:?}: left should be red, got {rl:?}");
        assert!(bl.2 > bl.0 + 40, "{pat:?}: right should be blue, got {bl:?}");
    }
}

#[test]
fn malformed_and_unsupported_raw_gives_clear_error() {
    let mut raw = raw_with([C::Red, C::Green, C::Green, C::Blue], 4, 4, [1, 2, 3]);
    raw.samples.pop();
    assert!(viewer::demosaic_preview(&raw).unwrap_err().contains("malformed"));
    let mut x = raw_with([C::Red, C::Green, C::Green, C::Blue], 4, 4, [1, 2, 3]);
    x.metadata.layout = SampleLayout::OtherCfa;
    assert!(viewer::demosaic_preview(&x).unwrap_err().contains("unsupported"));
    let mut y = raw_with([C::Red, C::Green, C::Green, C::Blue], 4, 4, [1, 2, 3]);
    y.metadata.layout = SampleLayout::Unspecified;
    assert!(viewer::demosaic_preview(&y).is_err());
}

/// Real DNG -> ULISA .pkon -> viewer decode/preview/PNG; samples bit-identical, input mosaic untouched.
#[test]
fn real_dng_end_to_end() {
    let path = std::env::var("KON_TEST_RAW_DNG")
        .unwrap_or_else(|_| r"C:\Users\vishn\Downloads\Ricoh-GR-III.DNG".into());
    let Ok(bytes) = std::fs::read(&path) else { eprintln!("skip: {path} missing"); return };
    let d = kon_app::dng::ingest_dng(&bytes).unwrap();
    let file = ulisa::encode(Engine::Ulisa, &Source::Raw {
        samples: &d.samples, width: d.metadata.width, height: d.metadata.height,
        bit_depth: d.bit_depth, metadata: d.metadata.clone(),
    }).unwrap();
    let dir = std::env::temp_dir().join(format!("kon-rawprev-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pk = dir.join("gr3.pkon");
    std::fs::write(&pk, &file).unwrap();
    println!("{}x{} {}-bit, pkon {} bytes", d.metadata.width, d.metadata.height, d.bit_depth, file.len());

    let info = viewer::inspect_file(&pk).unwrap();
    assert!(info.previewable && info.codec_name == "ULISA");
    let Decoded::Raw(back) = ulisa::decode(&file).unwrap().1 else { panic!("not raw") };
    assert_eq!(back.samples, d.samples, "decoded Bayer samples must be bit-identical");
    assert_eq!(back.metadata, d.metadata);

    let before = back.samples.clone();
    let img = viewer::demosaic_preview(&back).unwrap();
    assert_eq!(back.samples, before);
    assert_eq!((img.spec.width as usize, img.spec.height as usize), (d.metadata.width, d.metadata.height));
    let viewed = viewer::decode_file(&pk).unwrap();
    assert_eq!(viewed.data, img.data);
    let png = viewer::encode_png(&viewed, false).unwrap();
    std::fs::write(dir.join("gr3.png"), &png).unwrap();
    let again = Canonical::load(&dir.join("gr3.png")).unwrap();
    assert_eq!(pkon::canon::first_mismatch(&viewed, &again), None);
    println!("PNG {} bytes at {}", png.len(), dir.join("gr3.png").display());
}
use pkon::canon::Canonical;

/// Real DNG rendered with its own camera metadata; optionally dumps the PNG (KON_RENDER_OUT) for reference comparison.
#[test]
fn real_dng_camera_aware_render() {
    let path = std::env::var("KON_TEST_RAW_DNG").unwrap_or_else(|_| r"C:\Users\vishn\Downloads\Ricoh-GR-III.DNG".into());
    let Ok(bytes) = std::fs::read(&path) else { eprintln!("skip: {path} missing"); return };
    let d = kon_app::dng::ingest_dng(&bytes).unwrap();
    let meta = kon_app::dng::color_metadata(&bytes).unwrap();
    println!("{meta:#?}");
    let raw = RawImage { samples: d.samples.clone(), metadata: d.metadata.clone(), bit_depth: d.bit_depth };
    let t = std::time::Instant::now();
    let r = viewer::render_raw(&raw, Some(&meta)).unwrap();
    println!("render {:?}: {}", t.elapsed(), r.description);
    assert!(r.camera_aware);
    assert_eq!(raw.samples, d.samples, "rendering must not touch the samples");
    let generic = viewer::render_raw(&raw, None).unwrap();
    assert!(!generic.camera_aware && generic.description.contains("GENERIC"));
    if let Ok(out) = std::env::var("KON_RENDER_OUT") {
        std::fs::write(&out, viewer::encode_png(&r.image, false).unwrap()).unwrap();
        println!("wrote {out}");
    }
}

fn compress_to(src: &std::path::Path, dst: &std::path::Path) {
    use kon_app::engine::{Engine as _, Pkon};
    std::fs::write(dst, Pkon.compress(src).unwrap()).unwrap();
}

/// Small synthetic RGB PNG (standard-image path coverage only; the app sample is a bundled DNG).
fn synthetic_png() -> Vec<u8> {
    let (w, h) = (64u32, 48u32);
    let data: Vec<u8> = (0..h).flat_map(|y| (0..w).flat_map(move |x| [(x * 4) as u8, (y * 5) as u8, ((x ^ y) * 3) as u8])).collect();
    let img = Canonical::new(pkon::canon::Spec { width: w, height: h, layout: pkon::canon::Layout::Rgb, bit_depth: 8 }, data).unwrap();
    viewer::encode_png(&img, false).unwrap()
}

/// Synthetic image -> real .pkon -> compare: identical only because the decoded pixels really are equal; a different
/// source must be reported as different.
#[test]
fn compare_sample_png_reports_pixel_identity_honestly() {
    let dir = std::env::temp_dir().join(format!("kon-cmp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("sample.png");
    std::fs::write(&src, synthetic_png()).unwrap();
    let pk = dir.join("sample.pkon");
    compress_to(&src, &pk);
    let c = viewer::compare_files(&src, &pk).unwrap();
    assert!(c.identical, "{}", c.verdict);
    assert_eq!(c.original.data, c.decoded.data);
    assert!(c.rendering.is_none());
    // Same .pkon against a different image of the same size: must NOT claim identity.
    let mut other = synthetic_png();
    let img = Canonical::load(&src).unwrap();
    let mut data = img.data.clone();
    data[100] ^= 0x40;
    let altered = Canonical::new(img.spec, data).unwrap();
    other.clear();
    other.extend(viewer::encode_png(&altered, true).unwrap());
    let alt = dir.join("alt.png");
    std::fs::write(&alt, other).unwrap();
    let c2 = viewer::compare_files(&alt, &pk).unwrap();
    assert!(!c2.identical && c2.verdict.contains("DIFFERS"), "{}", c2.verdict);
    std::fs::remove_dir_all(&dir).ok();
}

/// Real DNG -> real .pkon -> compare: both sides rendered with the DNG's colour data and identical settings.
#[test]
fn compare_real_dng_uses_same_rendering_both_sides() {
    let path = std::env::var("KON_TEST_RAW_DNG").unwrap_or_else(|_| r"C:\Users\vishn\Downloads\Ricoh-GR-III.DNG".into());
    if !std::path::Path::new(&path).exists() { eprintln!("skip"); return; }
    let dir = std::env::temp_dir().join(format!("kon-cmpd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pk = dir.join("gr3.pkon");
    compress_to(std::path::Path::new(&path), &pk);
    let c = viewer::compare_files(std::path::Path::new(&path), &pk).unwrap();
    assert!(c.identical, "{}", c.verdict);
    assert_eq!(c.original.data, c.decoded.data, "identical mosaics + identical settings must render identically");
    assert!(c.rendering.unwrap().starts_with("Camera-aware"));
    println!("{}", c.verdict);
    std::fs::remove_dir_all(&dir).ok();
}

/// The bundled demo photograph: genuine Bayer RAW, real ULISA compression, exact mosaic identity, camera-aware render.
#[test]
fn bundled_sample_dng_round_trips_exactly() {
    let dir = std::env::temp_dir().join(format!("kon-sample-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("kon-sample.dng");
    std::fs::write(&src, viewer::SAMPLE_DNG).unwrap();
    let ing = kon_app::dng::ingest_dng(viewer::SAMPLE_DNG).unwrap();
    assert_eq!(ing.bit_depth, 14);
    assert_eq!(ing.metadata.layout, SampleLayout::BayerCfa);
    let pk = dir.join("kon-sample.pkon");
    let t = std::time::Instant::now();
    compress_to(&src, &pk);
    eprintln!("SAMPLE dims {}x{} bits {} dng {} pkon {} compress {:?}", ing.metadata.width, ing.metadata.height, ing.bit_depth, viewer::SAMPLE_DNG.len(), std::fs::metadata(&pk).unwrap().len(), t.elapsed());
    let t = std::time::Instant::now();
    let c = viewer::compare_files(&src, &pk).unwrap();
    eprintln!("SAMPLE compare {:?}: {} | {:?}", t.elapsed(), c.verdict, c.rendering);
    assert!(c.identical, "{}", c.verdict);
    assert_eq!(c.original.data, c.decoded.data);
    std::fs::remove_dir_all(&dir).ok();
}
