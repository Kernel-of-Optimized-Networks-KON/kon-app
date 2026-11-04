//! RAW development for display and PNG export only. It reads decoded Bayer samples and never modifies them; nothing
//! here is part of the codec, the `.pkon` format or lossless verification.
//!
//! Pipeline (DNG specification, "Mapping Camera Color Space to CIE XYZ Space"):
//! black/white level -> as-shot white balance -> highlight clip -> Malvar-He-Cutler demosaic ->
//! camera RGB -> XYZ(D50) via ForwardMatrix, or interpolated ColorMatrix + Bradford adaptation -> linear sRGB ->
//! baseline + automatic exposure -> luminance roll-off -> sRGB transfer curve.
//!
//! Camera data comes from the original DNG ([`ColorMeta`]). `.pkon` stores none of it, so without it the result is a
//! clearly labelled generic preview (gray-world balance, camera RGB treated as sRGB), never presented as camera colour.
use crate::dng::ColorMeta;
use pkon::canon::{Canonical, Layout, Spec};
use pkon::lisa::{CfaPattern, ColorFilter};

pub struct Rendered {
    pub image: Canonical,
    /// What was applied, shown to the user.
    pub description: String,
    /// True only when white balance and colour transform came from the DNG's own metadata.
    pub camera_aware: bool,
}

type M3 = [[f64; 3]; 3];

const I3: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
/// XYZ (D50) -> linear sRGB, Bradford-adapted (Lindbloom).
const XYZ_D50_TO_SRGB: M3 = [
    [3.1338561, -1.6168667, -0.4906146],
    [-0.9787684, 1.9161415, 0.0334540],
    [0.0719453, -0.2289914, 1.4052427],
];
const D50: [f64; 3] = [0.9642, 1.0, 0.8249];
const BRADFORD: M3 = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}
fn mulv(a: &M3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| (0..3).map(|k| a[i][k] * v[k]).sum())
}
fn diag(v: [f64; 3]) -> M3 {
    [[v[0], 0.0, 0.0], [0.0, v[1], 0.0], [0.0, 0.0, v[2]]]
}
fn inv(a: &M3) -> Result<M3, String> {
    let c = |i: usize, j: usize| {
        let (r0, r1, c0, c1) = ((i + 1) % 3, (i + 2) % 3, (j + 1) % 3, (j + 2) % 3);
        a[r0][c0] * a[r1][c1] - a[r0][c1] * a[r1][c0]
    };
    let det = a[0][0] * c(0, 0) + a[0][1] * c(0, 1) + a[0][2] * c(0, 2);
    if det.abs() < 1e-12 {
        return Err("a camera colour matrix in the DNG is singular".into());
    }
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = c(j, i) / det;
        }
    }
    Ok(r)
}
fn m9(v: &[f64; 9]) -> M3 {
    [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]]
}
/// g*a + (1-g)*b
fn lerp(a: &M3, b: &M3, g: f64) -> M3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = g * a[i][j] + (1.0 - g) * b[i][j];
        }
    }
    r
}

/// DNG CalibrationIlluminant code -> correlated colour temperature (K).
fn illuminant_cct(code: u16) -> Option<f64> {
    Some(match code {
        3 => 2850.0,  // tungsten
        17 => 2856.0, // standard light A
        18 => 4874.0, // B
        19 => 6774.0, // C
        20 => 5503.0, // D55
        21 => 6504.0, // D65
        22 => 7504.0, // D75
        23 => 5003.0, // D50
        24 => 3200.0, // ISO studio tungsten
        _ => return None,
    })
}
fn xyz_cct(xyz: [f64; 3]) -> f64 {
    let s = xyz[0] + xyz[1] + xyz[2];
    let (x, y) = (xyz[0] / s, xyz[1] / s);
    let n = (x - 0.3320) / (0.1858 - y);
    (449.0 * n * n * n + 3525.0 * n * n + 6823.3 * n + 5520.33).clamp(2000.0, 15000.0)
}

