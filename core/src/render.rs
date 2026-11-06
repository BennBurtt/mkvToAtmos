//! Simple 7.1.4 renderer for Atmos beds and objects.
//!
//! Cartesian, per-layer "dual balance" panning in the spirit of the Dolby home renderer:
//! the object is split between the floor and the ceiling layer by its height, then between
//! the two nearest speaker rows, then between the two nearest speakers of each row, each
//! time with a constant-power sine/cosine law. Object size spreads energy over the layer.
//! It is a fallback: the ADM file keeps the objects so Virtuoso or Pro Tools can render them.

use crate::damf::{Element, Keyframe, Kind};
use std::f64::consts::FRAC_PI_2;

/// Output channel order (Dolby / BS.2094 AP_00010017):
/// L R C LFE Lss Rss Lrs Rrs Ltf Rtf Ltr Rtr
pub const CHANNELS: usize = 12;
const L: usize = 0;
const R: usize = 1;
const C: usize = 2;
const LFE: usize = 3;
const LSS: usize = 4;
const RSS: usize = 5;
const LRS: usize = 6;
const RRS: usize = 7;
const LTF: usize = 8;
const RTF: usize = 9;
const LTR: usize = 10;
const RTR: usize = 11;

const FLOOR: [usize; 7] = [L, R, C, LSS, RSS, LRS, RRS];
const TOP: [usize; 4] = [LTF, RTF, LTR, RTR];

struct Row {
    y: f64,
    speakers: &'static [(f64, usize)],
}

const FLOOR_ROWS: [Row; 3] = [
    Row { y: 1.0, speakers: &[(-1.0, L), (0.0, C), (1.0, R)] },
    Row { y: 0.0, speakers: &[(-1.0, LSS), (1.0, RSS)] },
    Row { y: -1.0, speakers: &[(-1.0, LRS), (1.0, RRS)] },
];
const TOP_ROWS: [Row; 2] = [
    Row { y: 1.0, speakers: &[(-1.0, LTF), (1.0, RTF)] },
    Row { y: -1.0, speakers: &[(-1.0, LTR), (1.0, RTR)] },
];

fn bracket(v: f64, a: f64, b: f64) -> (f64, f64) {
    // constant-power split between a (at v==a) and b (at v==b)
    let t = if (b - a).abs() < 1e-12 { 0.0 } else { ((v - a) / (b - a)).clamp(0.0, 1.0) };
    ((t * FRAC_PI_2).cos(), (t * FRAC_PI_2).sin())
}

fn pan_row(row: &Row, x: f64, w: f64, g: &mut [f64; CHANNELS]) {
    let sp = row.speakers;
    let x = x.clamp(sp[0].0, sp[sp.len() - 1].0);
    for i in 0..sp.len() - 1 {
        let (xa, ca) = sp[i];
        let (xb, cb) = sp[i + 1];
        if x >= xa && x <= xb {
            let (ga, gb) = bracket(x, xa, xb);
            g[ca] += w * ga;
            g[cb] += w * gb;
            return;
        }
    }
}

fn pan_layer(rows: &[Row], x: f64, y: f64, w: f64, g: &mut [f64; CHANNELS]) {
    let y = y.clamp(rows[rows.len() - 1].y, rows[0].y);
    for i in 0..rows.len() - 1 {
        let (ya, yb) = (rows[i].y, rows[i + 1].y);
        if y <= ya && y >= yb {
            let (ga, gb) = bracket(y, ya, yb);
            pan_row(&rows[i], x, w * ga, g);
            pan_row(&rows[i + 1], x, w * gb, g);
            return;
        }
    }
}

/// Speaker gains (power-normalised) for an object at `pos` (DAMF coordinates) with `size`.
pub fn object_gains(pos: [f64; 3], size: f64) -> [f64; CHANNELS] {
    let mut g = [0.0; CHANNELS];
    let z = pos[2].clamp(0.0, 1.0);
    let (gf, gt) = bracket(z, 0.0, 1.0);
    if gf > 1e-9 {
        pan_layer(&FLOOR_ROWS, pos[0], pos[1], gf, &mut g);
    }
    if gt > 1e-9 {
        pan_layer(&TOP_ROWS, pos[0], pos[1], gt, &mut g);
    }
    let s = size.clamp(0.0, 1.0);
    if s > 0.0 {
        // Blend the energy towards an even spread over the speakers of the active layers.
        let floor_e: f64 = FLOOR.iter().map(|&c| g[c] * g[c]).sum();
        let top_e: f64 = TOP.iter().map(|&c| g[c] * g[c]).sum();
        for &c in &FLOOR {
            g[c] = ((1.0 - s) * g[c] * g[c] + s * floor_e / FLOOR.len() as f64).sqrt();
        }
        for &c in &TOP {
            g[c] = ((1.0 - s) * g[c] * g[c] + s * top_e / TOP.len() as f64).sqrt();
        }
    }
    g
}

