//! PKON Viewer core: discover `.pkon` files, read container headers, decode for display, and encode PNG.
//! Everything here reuses PKON's own parser/decoder (`pkon::pkon::parse_header`, `ulisa::decode`); there is no second decoder.
use crate::ingest;
pub use pkon::canon::Canonical;
use pkon::{
    canon::Layout,
    pkon::{self as container, CodecParams, Decoded, RawImage},
    ulisa,
};
use std::io::Read;
use std::path::{Path, PathBuf};

pub fn is_pkon(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pkon"))
}

/// Expand files/folders and keep only `.pkon` files.
pub fn scan_pkon(paths: &[PathBuf]) -> Vec<PathBuf> {
    ingest::scan(paths)
        .into_iter()
        .filter(|p| is_pkon(p))
        .collect()
}

/// Header fields the container actually stores, plus values derived from them. Nothing here is guessed:
/// the v2 container has no field for original filename, original file size, ICC profile, EXIF or text metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerInfo {
    pub container_version: u8,
    pub codec_id: u8,
    pub codec_name: &'static str,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub channels: u8,
    /// Colour layout (ELLIE) or a description of the mosaic (RAW codecs).
    pub layout: String,
    /// ELLIE only: `untransformed` or `YCoCg-R (reversible)`.
    pub representation: Option<String>,
    /// ELLIE only: Stage 3 model profile.
    pub profile: Option<String>,
    pub meta_version: u8,
    pub meta_len: u16,
    pub payload_len: u32,
    pub header_crc32: u32,
    pub pixel_crc32: u32,
    /// Size of the uncompressed pixel samples (w x h x channels x bytes/sample); NOT the size of any original file.
    pub raw_pixel_bytes: u64,
    /// Whether the viewer can display it: only decoded-pixel (ELLIE) images; RAW mosaics would need demosaicing.
    pub previewable: bool,
}

/// Bytes needed to parse a header: fixed header + the largest possible metadata block.
const HEADER_PREFIX: usize = container::FIXED_HEADER_LEN + u16::MAX as usize;

/// Read only the file prefix and validate the header (magic, version, header CRC, codec metadata). No pixels are decoded.
pub fn inspect_file(path: &Path) -> Result<ContainerInfo, String> {
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(HEADER_PREFIX as u64).read_to_end(&mut buf))
        .map_err(|e| e.to_string())?;
    inspect_bytes(&buf)
}

pub fn inspect_bytes(bytes: &[u8]) -> Result<ContainerInfo, String> {
    let h = container::parse_header(bytes).map_err(|e| e.to_string())?;
    let (codec_name, layout, representation, profile, previewable) = match &h.params {
        CodecParams::Ellie(p) => (
            "ELLIE",
            p.layout.name().to_string(),
            Some(
                match p.representation {
                    container::ELLIE_REPR_YCOCG_R => "YCoCg-R (reversible)",
                    _ => "untransformed",
                }
                .to_string(),
            ),
            Some(format!("{:?}", p.profile)),
            true,
        ),
        CodecParams::Lisa(_) => ("LISA", "RAW mosaic".to_string(), None, None, true),
        CodecParams::Ulisa(_) => ("ULISA", "RAW mosaic".to_string(), None, None, true),
    };
    let bps = container::bytes_per_sample(h.bit_depth) as u64;
    Ok(ContainerInfo {
        container_version: bytes[4],
        codec_id: h.codec,
        codec_name,
        width: h.width,
        height: h.height,
        bit_depth: h.bit_depth,
        channels: h.channels,
        layout,
        representation,
        profile,
        meta_version: h.meta_version,
        meta_len: h.meta_len,
        payload_len: h.payload_len,
        header_crc32: h.header_crc32,
        pixel_crc32: h.pixel_crc32,
        raw_pixel_bytes: u64::from(h.width) * u64::from(h.height) * u64::from(h.channels) * bps,
        previewable,
    })
}

/// Decode a `.pkon` file with PKON's own decoder. Its checks (header CRC, pixel CRC32 of the reconstructed samples)
/// all pass if this returns `Ok`. RAW mosaics are developed for display (see [`render_raw`]).
pub fn decode_file(path: &Path) -> Result<Canonical, String> {
    decode_file_with(path, None).map(|(img, _)| img)
}

/// Like [`decode_file`], but a RAW mosaic is developed with the colour data of `source` (the original DNG) when
/// given. Returns the picture and a description of how it was rendered.
pub fn decode_file_with(path: &Path, source: Option<&Path>) -> Result<(Canonical, String), String> {
    let t = std::time::Instant::now();
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let t_read = t.elapsed();
    let decoded = ulisa::decode(&bytes).map_err(|e| e.to_string())?;
    let t_dec = t.elapsed();
    match decoded {
        (_, Decoded::Standard(img)) => {
            eprintln!("[kon-viewer] read {t_read:?}, decode {:?}", t_dec - t_read);
            Ok((img, "Decoded pixels (no rendering applied)".into()))
        }
        // The decoder has already verified the exact mosaic; rendering below only reads it.
        (_, Decoded::Raw(raw)) => {
            let meta = source.and_then(|s| source_color_meta(s).ok());
            let r = render_raw(&raw, meta.as_ref())?;
            eprintln!(
                "[kon-viewer] read {t_read:?}, ULISA decode+verify {:?}, render {:?}",
                t_dec - t_read,
                t.elapsed() - t_dec
            );
            Ok((r.image, r.description))
        }
    }
}