/// Everything derived from the metadata for one image.
struct Setup {
    /// Per-channel gain applied to normalised samples before demosaicing (green = 1).
    gains: [f32; 3],
    /// Balanced camera RGB -> linear sRGB.
    after: [[f32; 3]; 3],
    black: Vec<f32>,
    black_rep: (usize, usize),
    white: f32,
    baseline_ev: f64,
    camera_aware: bool,
    notes: Vec<String>,
}

/// Camera-aware setup, or the reason it is impossible.
fn camera_setup(m: &ColorMeta) -> Result<(Vec<String>, [f64; 3], M3, M3), String> {
    let n = m.as_shot_neutral.ok_or("the DNG has no AsShotNeutral white balance")?;
    if n.iter().any(|&v| !(v > 0.0)) {
        return Err("the DNG's AsShotNeutral is invalid".into());
    }
    let (cm1, cm2) = (m.color_matrix1.as_ref().map(m9), m.color_matrix2.as_ref().map(m9));
    let (cc1, cc2) = (m.camera_calibration1.as_ref().map(m9), m.camera_calibration2.as_ref().map(m9));
    let (fm1, fm2) = (m.forward_matrix1.as_ref().map(m9), m.forward_matrix2.as_ref().map(m9));
    let ab = diag(m.analog_balance.unwrap_or([1.0; 3]));
    if cm1.is_none() && cm2.is_none() {
        return Err("the DNG has no ColorMatrix1/2".into());
    }
    let (t1, t2) = (
        m.illuminant1.and_then(illuminant_cct),
        m.illuminant2.and_then(illuminant_cct),
    );
    let two = cm1.is_some() && cm2.is_some();
    if two && (t1.is_none() || t2.is_none() || t1 == t2) {
        return Err("the DNG's calibration illuminants are missing or unsupported".into());
    }
    // Weight of matrix 1 at a colour temperature (interpolation in inverse CCT, per the DNG spec).
    let weight = |cct: f64| {
        if !two {
            return 1.0;
        }
        let (t1, t2) = (t1.unwrap(), t2.unwrap());
        ((1.0 / cct - 1.0 / t2) / (1.0 / t1 - 1.0 / t2)).clamp(0.0, 1.0)
    };
    let pick = |a: &Option<M3>, b: &Option<M3>, g: f64, dflt: M3| match (a, b) {
        (Some(a), Some(b)) => lerp(a, b, g),
        (Some(a), None) => *a,
        (None, Some(b)) => *b,
        (None, None) => dflt,
    };
    let (cmx1, cmx2) = (cm1.or(cm2).unwrap(), cm2.or(cm1).unwrap());
    // Solve for the as-shot colour temperature: the white point depends on the matrix, which depends on the CCT.
    let mut cct = 5000.0;
    for _ in 0..40 {
        let g = weight(cct);
        let abcc = mul(&ab, &pick(&cc1, &cc2, g, I3));
        let cm = lerp(&cmx1, &cmx2, g);
        let xyz = mulv(&inv(&mul(&abcc, &cm))?, n);
        let next = xyz_cct(xyz);
        if (next - cct).abs() < 1.0 {
            cct = next;
            break;
        }
        cct = next;
    }
    let g = weight(cct);
    let abcc = mul(&ab, &pick(&cc1, &cc2, g, I3));
    let mut notes = vec![format!("as-shot white balance (AsShotNeutral), as-shot colour temperature ≈ {cct:.0} K")];
    let to_xyz_d50 = if fm1.is_some() || fm2.is_some() {
        let fm = pick(&fm1, &fm2, g, I3);
        let cw = mulv(&inv(&abcc)?, n);
        notes.push("ForwardMatrix".into());
        mul(&mul(&fm, &diag(cw.map(|v| 1.0 / v))), &inv(&abcc)?)
    } else {
        let cm = lerp(&cmx1, &cmx2, g);
        let native = inv(&mul(&abcc, &cm))?;
        let w = mulv(&native, n);
        let w = [w[0] / w[1], 1.0, w[2] / w[1]];
        let (src, dst) = (mulv(&BRADFORD, w), mulv(&BRADFORD, D50));
        let adapt = mul(&mul(&inv(&BRADFORD)?, &diag([dst[0] / src[0], dst[1] / src[1], dst[2] / src[2]])), &BRADFORD);
        notes.push(if two { "ColorMatrix1/2 interpolated by CCT, Bradford adaptation to D50".into() } else { "ColorMatrix, Bradford adaptation to D50".into() });
        mul(&adapt, &native)
    };
    let cw = mulv(&inv(&abcc)?, n);
    Ok((notes, [cw[0], cw[1], cw[2]], mul(&XYZ_D50_TO_SRGB, &to_xyz_d50), abcc))
}

