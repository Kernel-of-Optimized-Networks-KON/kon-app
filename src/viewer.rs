//! PKON Viewer core: discover `.pkon` files, read container headers, decode for display, and encode PNG.
//! Everything here reuses PKON's own parser/decoder (`pkon::pkon::parse_header`, `ulisa::decode`); there is no second decoder.
use crate::ingest;
pub use pkon::canon::Canonical;
use pkon::{
    canon::Layout,
    pkon::{self as container, CodecParams, Decoded},
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
        CodecParams::Lisa(_) => ("LISA", "RAW mosaic".to_string(), None, None, false),
        CodecParams::Ulisa(_) => ("ULISA", "RAW mosaic".to_string(), None, None, false),
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
/// all pass if this returns `Ok`. RAW mosaics are reported as an error rather than shown as a fake picture.
pub fn decode_file(path: &Path) -> Result<Canonical, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    match ulisa::decode(&bytes).map_err(|e| e.to_string())? {
        (_, Decoded::Standard(img)) => Ok(img),
        (engine, Decoded::Raw(_)) => Err(format!(
            "{engine:?} RAW mosaic: decodes, but cannot be previewed or exported as a picture (no demosaicing in KON yet)"
        )),
    }
}

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
