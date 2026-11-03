//! Q: does the native Rust DNG reader produce exactly what the Python reference (pkon::dng::ingest_dng) produced,
//! for every DNG in the corpus? Any difference is a bug until explained. Needs Python only for this oracle test.
use std::path::PathBuf;

fn corpus() -> Vec<PathBuf> {
    let dir = PathBuf::from(std::env::var("KON_TEST_DNG_DIR").expect("set KON_TEST_DNG_DIR"));
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dng")))
        .collect();
    v.sort();
    assert!(v.len() >= 40, "expected the full corpus, found {}", v.len());
    v
}

#[test]
fn native_matches_python_on_every_corpus_file() {
    let mut bad = Vec::new();
    for p in corpus() {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(&p).unwrap();
        let (nat, py) = (kon_app::dng::ingest_dng(&bytes), pkon::dng::ingest_dng(&bytes));
        match (nat, py) {
            (Ok(n), Ok(r)) => {
                let same = n.samples == r.samples
                    && n.bit_depth == r.bit_depth
                    && n.declared_bits == r.declared_bits
                    && n.metadata == r.metadata
                    && n.camera == r.camera
                    && n.dng_compression == r.dng_compression;
                println!("{} {name}: {}x{} {}-bit {:?} {}", if same { "SAME" } else { "DIFF" }, n.metadata.width, n.metadata.height, n.bit_depth, n.metadata.layout, n.dng_compression);
                if !same {
                    let first = n.samples.iter().zip(&r.samples).position(|(a, b)| a != b);
                    bad.push(format!("{name}: bd {}/{} decl {}/{} meta_eq {} cam {:?}/{:?} comp {}/{} first sample diff {first:?}", n.bit_depth, r.bit_depth, n.declared_bits, r.declared_bits, n.metadata == r.metadata, n.camera, r.camera, n.dng_compression, r.dng_compression));
                }
            }
            (Err(a), Err(b)) => {
                println!("BOTH-ERR {name}\n   native: {a}\n   python: {b}");
                if a != b {
                    bad.push(format!("{name}: error text differs"));
                }
            }
            (n, r) => bad.push(format!("{name}: native ok={} python ok={} ({:?} / {:?})", n.is_ok(), r.is_ok(), n.err(), r.err())),
        }
    }
    assert!(bad.is_empty(), "native/python disagree:\n{}", bad.join("\n"));
}