fn build_setup(
    samples: &[u16],
    cfa_of: &dyn Fn(usize, usize) -> usize,
    w: usize,
    h: usize,
    bit_depth: u8,
    meta: Option<&ColorMeta>,
) -> Result<Setup, String> {
    let mut notes = Vec::new();
    let default_white = ((1u32 << bit_depth) - 1) as f32;
    let (black, black_rep, white) = match meta {
        Some(m) => {
            let rep = m.black_repeat;
            let black: Vec<f32> = match &m.black_level {
                Some(v) if v.len() == rep.0 * rep.1 => v.iter().map(|&x| x as f32).collect(),
                Some(v) if v.len() == 1 => vec![v[0] as f32; rep.0 * rep.1],
                _ => vec![0.0; rep.0 * rep.1],
            };
            notes.push(match &m.black_level {
                Some(v) => format!("black level {:?}", v.iter().map(|x| x.round() as i64).collect::<Vec<_>>()),
                None => "no BlackLevel in the DNG (0 assumed)".into(),
            });
            let white = m.white_level.map(|v| v as f32).unwrap_or(default_white);
            notes.push(match m.white_level {
                Some(v) => format!("white level {v}"),
                None => format!("no WhiteLevel in the DNG ({default_white} from bit depth)"),
            });
            (black, rep, white)
        }
        None => (vec![0.0], (1, 1), default_white),
    };
    if white <= black.iter().cloned().fold(0.0, f32::max) {
        return Err("malformed RAW metadata: white level is not above the black level".into());
    }
    let mut setup = Setup {
        gains: [1.0; 3],
        after: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        black,
        black_rep,
        white,
        baseline_ev: 0.0,
        camera_aware: false,
        notes,
    };
    let camera = meta.map(|m| camera_setup(m).map(|c| (c, m.baseline_exposure)));
    match camera {
        Some(Ok(((cam_notes, cw, after, _), be))) => {
            setup.gains = [(cw[1] / cw[0]) as f32, 1.0, (cw[1] / cw[2]) as f32];
            let gd = diag([cw[1] / cw[0], 1.0, cw[1] / cw[2]]);
            let a = mul(&after, &inv(&gd)?);
            setup.after = a.map(|r| r.map(|v| v as f32));
            setup.camera_aware = true;
            setup.baseline_ev = be.unwrap_or(0.0);
            setup.notes.splice(0..0, cam_notes);
            if let Some(b) = be {
                setup.notes.push(format!("BaselineExposure {b:+.2} EV"));
            }
        }
        other => {
            // Generic: gray-world balance over the black/white-normalised mosaic; camera RGB treated as sRGB.
            let reason = match other {
                Some(Err(e)) => format!("camera colour data unusable: {e}"),
                _ => "no camera colour data (a .pkon does not store it)".into(),
            };
            let (mut sum, mut cnt) = ([0f64; 3], [0u64; 3]);
            let step = 3;
            for y in (0..h).step_by(step) {
                for x in (0..w).step_by(step) {
                    let c = cfa_of(x, y);
                    let b = setup.black[(y % setup.black_rep.0) * setup.black_rep.1 + x % setup.black_rep.1];
                    sum[c] += ((samples[y * w + x] as f32 - b) / (white - b)).max(0.0) as f64;
                    cnt[c] += 1;
                }
            }
            if cnt.contains(&0) || sum[1] <= 0.0 || sum[0] <= 0.0 || sum[2] <= 0.0 {
                return Err("RAW mosaic has no usable red/green/blue samples; cannot render a preview".into());
            }
            let mean = |c: usize| sum[c] / cnt[c] as f64;
            setup.gains = [(mean(1) / mean(0)) as f32, 1.0, (mean(1) / mean(2)) as f32];
            setup.notes.push(format!("GENERIC preview: {reason}; gray-world white balance, camera RGB shown as sRGB"));
        }
    }
    Ok(setup)
}

