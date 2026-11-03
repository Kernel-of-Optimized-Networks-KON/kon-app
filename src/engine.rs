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
