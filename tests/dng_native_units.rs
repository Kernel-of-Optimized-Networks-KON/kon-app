//! Fixture-free checks of the native DNG reader on hand-built TIFFs, plus truncation robustness on a real file.
use kon_app::dng::ingest_dng;

/// Minimal single-IFD TIFF/DNG builder: (tag, type, values) with values stored out of line.
fn tiff(le: bool, tags: &[(u16, u16, Vec<u32>)], data: &[u8]) -> Vec<u8> {
    let w16 = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
    let w32 = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };
    let n = tags.len();
    let ifd_len = 2 + n * 12 + 4;
    let data_off = 8 + ifd_len;
    // data at data_off, then out-of-line values
    let mut extra = Vec::new();
    let mut ifd = Vec::new();
    ifd.extend(w16(n as u16));
    let mut sorted = tags.to_vec();
    sorted.sort_by_key(|t| t.0);
    for (tag, typ, vals) in &sorted {
        ifd.extend(w16(*tag));
        ifd.extend(w16(*typ));
        ifd.extend(w32(vals.len() as u32));
        let bytes: Vec<u8> = vals
            .iter()
            .flat_map(|&v| match typ {
                1 => vec![v as u8],
                3 => w16(v as u16).to_vec(),
                _ => w32(v).to_vec(),
            })
            .collect();
        if bytes.len() <= 4 {
            let mut f = bytes.clone();
            f.resize(4, 0);
            ifd.extend(f);
        } else {
            ifd.extend(w32((data_off + data.len() + extra.len()) as u32));
            extra.extend(bytes);
        }
    }
    ifd.extend(w32(0));
    let mut out = if le {
        b"II*\0".to_vec()
    } else {
        b"MM\0*".to_vec()
    };
    out.extend(w32(8));
    out.extend(ifd);
    out.extend(data);
    out.extend(extra);
    out
}

fn raw_tags(
    w: u32,
    h: u32,
    bps: u32,
    comp: u32,
    data_len: u32,
    off: u32,
) -> Vec<(u16, u16, Vec<u32>)> {
    vec![
        (256, 4, vec![w]),
        (257, 4, vec![h]),
        (258, 3, vec![bps]),
        (259, 3, vec![comp]),
        (262, 3, vec![32803]),
        (273, 4, vec![off]),
        (277, 3, vec![1]),
        (278, 4, vec![h]),
        (279, 4, vec![data_len]),
        (33421, 3, vec![2, 2]),
        (33422, 1, vec![0, 1, 1, 2]),
    ]
}

/// Q: are bit-packed 12-bit rows (byte-aligned per row), both byte orders, and 16-bit big-endian read exactly?
#[test]
fn packed_and_endian_variants_decode_exactly() {
    // 3x2 image, 12-bit: rows are ceil(36/8)=5 bytes.
    let vals = [[0x123u16, 0xABC, 0x0F0], [0xFFF, 0x001, 0x800]];
    let mut data = Vec::new();
    for r in vals {
        let bits: u64 = r.iter().fold(0, |a, &v| (a << 12) | v as u64) << 4; // 36 bits -> pad to 40
        data.extend(&bits.to_be_bytes()[3..]);
    }
    for le in [true, false] {
        let off = 8 + 2 + 11 * 12 + 4;
        let f = tiff(
            le,
            &raw_tags(3, 2, 12, 1, data.len() as u32, off as u32),
            &data,
        );
        let d = ingest_dng(&f).unwrap();
        assert_eq!(
            d.samples,
            [0x123, 0xABC, 0x0F0, 0xFFF, 0x001, 0x800],
            "le={le}"
        );
        assert_eq!((d.bit_depth, d.declared_bits), (12, 12));
        assert_eq!(d.dng_compression, "NONE");
    }
    // 16-bit big-endian
    let data16: Vec<u8> = [1u16, 513, 65535, 7]
        .iter()
        .flat_map(|v| v.to_be_bytes())
        .collect();
    let off = 8 + 2 + 11 * 12 + 4;
    let d = ingest_dng(&tiff(false, &raw_tags(2, 2, 16, 1, 8, off as u32), &data16)).unwrap();
    assert_eq!(d.samples, [1, 513, 65535, 7]);
    assert_eq!(d.bit_depth, 16);
}

/// Q: does an unsupported compression produce a message naming it, instead of garbage or a panic?
#[test]
fn unsupported_variants_are_named() {
    let off = (8 + 2 + 11 * 12 + 4) as u32;
    let e = ingest_dng(&tiff(true, &raw_tags(2, 2, 16, 8, 8, off), &[0; 8]))
        .err()
        .expect("should fail");
    assert!(e.contains("compression 8") && e.contains("DEFLATE"), "{e}");
    let mut t = raw_tags(2, 2, 16, 1, 8, off);
    t.iter_mut().find(|x| x.0 == 258).unwrap().2 = vec![32];
    let e = ingest_dng(&tiff(true, &t, &[0; 16]))
        .err()
        .expect("should fail");
    assert!(e.contains("unsupported DNG: sample type"), "{e}");
    let e = ingest_dng(b"II+\0\x08\0\x08\0").err().expect("should fail");
    assert!(e.contains("classic TIFF"), "{e}");
    assert!(ingest_dng(b"").is_err());
}

/// Q: can a damaged file make the reader panic or hang? Truncate a real lossless-JPEG DNG at many points.
#[test]
fn truncated_real_dng_errors_cleanly() {
    let dir = std::env::var("KON_TEST_DNG_DIR").expect("set KON_TEST_DNG_DIR");
    let bytes = std::fs::read(std::path::Path::new(&dir).join("Nikon__D80__DSC_0143.dng")).unwrap();
    for frac in [1, 7, 50, 333, 500, 777, 990, 999] {
        let cut = bytes.len() * frac / 1000;
        assert!(
            ingest_dng(&bytes[..cut]).is_err(),
            "cut at {cut} should fail"
        );
    }
    // Corrupt entropy data in place: must not panic (Ok or Err both acceptable; samples would simply be wrong, which
    // verification at the engine level would catch against the *same* reader, so this only guards against crashes).
    let mut bad = bytes.clone();
    for i in (bytes.len() / 2..bytes.len() / 2 + 4000).step_by(7) {
        bad[i] ^= 0x5A;
    }
    let _ = ingest_dng(&bad);
}
