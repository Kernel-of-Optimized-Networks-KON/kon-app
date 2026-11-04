//! Native, in-process DNG ingestion. Replaces PKON's external Python/tifffile step (`pkon::dng::ingest_dng`) for KON.
//!
//! Produces the same `pkon::dng::DngInput` the Python path produced: the stored raster of the main raw IFD as `u16`
//! samples (no crop, black-level subtraction, white balance, scaling or demosaic) plus the CFA metadata LISA/ULISA need.
//! Selection and metadata rules mirror `pkon/tools/lisa_dng.py` + `lisa_dng_extract.py`; the tests compare the two
//! implementations sample for sample on real files.
//!
//! Supported: classic little/big-endian TIFF, strips or tiles, single-plane CFA or monochrome, 1..16 bits per sample,
//! compression 1 (uncompressed, byte-aligned or bit-packed) and 7 (lossless JPEG, any component count / predictor).
//! Everything else is refused with an explicit message.
use pkon::dng::DngInput;
use pkon::lisa::{CfaPattern, ColorFilter, ImageMetadata, SampleLayout};
use std::collections::HashMap;
use std::rc::Rc;

type R<T> = Result<T, String>;

// ---------------------------------------------------------------- TIFF structure

struct Tiff<'a> {
    d: &'a [u8],
    le: bool,
}

impl Tiff<'_> {
    fn u16(&self, o: usize) -> R<u16> {
        let b: [u8; 2] = self
            .d
            .get(o..o + 2)
            .ok_or("truncated file")?
            .try_into()
            .unwrap();
        Ok(if self.le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }
    fn u32(&self, o: usize) -> R<u32> {
        let b: [u8; 4] = self
            .d
            .get(o..o + 4)
            .ok_or("truncated file")?
            .try_into()
            .unwrap();
        Ok(if self.le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }
}

#[derive(Clone, Copy)]
struct Entry {
    typ: u16,
    count: usize,
    /// Absolute position of the value bytes.
    at: usize,
}

struct Ifd {
    tags: HashMap<u16, Entry>,
    subs: Vec<Ifd>,
}

fn type_size(t: u16) -> Option<usize> {
    Some(match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 => 8,
        _ => return None,
    })
}

fn read_ifd(t: &Tiff, off: usize, depth: u8) -> R<(Ifd, usize)> {
    let n = t.u16(off)? as usize;
    let mut tags = HashMap::new();
    for i in 0..n {
        let e = off + 2 + i * 12;
        let (tag, typ, count) = (t.u16(e)?, t.u16(e + 2)?, t.u32(e + 4)? as usize);
        let Some(sz) = type_size(typ) else { continue };
        let total = sz.checked_mul(count).ok_or("bad tag size")?;
        let at = if total <= 4 {
            e + 8
        } else {
            t.u32(e + 8)? as usize
        };
        if at.checked_add(total).is_none_or(|end| end > t.d.len()) {
            return Err(format!("tag {tag} points outside the file"));
        }
        tags.insert(tag, Entry { typ, count, at });
    }
    let next = t.u32(off + 2 + n * 12)? as usize;
    let mut ifd = Ifd { tags, subs: vec![] };
    if depth < 3 {
        if let Some(offs) = ifd.uints(t, 330) {
            for o in offs {
                if let Ok((sub, _)) = read_ifd(t, o as usize, depth + 1) {
                    ifd.subs.push(sub);
                }
            }
        }
    }
    Ok((ifd, next))
}

impl Ifd {
    fn uints(&self, t: &Tiff, tag: u16) -> Option<Vec<u64>> {
        let e = self.tags.get(&tag)?;
        let sz = type_size(e.typ)?;
        (0..e.count)
            .map(|i| {
                let o = e.at + i * sz;
                match e.typ {
                    1 | 7 => t.d.get(o).map(|&b| b as u64),
                    3 => t.u16(o).ok().map(u64::from),
                    4 => t.u32(o).ok().map(u64::from),
                    _ => None,
                }
            })
            .collect()
    }
    fn uint(&self, t: &Tiff, tag: u16) -> Option<u64> {
        self.uints(t, tag).and_then(|v| v.first().copied())
    }
    fn ascii(&self, t: &Tiff, tag: u16) -> Option<String> {
        let e = self.tags.get(&tag)?;
        let b = t.d.get(e.at..e.at + e.count)?;
        Some(
            String::from_utf8_lossy(b)
                .trim_end_matches('\0')
                .trim()
                .to_string(),
        )
    }
}

