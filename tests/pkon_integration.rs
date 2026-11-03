//! Cross-platform engine checks. Run on every OS in CI; each answers one question (see comments).
use pkon::canon::{Canonical, Layout, Spec};
use pkon::pkon::Decoded;
use pkon::ulisa::{self, Engine, Source};
use std::path::Path;

/// Q: does PKON, built on THIS OS/arch, decode files produced elsewhere? (Byte-level portability of the format.)
#[test]
fn decodes_pkon_golden_files() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../pkon/tests/golden_pkon");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).expect("pkon sibling checkout with golden files") {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "pkon") {
            ulisa::decode(&std::fs::read(&p).unwrap())
                .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            n += 1;
        }
    }
    assert!(n >= 5, "expected golden files, found {n}");
}

/// Q: is ELLIE *encoding* bit-identical across OS/arch? Pinned from the Windows x86_64 build; a mismatch elsewhere
/// means non-portable output (e.g. float or endianness dependence) and must be investigated, not re-pinned.
#[test]
fn ellie_encode_is_deterministic_across_platforms() {
    let (w, h) = (16u32, 12u32);
    let data = (0..w * h * 3).map(|i| (i * 3 % 251) as u8).collect();
    let img = Canonical::new(
        Spec {
            width: w,
            height: h,
            layout: Layout::Rgb,
            bit_depth: 8,
        },
        data,
    )
    .unwrap();
    let file = ulisa::encode(Engine::Ellie, &Source::Standard(&img)).unwrap();
    assert!(
        matches!(ulisa::decode(&file).unwrap(), (Engine::Ellie, Decoded::Standard(c)) if c.data == img.data)
    );
    let fnv = file.iter().fold(0xcbf29ce484222325u64, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    });
    println!("len={} fnv={fnv:016x}", file.len());
    assert_eq!((file.len(), fnv), (PINNED_LEN, PINNED_FNV));
}
const PINNED_LEN: usize = 201;
const PINNED_FNV: u64 = 0x7037de82c5c8171b;