fn reflect(i: i64, n: i64) -> usize {
    let mut i = i;
    if i < 0 {
        i = -i;
    }
    if i >= n {
        i = 2 * n - 2 - i;
    }
    i.clamp(0, n - 1) as usize
}

/// Malvar-He-Cutler gradient-corrected linear interpolation: the missing channels at (x, y).
struct Mosaic<'a> {
    m: &'a [f32],
    w: i64,
    h: i64,
    tab: &'a [u8],
    pw: usize,
    ph: usize,
    clip: f32,
}

impl Mosaic<'_> {
    #[inline]
    fn ch(&self, x: usize, y: usize) -> usize {
        self.tab[(y % self.ph) * self.pw + x % self.pw] as usize
    }
    #[inline]
    fn p(&self, x: usize, y: usize, dx: i64, dy: i64) -> f32 {
        self.m[reflect(y as i64 + dy, self.h) * self.w as usize + reflect(x as i64 + dx, self.w)]
    }
    fn rgb(&self, x: usize, y: usize) -> [f32; 3] {
        let c0 = self.ch(x, y);
        let p = |dx, dy| self.p(x, y, dx, dy);
        let v0 = p(0, 0);
        let mut out = [0f32; 3];
        out[c0] = v0;
        let cross4 = p(-1, 0) + p(1, 0) + p(0, -1) + p(0, 1);
        let diag4 = p(-1, -1) + p(1, -1) + p(-1, 1) + p(1, 1);
        let far_h = p(-2, 0) + p(2, 0);
        let far_v = p(0, -2) + p(0, 2);
        let green_at_rb = (4.0 * v0 + 2.0 * cross4 - far_h - far_v) / 8.0;
        for d in 0..3 {
            if d == c0 {
                continue;
            }
            out[d] = if d == 1 {
                green_at_rb
            } else if c0 == 1 {
                // On a green site: is the wanted colour left/right (same row) or above/below?
                if self.ch(x + 1, y) == d {
                    (5.0 * v0 + 4.0 * (p(-1, 0) + p(1, 0)) - diag4 - far_h + 0.5 * far_v) / 8.0
                } else {
                    (5.0 * v0 + 4.0 * (p(0, -1) + p(0, 1)) - diag4 - far_v + 0.5 * far_h) / 8.0
                }
            } else {
                // Red at blue site or blue at red site.
                (6.0 * v0 + 2.0 * diag4 - 1.5 * (far_h + far_v)) / 8.0
            };
            out[d] = out[d].clamp(0.0, self.clip);
        }
        out
    }
}

#[inline]
fn srgb_oetf(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let s = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0 + 0.5) as u8
}

/// Soft shoulder on luminance: identity below 0.6, smooth roll-off to 1.0.
#[inline]
fn shoulder(y: f32) -> f32 {
    if y <= 0.6 {
        y
    } else {
        0.6 + 0.4 * (1.0 - (-(y - 0.6) / 0.4).exp())
    }
}

#[inline]
fn finish(rgb: [f32; 3], after: &[[f32; 3]; 3], k: f32) -> [u8; 3] {
    let l = [0, 1, 2].map(|i| ((after[i][0] * rgb[0] + after[i][1] * rgb[1] + after[i][2] * rgb[2]) * k).max(0.0));
    let y = 0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2];
    let s = if y > 1e-6 { shoulder(y) / y } else { 1.0 };
    let c = l.map(|v| v * s);
    // Keep hue when a channel still exceeds 1: scale by the largest channel instead of clipping one.
    let m = c[0].max(c[1]).max(c[2]);
    let c = if m > 1.0 { c.map(|v| v / m) } else { c };
    c.map(srgb_oetf)
}

