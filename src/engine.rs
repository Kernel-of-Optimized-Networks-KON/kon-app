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

/// PKON via its ULISA router. Currently standard 8/16-bit PNG input -> ELLIE. RAW/DNG is not wired
/// (PKON's DNG path shells out to Python; decide separately whether the app ships that).
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
        let img = pkon::canon::Canonical::load(input)?;
        ulisa::encode(E::Ellie, &Source::Standard(&img)).map_err(|e| e.to_string())
    }
    fn verify(&self, input: &Path, compressed: &[u8]) -> Result<(), String> {
        use pkon::pkon::Decoded;
        let original = pkon::canon::Canonical::load(input)?;
        match pkon::ulisa::decode(compressed).map_err(|e| e.to_string())? {
            (_, Decoded::Standard(back)) => match pkon::canon::first_mismatch(&original, &back) {
                None => Ok(()),
                Some(m) => Err(format!("round trip mismatch: {m:?}")),
            },
            _ => Err("decoded payload is not a standard image".into()),
        }
    }
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
    /// Whether the PKON adapter can currently take this file (standard PNG only; RAW/DNG is not wired).
    pub fn accepts(path: &Path) -> bool {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("png"))
    }

    pub fn inspect(path: &Path) -> Result<ImageInfo, String> {
        let img = pkon::canon::Canonical::load(path)?;
        Ok(ImageInfo {
            width: img.spec.width,
            height: img.spec.height,
            bit_depth: img.spec.bit_depth,
            layout: img.spec.layout.name().to_string(),
            has_icc: img.icc.is_some(),
            icc_bytes: img.icc.as_ref().map_or(0, |v| v.len()),
            notes: img.notes.clone(),
        })
    }
}
