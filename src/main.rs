//! Minimal integration proof: KON calls PKON's real ULISA/container APIs (no GUI yet).
use pkon::canon::{Canonical, Layout, Spec};
use pkon::lisa::{ImageMetadata, SampleLayout};
use pkon::pkon::Decoded;
use pkon::ulisa::{self, Engine, Source};

fn main() {
    // Small synthetic monochrome RAW frame, encoded with LISA through the ULISA router.
    let (w, h) = (24usize, 16usize);
    let samples: Vec<u16> = (0..w * h).map(|i| ((i * 7 + (i / w) * 13) % 4096) as u16).collect();
    let meta = ImageMetadata::without_cfa(w, h, SampleLayout::Monochrome);
    let src = Source::Raw { samples: &samples, width: w, height: h, bit_depth: 12, metadata: meta };
    let file = ulisa::encode(Engine::Lisa, &src).expect("PKON encode failed");
    assert_eq!(ulisa::engine_of(&file).unwrap(), Engine::Lisa);
    match ulisa::decode(&file).expect("PKON decode failed") {
        (Engine::Lisa, Decoded::Raw(r)) => {
            assert_eq!(r.samples, samples, "RAW round trip not lossless");
            println!("RAW/LISA: {} samples -> {} bytes -> lossless OK", samples.len(), file.len());
        }
        _ => panic!("unexpected engine or payload"),
    }

    // Standard RGB image, encoded with ELLIE.
    let (w, h) = (16u32, 12u32);
    let data = (0..w * h * 3).map(|i| (i * 3 % 251) as u8).collect();
    let img = Canonical::new(Spec { width: w, height: h, layout: Layout::Rgb, bit_depth: 8 }, data).unwrap();
    let file = ulisa::encode(Engine::Ellie, &Source::Standard(&img)).expect("PKON encode failed");
    match ulisa::decode(&file).expect("PKON decode failed") {
        (Engine::Ellie, Decoded::Standard(c)) => {
            assert_eq!(c.data, img.data, "ELLIE round trip not lossless");
            println!("RGB/ELLIE: {} bytes -> {} bytes -> lossless OK", img.data.len(), file.len());
        }
        _ => panic!("unexpected engine or payload"),
    }
}
