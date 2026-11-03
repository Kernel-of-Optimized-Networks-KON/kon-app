//! Seam between the app and independent compression engines. An engine adapter only translates files <-> bytes;
//! it holds no GUI, threading, or path logic. PKON is the first adapter.
use std::path::Path;

pub trait Engine: Send + Sync {
    fn name(&self) -> &'static str;
    /// File extension of this engine's output (no dot).
    fn extension(&self) -> &'static str;
    fn compress(&self, input: &Path) -> Result<Vec<u8>, String>;
    /// Decode `compressed` and prove it reproduces `input` exactly.
    fn verify(&self, input: &Path, compressed: &[u8]) -> Result<(), String>;
}

/// Input formats KON can route to a PKON engine. Detected from file content, never from the extension alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Dng,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpeg => "JPEG",
            Format::Dng => "DNG",
        }
    }
    /// The PKON engine that encodes this format: standard pixels -> ELLIE; RAW mosaic -> ULISA (Bayer codec 7,
    /// which PKON itself replaces with LISA's RAW path, codec 4, for a CFA the Bayer frame cannot represent).
    pub fn engine(self) -> &'static str {
        match self {
            Format::Png | Format::Jpeg => "ELLIE",
            Format::Dng => "ULISA",
        }
    }
}

/// Decide the format from the first bytes of the file. The error text says exactly why a file is not accepted.
pub fn detect(path: &Path) -> Result<Format, String> {
    use std::io::Read;
    let mut head = [0u8; 8];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|e| format!("cannot read file: {e}"))?;
    let head = &head[..n];
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Ok(Format::Png)
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Ok(Format::Jpeg)
    } else if pkon::dng::looks_like_tiff(head) {
        if ext == "dng" {
            Ok(Format::Dng)
        } else {
            Err(format!(
                "This is a TIFF-structured file (.{ext}), not a DNG. KON compresses DNG raw files only; \
                 proprietary raw formats (CR2, NEF, ARW, ...) and plain TIFF are not supported."
            ))
        }
    } else if ext == "dng" {
        Err("This .dng file does not have a TIFF/DNG header (BigTIFF and damaged files are not supported).".into())
    } else {
        Err(format!(
            "Unsupported format{}. KON compresses PNG, JPEG and DNG.",
            if ext.is_empty() {
                String::new()
            } else {
                format!(" (.{ext})")
            }
        ))
    }
}

/// PKON via its ULISA router: PNG/JPEG decoded pixels -> ELLIE (codec 6); DNG stored raster -> ULISA (codec 7, LISA
/// fallback for non-Bayer CFAs). All output is a `.pkon` v2 container. DNG ingestion needs Python (see `pkon::dng`).
pub struct Pkon;

impl Engine for Pkon {
    fn name(&self) -> &'static str {
        "PKON"
    }
    fn extension(&self) -> &'static str {
        "pkon"
    }
    fn compress(&self, input: &Path) -> Result<Vec<u8>, String> {
        use pkon::ulisa::{self, Engine as E, Source};
        match detect(input)? {
            Format::Png => {
                let img = pkon::canon::Canonical::load(input)?;
                ulisa::encode(E::Ellie, &Source::Standard(&img)).map_err(|e| e.to_string())
            }
            Format::Jpeg => {
                let img = pkon::canon::Canonical::load(input).map_err(|e| {
                    format!("JPEG cannot be decoded (only 8-bit gray/RGB JPEG is supported): {e}")
                })?;
                ulisa::encode(E::Ellie, &Source::Standard(&img)).map_err(|e| e.to_string())
            }
            Format::Dng => {
                let d = ingest_dng(input)?;
                let src = Source::Raw {
                    samples: &d.samples,
                    width: d.metadata.width,
                    height: d.metadata.height,
                    bit_depth: d.bit_depth,
                    metadata: d.metadata.clone(),
                };
                ulisa::encode(E::Ulisa, &src).map_err(|e| e.to_string())
            }
        }
    }
    fn verify(&self, input: &Path, compressed: &[u8]) -> Result<(), String> {
        use pkon::pkon::Decoded;
        // The source is read again independently, so verification does not trust the encoder's in-memory copy.
        let decoded = pkon::ulisa::decode(compressed)
            .map_err(|e| e.to_string())?
            .1;
        match (detect(input)?, decoded) {
            (Format::Png | Format::Jpeg, Decoded::Standard(back)) => {
                let original = pkon::canon::Canonical::load(input)?;
                match pkon::canon::first_mismatch(&original, &back) {
                    None => Ok(()),
                    Some(m) => Err(format!("round trip mismatch: {m:?}")),
                }
            }
            (Format::Dng, Decoded::Raw(back)) => {
                let d = ingest_dng(input)?;
                if back.bit_depth != d.bit_depth || back.metadata != d.metadata {
                    Err("round trip mismatch: RAW geometry, CFA or bit depth differ".into())
                } else if back.samples != d.samples {
                    let n = back
                        .samples
                        .iter()
                        .zip(&d.samples)
                        .filter(|(a, b)| a != b)
                        .count();
                    Err(format!("round trip mismatch: {n} RAW samples differ"))
                } else {
                    Ok(())
                }
            }
            (Format::Dng, Decoded::Standard(_)) => Err("decoded payload is not a RAW mosaic".into()),
            _ => Err("decoded payload is not a standard image".into()),
        }
    }
}