fn source_color_meta(source: &Path) -> Result<crate::dng::ColorMeta, String> {
    crate::dng::color_metadata(&std::fs::read(source).map_err(|e| e.to_string())?)
}

/// Display/export-only rendering of a decoded RAW mosaic. The mosaic is only read. Bayer files go through the
/// camera-aware pipeline in [`crate::rawdev`] (generic and clearly labelled when `meta` is `None`); monochrome
/// files are shown as gray; anything else is refused rather than shown with wrong colours.
pub fn render_raw(raw: &RawImage, meta: Option<&crate::dng::ColorMeta>) -> Result<crate::rawdev::Rendered, String> {
    use pkon::lisa::SampleLayout as L;
    let (w, h) = (raw.metadata.width, raw.metadata.height);
    if w == 0 || h == 0 || raw.samples.len() != w * h {
        return Err("malformed RAW: sample count does not match the dimensions".into());
    }
    if !(1..=16).contains(&raw.bit_depth) {
        return Err(format!("malformed RAW: unsupported bit depth {}", raw.bit_depth));
    }
    match raw.metadata.layout {
        L::Monochrome => {
            let white = ((1u32 << raw.bit_depth) - 1) as f32;
            let data = raw
                .samples
                .iter()
                .map(|&s| {
                    let v = (s as f32 / white).clamp(0.0, 1.0);
                    let o = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
                    (o * 255.0 + 0.5) as u8
                })
                .collect();
            let spec = pkon::canon::Spec { width: w as u32, height: h as u32, layout: Layout::Gray, bit_depth: 8 };
            Ok(crate::rawdev::Rendered {
                image: Canonical::new(spec, data)?,
                description: "Monochrome sensor: linear to sRGB curve".into(),
                camera_aware: false,
            })
        }
        L::BayerCfa => {
            raw.metadata.validate().map_err(|e| format!("malformed RAW metadata: {e}"))?;
            let cfa = raw.metadata.cfa.as_ref().ok_or("malformed RAW metadata: Bayer layout without a CFA pattern")?;
            crate::rawdev::develop(&raw.samples, w, h, raw.bit_depth, cfa, meta)
        }
        L::OtherCfa => Err("unsupported RAW: non-Bayer colour filter array (e.g. X-Trans or a larger repeat); KON cannot demosaic it, so no preview is shown".into()),
        L::Linear | L::Unspecified => Err("unsupported RAW: the file does not describe a Bayer or monochrome sensor layout, so colours cannot be interpreted".into()),
        _ => Err("unsupported RAW sample layout".into()),
    }
}

/// Generic (metadata-free) rendering, as used when only the `.pkon` is available.
pub fn demosaic_preview(raw: &RawImage) -> Result<Canonical, String> {
    render_raw(raw, None).map(|r| r.image)
}

/// Result of comparing a source file with the image decoded from its `.pkon` output.
pub struct Comparison {
    /// Source rendered for display (RAW: same pipeline and settings as `decoded`).
    pub original: Canonical,
    /// Image decoded from the actual `.pkon` file.
    pub decoded: Canonical,
    /// True only if the underlying data were compared and found identical.
    pub identical: bool,
    /// Exactly what was compared and what was found.
    pub verdict: String,
    /// How the pictures were rendered (RAW only).
    pub rendering: Option<String>,
}