/// tifffile's page order: each top-level IFD followed by its SubIFDs.
fn all_pages(ifds: &[Ifd]) -> Vec<&Ifd> {
    let mut out = Vec::new();
    for i in ifds {
        out.push(i);
        out.extend(i.subs.iter());
    }
    out
}

fn compression_name(c: u64) -> String {
    match c {
        1 => "NONE".into(),
        7 => "JPEG".into(),
        8 | 32946 => "ADOBE_DEFLATE".into(),
        5 => "LZW".into(),
        32773 => "PACKBITS".into(),
        34892 => "JPEG_LOSSY".into(),
        52546 => "JPEGXL_DNG".into(),
        n => format!("COMPRESSION_{n}"),
    }
}

// ---------------------------------------------------------------- colour metadata (display only)

/// DNG colour/level tags needed to render a mosaic for display. Read-only: none of this touches the stored samples
/// and none of it is stored in `.pkon`, so it must come from the original DNG.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColorMeta {
    pub camera: String,
    pub as_shot_neutral: Option<[f64; 3]>,
    pub black_level: Option<Vec<f64>>,
    /// (rows, cols) of the BlackLevel repeat pattern.
    pub black_repeat: (usize, usize),
    pub white_level: Option<f64>,
    pub color_matrix1: Option<[f64; 9]>,
    pub color_matrix2: Option<[f64; 9]>,
    pub forward_matrix1: Option<[f64; 9]>,
    pub forward_matrix2: Option<[f64; 9]>,
    pub camera_calibration1: Option<[f64; 9]>,
    pub camera_calibration2: Option<[f64; 9]>,
    pub analog_balance: Option<[f64; 3]>,
    pub illuminant1: Option<u16>,
    pub illuminant2: Option<u16>,
    pub baseline_exposure: Option<f64>,
}

impl Ifd {
    fn floats(&self, t: &Tiff, tag: u16) -> Option<Vec<f64>> {
        let e = self.tags.get(&tag)?;
        let sz = type_size(e.typ)?;
        (0..e.count)
            .map(|i| {
                let o = e.at + i * sz;
                match e.typ {
                    1 | 7 => t.d.get(o).map(|&b| b as f64),
                    6 => t.d.get(o).map(|&b| b as i8 as f64),
                    3 => t.u16(o).ok().map(f64::from),
                    8 => t.u16(o).ok().map(|v| v as i16 as f64),
                    4 => t.u32(o).ok().map(f64::from),
                    9 => t.u32(o).ok().map(|v| v as i32 as f64),
                    11 => t.u32(o).ok().map(|v| f32::from_bits(v) as f64),
                    5 | 10 => {
                        let (n, d) = (t.u32(o).ok()?, t.u32(o + 4).ok()?);
                        if d == 0 {
                            return None;
                        }
                        let n = if e.typ == 10 { n as i32 as f64 } else { n as f64 };
                        let d = if e.typ == 10 { d as i32 as f64 } else { d as f64 };
                        Some(n / d)
                    }
                    _ => None,
                }
            })
            .collect()
    }
}