/// Develop a Bayer mosaic for display. `meta` is the original DNG's colour data when available.
pub fn develop(
    samples: &[u16],
    w: usize,
    h: usize,
    bit_depth: u8,
    cfa: &CfaPattern,
    meta: Option<&ColorMeta>,
) -> Result<Rendered, String> {
    if w < 3 || h < 3 || samples.len() != w * h {
        return Err("malformed RAW: sample count does not match the dimensions".into());
    }
    let idx = |c: ColorFilter| match c {
        ColorFilter::Red => 0u8,
        ColorFilter::Blue => 2,
        _ => 1,
    };
    let tab: Vec<u8> = (0..cfa.width * cfa.height).map(|i| idx(cfa.color_at(i % cfa.width, i / cfa.width))).collect();
    let (pw, ph) = (cfa.width, cfa.height);
    let cfa_of = |x: usize, y: usize| tab[(y % ph) * pw + x % pw] as usize;
    let setup = build_setup(samples, &cfa_of, w, h, bit_depth, meta)?;

    // Normalised, white-balanced mosaic, highlight-clipped at the level every channel can represent.
    let clip = setup.gains.iter().cloned().fold(f32::MAX, f32::min);
    let (br, bc) = setup.black_rep;
    let mut mosaic = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let b = setup.black[(y % br) * bc + x % bc];
            let v = ((samples[y * w + x] as f32 - b) / (setup.white - b)).max(0.0);
            mosaic[y * w + x] = (v * setup.gains[cfa_of(x, y)]).min(clip);
        }
    }
    let mos = Mosaic { m: &mosaic, w: w as i64, h: h as i64, tab: &tab, pw, ph, clip };

    // Automatic exposure from a sparse sample: lift under-exposed scenes so the 99.5th percentile sits near white.
    let base = (2f64.powf(setup.baseline_ev)) as f32;
    let mut ys = Vec::new();
    for y in (1..h - 1).step_by(7) {
        for x in (1..w - 1).step_by(7) {
            let c = mos.rgb(x, y);
            let a = &setup.after;
            let l = [0, 1, 2].map(|i| (a[i][0] * c[0] + a[i][1] * c[1] + a[i][2] * c[2]) * base);
            ys.push((0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2]).max(0.0));
        }
    }
    let n = ys.len();
    let p995 = *ys.select_nth_unstable_by((n * 995 / 1000).min(n - 1), |a, b| a.total_cmp(b)).1;
    let auto = (0.98 / p995.max(1e-6)).clamp(1.0, 16.0);
    let k = base * auto;

    let mut out = vec![0u8; w * h * 3];
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    let rows_per = h.div_ceil(threads);
    std::thread::scope(|s| {
        for (ci, chunk) in out.chunks_mut(rows_per * w * 3).enumerate() {
            let (mos, after) = (&mos, &setup.after);
            s.spawn(move || {
                for (r, row) in chunk.chunks_mut(w * 3).enumerate() {
                    let y = ci * rows_per + r;
                    for x in 0..w {
                        row[x * 3..x * 3 + 3].copy_from_slice(&finish(mos.rgb(x, y), after, k));
                    }
                }
            });
        }
    });

    let mut description = format!(
        "{} · {}Malvar–He–Cutler demosaic · exposure {:+.2} EV (auto) · highlight roll-off · sRGB",
        if setup.camera_aware { "Camera-aware" } else { "Generic preview" },
        setup.notes.iter().map(|n| format!("{n} · ")).collect::<String>(),
        (k as f64).log2()
    );
    if !setup.camera_aware {
        description.push_str(" — original DNG colour data not available");
    }
    let image = Canonical::new(Spec { width: w as u32, height: h as u32, layout: Layout::Rgb, bit_depth: 8 }, out)?;
    Ok(Rendered { image, description, camera_aware: setup.camera_aware })
}