/// Static gains for a bed channel.
pub fn bed_gains(label: &str) -> [f64; CHANNELS] {
    let mut g = [0.0; CHANNELS];
    let h = std::f64::consts::FRAC_1_SQRT_2;
    match label {
        "L" => g[L] = 1.0,
        "R" => g[R] = 1.0,
        "C" => g[C] = 1.0,
        "LFE" | "LFE2" => g[LFE] = 1.0,
        "Lss" => g[LSS] = 1.0,
        "Rss" => g[RSS] = 1.0,
        "Lrs" => g[LRS] = 1.0,
        "Rrs" => g[RRS] = 1.0,
        "Lts" => {
            g[LTF] = h;
            g[LTR] = h;
        }
        "Rts" => {
            g[RTF] = h;
            g[RTR] = h;
        }
        "Lfh" => g[LTF] = 1.0,
        "Rfh" => g[RTF] = 1.0,
        "Lrh" => g[LTR] = 1.0,
        "Rrh" => g[RTR] = 1.0,
        "Lw" => return object_gains([-1.0, 0.5, 0.0], 0.0),
        "Rw" => return object_gains([1.0, 0.5, 0.0], 0.0),
        "Lc" => return object_gains([-0.5, 1.0, 0.0], 0.0),
        "Rc" => return object_gains([0.5, 1.0, 0.0], 0.0),
        "Cs" => {
            g[LRS] = h;
            g[RRS] = h;
        }
        "Tfc" => {
            g[LTF] = h;
            g[RTF] = h;
        }
        "Tbc" => {
            g[LTR] = h;
            g[RTR] = h;
        }
        "Tc" => {
            for c in TOP {
                g[c] = 0.5;
            }
        }
        _ => g[C] = 1.0,
    }
    g
}

/// Interpolated state of one element at absolute sample `t`.
fn state_at(kfs: &[Keyframe], cursor: &mut usize, t: u64) -> ([f64; 3], f64, f64) {
    while *cursor + 1 < kfs.len() && kfs[*cursor + 1].sample_pos <= t {
        *cursor += 1;
    }
    let k = kfs[*cursor];
    if *cursor > 0 && k.ramp > 0 && t < k.sample_pos + k.ramp as u64 {
        let p = kfs[*cursor - 1];
        let a = (t - k.sample_pos) as f64 / k.ramp as f64;
        let lerp = |x: f64, y: f64| x + (y - x) * a;
        (
            [lerp(p.pos[0], k.pos[0]), lerp(p.pos[1], k.pos[1]), lerp(p.pos[2], k.pos[2])],
            lerp(p.gain, k.gain),
            lerp(p.size, k.size),
        )
    } else {
        (k.pos, k.gain, k.size)
    }
}

pub struct Renderer<'a> {
    elements: &'a [Element],
    keyframes: &'a [Vec<Keyframe>],
    cursors: Vec<usize>,
    /// Gains at the start of the current block, per element.
    prev: Vec<[f64; CHANNELS]>,
    started: bool,
}

pub const BLOCK: usize = 64;

impl<'a> Renderer<'a> {
    pub fn new(elements: &'a [Element], keyframes: &'a [Vec<Keyframe>]) -> Self {
        Self {
            elements,
            keyframes,
            cursors: vec![0; elements.len()],
            prev: vec![[0.0; CHANNELS]; elements.len()],
            started: false,
        }
    }

    fn gains(&mut self, i: usize, t: u64) -> [f64; CHANNELS] {
        let e = &self.elements[i];
        let (pos, gain, size) = state_at(&self.keyframes[i], &mut self.cursors[i], t);
        let mut g = if e.kind == Kind::Bed { bed_gains(&e.label) } else { object_gains(pos, size) };
        let gain = if e.kind == Kind::Bed { gain.max(0.0) } else { gain };
        for v in g.iter_mut() {
            *v *= gain;
        }
        g
    }

    /// Renders `frames` interleaved input frames starting at absolute sample `t0` into `out`
    /// (interleaved 12 channels).
    pub fn process(&mut self, input: &[f32], frames: usize, t0: u64, out: &mut Vec<f32>) {
        let nin = self.elements.len();
        out.clear();
        out.resize(frames * CHANNELS, 0.0);
        if !self.started {
            for i in 0..nin {
                self.prev[i] = self.gains(i, t0);
            }
            self.started = true;
        }
        let mut f = 0;
        while f < frames {
            let n = BLOCK.min(frames - f);
            let t_end = t0 + (f + n) as u64;
            for i in 0..nin {
                let g0 = self.prev[i];
                let g1 = self.gains(i, t_end);
                let mut active = false;
                for c in 0..CHANNELS {
                    if g0[c] != 0.0 || g1[c] != 0.0 {
                        active = true;
                        break;
                    }
                }
                if active {
                    for s in 0..n {
                        let a = (s + 1) as f64 / n as f64;
                        let x = input[(f + s) * nin + i] as f64;
                        if x == 0.0 {
                            continue;
                        }
                        let o = &mut out[(f + s) * CHANNELS..(f + s + 1) * CHANNELS];
                        for c in 0..CHANNELS {
                            let g = g0[c] + (g1[c] - g0[c]) * a;
                            if g != 0.0 {
                                o[c] += (x * g) as f32;
                            }
                        }
                    }
                }
                self.prev[i] = g1;
            }
            f += n;
        }
    }
}