/// Read the DNG colour tags from the same raw directory `ingest_dng` selects (falling back to the first directory,
/// where DNG puts the colour tags). Missing tags stay `None`; nothing is invented.
pub fn color_metadata(d: &[u8]) -> R<ColorMeta> {
    let le = match d.get(..4) {
        Some(b"II*\0") => true,
        Some(b"MM\0*") => false,
        _ => return Err("not a classic TIFF/DNG header".into()),
    };
    let t = Tiff { d, le };
    let mut ifds = Vec::new();
    let mut off = t.u32(4)? as usize;
    while off != 0 && ifds.len() < 64 {
        let (ifd, next) = read_ifd(&t, off, 0)?;
        ifds.push(ifd);
        off = next;
    }
    let pages = all_pages(&ifds);
    let area = |p: &Ifd| p.uint(&t, 256).unwrap_or(0) * p.uint(&t, 257).unwrap_or(0);
    let raw: Vec<&Ifd> = pages.iter().copied().filter(|p| matches!(p.uint(&t, 262), Some(32803 | 34892))).collect();
    let cand = if raw.is_empty() { &pages } else { &raw };
    let p = cand.iter().copied().fold(None::<&Ifd>, |b, p| if b.is_none_or(|b| area(p) > area(b)) { Some(p) } else { b });
    let p = p.ok_or("no image directory found")?;
    let root = pages.first().ok_or("no image directory found")?;
    let find = |tag: u16| p.floats(&t, tag).or_else(|| root.floats(&t, tag));
    let nine = |tag: u16| find(tag).and_then(|v| <[f64; 9]>::try_from(v).ok());
    let three = |tag: u16| find(tag).and_then(|v| <[f64; 3]>::try_from(v).ok());
    let rep = find(50713).filter(|v| v.len() == 2 && v[0] >= 1.0 && v[1] >= 1.0).map(|v| (v[0] as usize, v[1] as usize)).unwrap_or((1, 1));
    let black = find(50714).filter(|v| v.len() == rep.0 * rep.1 || v.len() == 1);
    Ok(ColorMeta {
        camera: format!("{} {}", root.ascii(&t, 271).unwrap_or_default(), root.ascii(&t, 272).unwrap_or_default()).trim().to_string(),
        as_shot_neutral: three(50728),
        black_level: black,
        black_repeat: rep,
        white_level: find(50717).and_then(|v| v.first().copied()),
        color_matrix1: nine(50721),
        color_matrix2: nine(50722),
        forward_matrix1: nine(50964),
        forward_matrix2: nine(50965),
        camera_calibration1: nine(50723),
        camera_calibration2: nine(50724),
        analog_balance: three(50727),
        illuminant1: find(50778).and_then(|v| v.first().map(|&x| x as u16)),
        illuminant2: find(50779).and_then(|v| v.first().map(|&x| x as u16)),
        baseline_exposure: find(50730).and_then(|v| v.first().copied()),
    })
}

// ---------------------------------------------------------------- public entry

pub fn ingest_dng(bytes: &[u8]) -> R<DngInput> {
    ingest(bytes).map_err(|e| {
        if e.starts_with("unsupported DNG") {
            e
        } else {
            format!("cannot read DNG: {e}")
        }
    })
}