/// Decode the real `.pkon` and load the real source, then compare the underlying data (not the rendering).
/// PNG/JPEG: decoded pixels vs. the source's decoded pixels. DNG: decoded Bayer samples + CFA + bit depth vs. the
/// source's stored raster; both sides are then rendered with the same DNG colour data and settings.
pub fn compare_files(source: &Path, pkon_path: &Path) -> Result<Comparison, String> {
    let bytes = std::fs::read(pkon_path).map_err(|e| e.to_string())?;
    match ulisa::decode(&bytes).map_err(|e| e.to_string())?.1 {
        Decoded::Standard(decoded) => {
            let original = Canonical::load(source).map_err(|e| format!("this .pkon holds a standard image, but the selected file cannot be read as PNG/JPEG: {e}"))?;
            let (identical, verdict) = match pkon::canon::first_mismatch(&original, &decoded) {
                None => (
                    true,
                    format!(
                        "Pixel data identical: all {}×{} {} samples of the decoded .pkon equal the source's decoded pixels.",
                        original.spec.width,
                        original.spec.height,
                        original.spec.label()
                    ),
                ),
                Some(m) => (false, format!("Pixel data DIFFERS from the source: {m:?}")),
            };
            Ok(Comparison { original, decoded, identical, verdict, rendering: None })
        }
        Decoded::Raw(back) => {
            let source_bytes = std::fs::read(source).map_err(|e| e.to_string())?;
            let wrong = |e: String| format!("this .pkon holds a RAW mosaic, so the original must be the DNG it was made from ({e})");
            let src = crate::dng::ingest_dng(&source_bytes).map_err(wrong)?;
            let meta = crate::dng::color_metadata(&source_bytes).map_err(wrong)?;
            let same = back.samples == src.samples && back.bit_depth == src.bit_depth && back.metadata == src.metadata;
            let n = back.samples.len();
            let src_raw = RawImage { samples: src.samples, metadata: src.metadata, bit_depth: src.bit_depth };
            // Same function, same metadata, same settings on both sides.
            let (a, b) = std::thread::scope(|s| {
                let h = s.spawn(|| render_raw(&src_raw, Some(&meta)));
                (h.join().unwrap(), render_raw(&back, Some(&meta)))
            });
            let (a, b) = (a?, b?);
            let verdict = if same {
                format!(
                    "RAW mosaic identical: all {n} Bayer samples, the CFA pattern and the {}-bit depth of the decoded .pkon equal the DNG's stored raster.",
                    back.bit_depth
                )
            } else {
                "RAW mosaic DIFFERS from the DNG's stored raster.".to_string()
            };
            Ok(Comparison { original: a.image, decoded: b.image, identical: same, verdict, rendering: Some(b.description) })
        }
    }
}

/// Bundled demo photograph: an unmodified RICOH GR III Bayer RAW DNG (CC0, see `assets/sample/ATTRIBUTION.md`).
/// Embedded in the binary so the sample works offline; it goes through the normal DNG -> ULISA path.
pub const SAMPLE_DNG: &[u8] = include_bytes!("../assets/sample/kon-sample.dng");

/// Encode decoded pixels as PNG (8 or 16 bit; PNG stores 16-bit samples big-endian, PKON holds them little-endian).
/// `fast` trades size for speed, for previews.
pub fn encode_png(img: &Canonical, fast: bool) -> Result<Vec<u8>, String> {
    let s = img.spec;
    let color = match s.layout {
        Layout::Gray => png::ColorType::Grayscale,
        Layout::GrayAlpha => png::ColorType::GrayscaleAlpha,
        Layout::Rgb => png::ColorType::Rgb,
        Layout::Rgba => png::ColorType::Rgba,
    };
    let (depth, data): (_, std::borrow::Cow<[u8]>) = match s.bit_depth {
        8 => (png::BitDepth::Eight, (&img.data[..]).into()),
        16 => (
            png::BitDepth::Sixteen,
            img.data
                .chunks_exact(2)
                .flat_map(|c| [c[1], c[0]])
                .collect::<Vec<u8>>()
                .into(),
        ),
        d => return Err(format!("unsupported bit depth {d} for PNG")),
    };
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, s.width, s.height);
    enc.set_color(color);
    enc.set_depth(depth);
    enc.set_compression(if fast {
        png::Compression::Fast
    } else {
        png::Compression::Balanced
    });
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(&data).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, Pkon};

    #[test]
    fn pkon_roundtrip_through_viewer_matches_original_pixels() {
        let d = std::env::temp_dir().join(format!("kon-viewer-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        // 16-bit RGB gradient so the endianness conversion is exercised.
        let (w, h) = (17u32, 9u32);
        let mut be = Vec::new();
        for y in 0..h {
            for x in 0..w {
                for c in 0..3u32 {
                    be.extend_from_slice(&((x * 3000 + y * 700 + c * 11) as u16).to_be_bytes());
                }
            }
        }
        let src = d.join("src.png");
        {
            let mut enc = png::Encoder::new(std::fs::File::create(&src).unwrap(), w, h);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Sixteen);
            enc.write_header().unwrap().write_image_data(&be).unwrap();
        }
        let pk = d.join("src.pkon");
        std::fs::write(&pk, Pkon.compress(&src).unwrap()).unwrap();

        assert_eq!(scan_pkon(&[d.clone()]), vec![pk.clone()]);
        let info = inspect_file(&pk).unwrap();
        assert_eq!(
            (info.codec_name, info.width, info.height, info.bit_depth),
            ("ELLIE", w, h, 16)
        );
        assert_eq!(info.layout, "rgb");
        assert!(info.previewable);

        let img = decode_file(&pk).unwrap();
        let original = pkon::canon::Canonical::load(&src).unwrap();
        assert_eq!(pkon::canon::first_mismatch(&original, &img), None);
        // Exported PNG decodes back to the same pixels.
        let out = d.join("out.png");
        std::fs::write(&out, encode_png(&img, false).unwrap()).unwrap();
        let again = pkon::canon::Canonical::load(&out).unwrap();
        assert_eq!(pkon::canon::first_mismatch(&original, &again), None);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn corrupt_file_reports_error() {
        assert!(inspect_bytes(b"not a pkon file at all, definitely not").is_err());
    }
}