fn ingest_dng(input: &Path) -> Result<pkon::dng::DngInput, String> {
    let bytes = std::fs::read(input).map_err(|e| e.to_string())?;
    crate::dng::ingest_dng(&bytes)
}

/// What PKON can tell us about an input without writing anything. Reflects only what the loader actually
/// reads: pixel geometry, the embedded ICC profile (present or not), and loader notes about representation changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub layout: String,
    pub has_icc: bool,
    pub icc_bytes: usize,
    pub notes: Vec<String>,
}

impl Pkon {
    /// Whether the PKON adapter can take this file (PNG, JPEG or DNG, by content).
    pub fn accepts(path: &Path) -> bool {
        detect(path).is_ok()
    }

    pub fn inspect(path: &Path) -> Result<ImageInfo, String> {
        let fmt = detect(path)?;
        if fmt != Format::Dng {
            let img = pkon::canon::Canonical::load(path)?;
            let mut notes = img.notes.clone();
            if fmt == Format::Jpeg {
                notes.push(
                    "JPEG is stored as its decoded pixels, reproduced exactly. The original JPEG bytes, EXIF and \
                     ICC profile cannot be recovered from the output."
                        .into(),
                );
            }
            return Ok(ImageInfo {
                width: img.spec.width,
                height: img.spec.height,
                bit_depth: img.spec.bit_depth,
                layout: img.spec.layout.name().to_string(),
                has_icc: img.icc.is_some(),
                icc_bytes: img.icc.as_ref().map_or(0, |v| v.len()),
                notes,
            });
        }
        let d = ingest_dng(path)?;
        let m = &d.metadata;
        let pattern = m.cfa.as_ref().map(|c| {
            c.entries
                .iter()
                .map(|f| match f {
                    pkon::lisa::ColorFilter::Red => 'R',
                    pkon::lisa::ColorFilter::Green => 'G',
                    pkon::lisa::ColorFilter::Blue => 'B',
                    _ => '?',
                })
                .collect::<String>()
        });
        let layout = match (&m.layout, &pattern) {
            (pkon::lisa::SampleLayout::BayerCfa, Some(p)) => format!("RAW Bayer mosaic {p}"),
            (pkon::lisa::SampleLayout::OtherCfa, _) => "RAW non-Bayer CFA mosaic (LISA fallback)".to_string(),
            _ => format!("RAW {:?}", m.layout),
        };
        let mut notes = vec![
            "The stored sensor raster is compressed unmodified (no crop, black level, white balance or demosaic). \
             The output is a .pkon file, not a DNG: DNG tags, EXIF, previews and maker notes are not stored."
                .to_string(),
        ];
        if !d.camera.is_empty() {
            notes.push(format!("Camera: {} (read for display only; not stored)", d.camera));
        }
        Ok(ImageInfo {
            width: m.width as u32,
            height: m.height as u32,
            bit_depth: d.bit_depth,
            layout,
            has_icc: false,
            icc_bytes: 0,
            notes,
        })
    }
}