fn ingest(d: &[u8]) -> R<DngInput> {
    let le = match d.get(..4) {
        Some(b"II*\0") => true,
        Some(b"MM\0*") => false,
        _ => return Err("not a classic TIFF/DNG header".into()),
    };
    let t = Tiff { d, le };
    let mut ifds = Vec::new();
    let mut off = t.u32(4)? as usize;
    while off != 0 && ifds.len() < 64 {
        let (ifd, next) = read_ifd(&t, off, 0)?;
        ifds.push(ifd);
        off = next;
    }
    let pages = all_pages(&ifds);
    let area = |p: &Ifd| p.uint(&t, 256).unwrap_or(0) * p.uint(&t, 257).unwrap_or(0);
    let raw: Vec<&Ifd> = pages
        .iter()
        .copied()
        .filter(|p| matches!(p.uint(&t, 262), Some(32803 | 34892)))
        .collect();
    let cand = if raw.is_empty() { &pages } else { &raw };
    // First maximum, like Python's max().
    let p = cand.iter().copied().fold(None::<&Ifd>, |b, p| {
        if b.is_none_or(|b| area(p) > area(b)) {
            Some(p)
        } else {
            b
        }
    });
    let p = p.ok_or("no image directory found")?;
    let root = pages.first().ok_or("no image directory found")?;

    let (w, h) = (
        p.uint(&t, 256).ok_or("missing ImageWidth")? as usize,
        p.uint(&t, 257).ok_or("missing ImageLength")? as usize,
    );
    let spp = p.uint(&t, 277).unwrap_or(1) as usize;
    let bps_all = p.uints(&t, 258).unwrap_or_else(|| vec![1]);
    let comp = p.uint(&t, 259).unwrap_or(1);
    if spp != 1 {
        return Err(format!(
            "unsupported DNG: stored raster has shape [{h}, {w}, {spp}] (only single-plane CFA/monochrome RAW is supported)"
        ));
    }
    let bps = *bps_all.first().ok_or("missing BitsPerSample")? as usize;
    let sample_fmt = p.uints(&t, 339).unwrap_or_else(|| vec![1]);
    if bps_all.len() != 1 || !(1..=16).contains(&bps) || sample_fmt.iter().any(|&f| f != 1) {
        return Err(format!(
            "unsupported DNG: sample type ({bps_all:?} bits, format {sample_fmt:?}); only 1..16-bit unsigned integer samples are supported"
        ));
    }
    if p.uint(&t, 284).unwrap_or(1) != 1 {
        return Err("unsupported DNG: planar configuration other than chunky".into());
    }
    if p.uint(&t, 266).unwrap_or(1) != 1 {
        return Err("unsupported DNG: FillOrder other than 1".into());
    }
    if !matches!(comp, 1 | 7) {
        return Err(format!(
            "unsupported DNG: compression {} ({}) is not supported by KON's native DNG reader (supported: uncompressed, lossless JPEG)",
            comp,
            compression_name(comp)
        ));
    }
    if p.uint(&t, 317).unwrap_or(1) != 1 && comp == 1 {
        return Err("unsupported DNG: TIFF predictor on uncompressed data".into());
    }

    // Stored units: tiles or strips.
    let tiled = p.tags.contains_key(&322);
    let (uw, uh, offs, cnts) = if tiled {
        (
            p.uint(&t, 322).ok_or("missing TileWidth")? as usize,
            p.uint(&t, 323).ok_or("missing TileLength")? as usize,
            p.uints(&t, 324).ok_or("missing TileOffsets")?,
            p.uints(&t, 325).ok_or("missing TileByteCounts")?,
        )
    } else {
        (
            w,
            (p.uint(&t, 278).unwrap_or(h as u64) as usize).clamp(1, h),
            p.uints(&t, 273).ok_or("missing StripOffsets")?,
            p.uints(&t, 279).ok_or("missing StripByteCounts")?,
        )
    };
    if uw == 0 || uh == 0 || offs.len() != cnts.len() {
        return Err("inconsistent strip/tile tables".into());
    }
    let (across, down) = (w.div_ceil(uw), h.div_ceil(uh));
    if offs.len() < across * down {
        return Err("fewer strips/tiles than the image needs".into());
    }
    let n = w
        .checked_mul(h)
        .filter(|&n| n > 0 && n <= 1 << 28)
        .ok_or("image dimensions are zero or exceed KON's 2^28-sample limit")?;
    let mut samples = vec![0u16; n];
    let mut tables = HashMap::new();
    for ty in 0..down {
        for tx in 0..across {
            let i = ty * across + tx;
            let (o, c) = (offs[i] as usize, cnts[i] as usize);
            let data = d
                .get(o..o.checked_add(c).ok_or("bad offset")?)
                .ok_or("strip/tile data outside the file")?;
            // Strips are as tall as the rows they hold; tiles are always full size.
            let rows_here = if tiled { uh } else { uh.min(h - ty * uh) };
            let unit = if comp == 1 {
                unpack_plain(data, uw, rows_here, bps, le)?
            } else {
                let (u, jw, jh) = ljpeg::decode(data, &mut tables)?;
                // The JPEG frame may fold the tile differently (e.g. 2 components, half the rows); the decoded samples
                // are the tile in raster order either way, so only the total must agree.
                if jw * jh != uw * rows_here {
                    return Err(format!("lossless JPEG tile holds {jw}x{jh} samples, directory says {uw}x{rows_here}"));
                }
                u
            };
            let (x0, y0) = (tx * uw, ty * uh);
            for r in 0..rows_here.min(h.saturating_sub(y0)) {
                let n = uw.min(w - x0);
                samples[(y0 + r) * w + x0..][..n].copy_from_slice(&unit[r * uw..][..n]);
            }
        }
    }

    // Bit depth, exactly as lisa_dng_extract.py.
    let mx = samples.iter().copied().max().unwrap_or(0) as u32;
    let wl = p
        .uints(&t, 50717)
        .and_then(|v| v.into_iter().max())
        .unwrap_or(0) as u32;
    let bit_depth = (bps as u32).min((32 - mx.max(wl).leading_zeros()).max(1));
    if bit_depth < 16 && mx >= (1u32 << bit_depth) {
        return Err(format!(
            "unsupported DNG: sample value {mx} does not fit {bit_depth} bits"
        ));
    }
    let bit_depth = bit_depth as u8;

    // CFA metadata.
    let cfa = p.uints(&t, 33422).filter(|c| !c.is_empty());
    let metadata = match cfa {
        None => ImageMetadata::without_cfa(w, h, SampleLayout::Monochrome),
        Some(cfa) => {
            let dim = p
                .uints(&t, 33421)
                .ok_or("DNG has CFAPattern without CFARepeatPatternDim")?;
            if dim.len() != 2 {
                return Err("malformed CFA dimensions".into());
            }
            let aa = p
                .uints(&t, 50829)
                .filter(|a| a.len() == 4)
                .unwrap_or_else(|| vec![0, 0, h as u64, w as u64]);
            let org = [
                (aa[1] as i64).wrapping_neg().rem_euclid(dim[1] as i64) as usize,
                (aa[0] as i64).wrapping_neg().rem_euclid(dim[0] as i64) as usize,
            ];
            let entries = cfa
                .iter()
                .map(|&v| {
                    u8::try_from(v)
                        .ok()
                        .and_then(|v| ColorFilter::try_from(v).ok())
                        .ok_or(format!("unsupported CFA color code {v}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let pat = CfaPattern {
                width: dim[1] as usize,
                height: dim[0] as usize,
                entries,
                origin_x: org[0],
                origin_y: org[1],
            };
            let bayer = ImageMetadata::with_cfa(w, h, SampleLayout::BayerCfa, pat.clone());
            if dim == [2, 2] && bayer.validate().is_ok() {
                bayer
            } else {
                ImageMetadata::with_cfa(w, h, SampleLayout::OtherCfa, pat)
            }
        }
    };
    let make = root.ascii(&t, 271).unwrap_or_default();
    let model = root.ascii(&t, 272).unwrap_or_default();
    Ok(DngInput {
        samples,
        bit_depth,
        declared_bits: bps as u8,
        metadata,
        camera: format!("{make} {model}").trim().to_string(),
        dng_compression: compression_name(comp),
    })
}

/// Uncompressed samples: rows are byte-aligned, samples packed MSB-first (byte order only matters for 16-bit).
fn unpack_plain(data: &[u8], w: usize, rows: usize, bps: usize, le: bool) -> R<Vec<u16>> {
    let stride = (w * bps).div_ceil(8);
    if data.len() < stride * rows {
        return Err(format!(
            "uncompressed strip/tile has {} bytes, needs {}",
            data.len(),
            stride * rows
        ));
    }
    let mut out = Vec::with_capacity(w * rows);
    for r in 0..rows {
        let row = &data[r * stride..][..stride];
        match bps {
            8 => out.extend(row[..w].iter().map(|&b| b as u16)),
            16 => out.extend(row.chunks_exact(2).take(w).map(|c| {
                if le {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            })),
            _ => {
                let (mut acc, mut nbits, mut it) = (0u32, 0usize, row.iter());
                for _ in 0..w {
                    while nbits < bps {
                        acc = (acc << 8) | *it.next().unwrap_or(&0) as u32;
                        nbits += 8;
                    }
                    nbits -= bps;
                    out.push(((acc >> nbits) & ((1 << bps) - 1)) as u16);
                    acc &= (1 << nbits) - 1;
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- lossless JPEG (ITU T.81 process 14, SOF3)

mod ljpeg {
    use super::*;

    pub struct Huff {
        /// Indexed by the next 16 bits: (code length << 8) | symbol; 0 = invalid.
        lut: Vec<u16>,
    }

    fn build(counts: &[u8; 16], syms: &[u8]) -> R<Huff> {
        let mut lut = vec![0u16; 1 << 16];
        let (mut code, mut k) = (0u32, 0usize);
        for len in 1..=16usize {
            for _ in 0..counts[len - 1] {
                let sym = *syms.get(k).ok_or("bad Huffman table")?;
                k += 1;
                if code >= (1 << len) {
                    return Err("bad Huffman table".into());
                }
                let first = (code << (16 - len)) as usize;
                for e in &mut lut[first..first + (1 << (16 - len))] {
                    *e = ((len as u16) << 8) | sym as u16;
                }
                code += 1;
            }
            code <<= 1;
        }
        Ok(Huff { lut })
    }

    struct Bits<'a> {
        d: &'a [u8],
        pos: usize,
        acc: u64,
        n: u32,
    }
    impl Bits<'_> {
        fn fill(&mut self) {
            while self.n <= 56 {
                let b = self.d.get(self.pos).copied().unwrap_or(0);
                self.pos += 1;
                self.acc |= (b as u64) << (56 - self.n);
                self.n += 8;
            }
        }
        fn peek16(&mut self) -> u32 {
            self.fill();
            (self.acc >> 48) as u32
        }
        fn eat(&mut self, k: u32) {
            self.acc <<= k;
            self.n -= k;
        }
        fn get(&mut self, k: u32) -> u32 {
            self.fill();
            let v = (self.acc >> (64 - k)) as u32;
            self.eat(k);
            v
        }
        /// Reading past the real data (zero padding) means the stream was truncated.
        fn overrun(&self) -> bool {
            self.pos > self.d.len() + 8
        }
    }

    /// Returns (samples in MCU-major order = row-major with components interleaved, width in samples, height).
    pub fn decode(d: &[u8], cache: &mut HashMap<Vec<u8>, Rc<Huff>>) -> R<(Vec<u16>, usize, usize)> {
        if d.get(..2) != Some(&[0xFF, 0xD8]) {
            return Err("not a JPEG stream (lossy-JPEG-only DNG variants are unsupported)".into());
        }
        let mut p = 2;
        let mut tabs: [Option<Rc<Huff>>; 4] = Default::default();
        let (mut prec, mut fh, mut fw, mut nc) = (0u32, 0usize, 0usize, 0usize);
        let mut ri = 0usize;
        loop {
            if d.get(p) != Some(&0xFF) {
                return Err("corrupt JPEG marker".into());
            }
            while d.get(p + 1) == Some(&0xFF) {
                p += 1;
            }
            let m = *d.get(p + 1).ok_or("truncated JPEG")?;
            let len = ((*d.get(p + 2).ok_or("truncated JPEG")? as usize) << 8)
                | *d.get(p + 3).ok_or("truncated JPEG")? as usize;
            let seg = d.get(p + 4..p + 2 + len).ok_or("truncated JPEG segment")?;
            p += 2 + len;
            match m {
                0xC4 => {
                    let mut s = seg;
                    while s.len() >= 17 {
                        let (id, counts) = (s[0], <[u8; 16]>::try_from(&s[1..17]).unwrap());
                        let nsym: usize = counts.iter().map(|&c| c as usize).sum();
                        let syms = s.get(17..17 + nsym).ok_or("bad DHT")?;
                        let key = s[..17 + nsym].to_vec();
                        let tab = match cache.get(&key) {
                            Some(t) => t.clone(),
                            None => {
                                let t = Rc::new(build(&counts, syms)?);
                                cache.insert(key, t.clone());
                                t
                            }
                        };
                        if id >> 4 != 0 || (id & 15) > 3 {
                            return Err("unsupported Huffman table class".into());
                        }
                        tabs[(id & 15) as usize] = Some(tab);
                        s = &s[17 + nsym..];
                    }
                }
                0xC3 => {
                    if seg.len() < 6 {
                        return Err("bad SOF3".into());
                    }
                    prec = seg[0] as u32;
                    fh = ((seg[1] as usize) << 8) | seg[2] as usize;
                    fw = ((seg[3] as usize) << 8) | seg[4] as usize;
                    nc = seg[5] as usize;
                    if !(2..=16).contains(&prec) || fh == 0 || fw == 0 || !(1..=4).contains(&nc) {
                        return Err("unsupported lossless JPEG frame".into());
                    }
                }
                0xC0..=0xCF if m != 0xC8 && m != 0xCC => {
                    return Err(format!(
                        "JPEG process SOF{} is not lossless JPEG (unsupported)",
                        m - 0xC0
                    ));
                }
                0xDD if seg.len() >= 2 => ri = ((seg[0] as usize) << 8) | seg[1] as usize,
                0xDA => {
                    if nc == 0 || seg.len() < 4 + 2 * nc || seg[0] as usize != nc {
                        return Err(
                            "only single-scan interleaved lossless JPEG is supported".into()
                        );
                    }
                    let sel: Vec<usize> = (0..nc).map(|i| (seg[2 + 2 * i] >> 4) as usize).collect();
                    let (pred, pt) = (seg[1 + 2 * nc] as u32, (seg[3 + 2 * nc] & 15) as u32);
                    if !(1..=7).contains(&pred) || pt >= prec {
                        return Err("unsupported lossless predictor".into());
                    }
                    let huff: Vec<Rc<Huff>> = sel
                        .iter()
                        .map(|&s| {
                            tabs.get(s)
                                .and_then(|t| t.clone())
                                .ok_or("missing Huffman table")
                        })
                        .collect::<Result<_, _>>()?;
                    return scan(&d[p..], &huff, nc, fw, fh, prec, pred, pt, ri);
                }
                _ => {}
            }
        }
    }

    /// Split entropy data into restart segments with byte stuffing removed.
    fn segments(d: &[u8]) -> Vec<Vec<u8>> {
        let (mut segs, mut cur, mut i) = (Vec::new(), Vec::new(), 0);
        while i < d.len() {
            if d[i] == 0xFF {
                match d.get(i + 1) {
                    Some(0x00) => {
                        cur.push(0xFF);
                        i += 2;
                    }
                    Some(0xFF) => i += 1,
                    Some(0xD0..=0xD7) => {
                        segs.push(std::mem::take(&mut cur));
                        i += 2;
                    }
                    _ => break, // EOI or other marker
                }
            } else {
                cur.push(d[i]);
                i += 1;
            }
        }
        segs.push(cur);
        segs
    }

    #[allow(clippy::too_many_arguments)]
    fn scan(
        d: &[u8],
        huff: &[Rc<Huff>],
        nc: usize,
        w: usize,
        h: usize,
        prec: u32,
        pred: u32,
        pt: u32,
        ri: usize,
    ) -> R<(Vec<u16>, usize, usize)> {
        let segs = segments(d);
        let total = w
            .checked_mul(h)
            .and_then(|n| n.checked_mul(nc))
            .ok_or("dimensions overflow")?;
        let mut out = vec![0u16; total];
        let init = 1i32 << (prec - pt - 1);
        let mut seg_i = 0;
        let mut bits = Bits {
            d: segs.first().map_or(&[], |s| s),
            pos: 0,
            acc: 0,
            n: 0,
        };
        let mut interval_start = 0usize;
        for y in 0..h {
            for x in 0..w {
                let m = y * w + x;
                if ri > 0 && m > 0 && m % ri == 0 {
                    seg_i += 1;
                    bits = Bits {
                        d: segs.get(seg_i).map_or(&[], |s| s),
                        pos: 0,
                        acc: 0,
                        n: 0,
                    };
                    interval_start = m;
                }
                for c in 0..nc {
                    let idx = m * nc + c;
                    let a = || out[idx - nc] as i32;
                    let b = || out[idx - w * nc] as i32;
                    let cc = || out[idx - w * nc - nc] as i32;
                    let px = if m == interval_start {
                        init
                    } else if m < interval_start + w {
                        a()
                    } else if x == 0 {
                        b()
                    } else {
                        let (ra, rb, rc) = (a(), b(), cc());
                        match pred {
                            1 => ra,
                            2 => rb,
                            3 => rc,
                            4 => ra + rb - rc,
                            5 => ra + ((rb - rc) >> 1),
                            6 => rb + ((ra - rc) >> 1),
                            _ => (ra + rb) >> 1,
                        }
                    };
                    let e = huff[c].lut[bits.peek16() as usize];
                    if e == 0 {
                        return Err("corrupt lossless JPEG (bad Huffman code)".into());
                    }
                    bits.eat((e >> 8) as u32);
                    let s = (e & 0xFF) as u32;
                    if s > 16 {
                        return Err("corrupt lossless JPEG (bad difference category)".into());
                    }
                    let diff = match s {
                        0 => 0,
                        16 => 32768,
                        _ => {
                            let v = bits.get(s) as i32;
                            if v < (1 << (s - 1)) {
                                v - ((1 << s) - 1)
                            } else {
                                v
                            }
                        }
                    };
                    out[idx] = (((px + diff) as u32 & 0xFFFF) << pt) as u16;
                }
            }
            if bits.overrun() {
                return Err("lossless JPEG data is truncated".into());
            }
        }
        Ok((out, w * nc, h))
    }
}
