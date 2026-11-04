//! The avatar the camera bubble shows when the camera is off and no photo is chosen: ink and
//! silver folding through each other like marbled paper, always drifting, and pulsing with
//! each syllable while you talk. It's a grid of square cells in four of the icon's tones, dithered only where
//! one tone meets the next, and cheap enough to recompute off the main thread every frame.

/// Points per cell.
pub const CELL: f32 = 3.;

/// One frame: the lit cells, as runs along each row. Everything else is ink.
pub struct Frame {
    /// Cells across (and down) the circle.
    pub cells: usize,
    /// `(row, first, end, tone)`, with `end` exclusive and `tone` from 1 (dark) to 3 (silver).
    pub runs: Vec<(u16, u16, u16, u8)>,
}

/// `t` is the clock in seconds, which the caller runs a little faster while you talk; `pulse`
/// follows each syllable, from 0 (silent) to 1 (loud).
pub fn frame(cells: usize, t: f32, pulse: f32) -> Frame {
    let mut runs = Vec::new();
    for row in 0..cells {
        let y = 1. - 2. * (row as f32 + 0.5) / cells as f32;
        let mut run: Option<(usize, u8)> = None;
        for col in 0..=cells {
            let tone = if col < cells {
                let x = 2. * (col as f32 + 0.5) / cells as f32 - 1.;
                if x * x + y * y <= 1. {
                    let s = marbled(x, y, t, pulse) * 3.;
                    let lo = s.floor();
                    (if s - lo > bayer4(col, row) {
                        lo + 1.
                    } else {
                        lo
                    }) as u8
                } else {
                    0
                }
            } else {
                0
            };
            if run.map(|(_, current)| current) != Some(tone) {
                if let Some((first, current)) = run.take()
                    && current > 0
                {
                    runs.push((row as u16, first as u16, col as u16, current));
                }
                run = Some((col, tone));
            }
        }
    }
    Frame { cells, runs }
}

/// The light at a point of the orb, from 0 (ink) to 1 (silver). `x` and `y` are in -1..1, y up.
/// The orb is stirred around its centre, then folded by noise that's warped by more noise. The
/// shape never depends on the voice; each syllable's pulse swells it out from the middle, lifts
/// its tones and lights up the seams.
fn marbled(x: f32, y: f32, t: f32, pulse: f32) -> f32 {
    let r2 = x * x + y * y;
    let stir = 0.6 * simplex(t * 0.08, 2., 5.) * (1. - r2);
    let (sin, cos) = stir.sin_cos();
    let zoom = 0.55 * (1. - 0.16 * pulse * (1. - 0.5 * r2));
    let (px, py) = ((cos * x - sin * y) * zoom, (sin * x + cos * y) * zoom);
    let qx = broad(px, py, t * 0.09);
    let qy = broad(px + 5.2, py + 1.3, t * 0.09 + 3.);
    let rx = broad(px + 1.1 * qx + 1.7, py + 1.1 * qy + 9.2, t * 0.12);
    let ry = broad(px + 1.1 * qx + 8.3, py + 1.1 * qy + 2.8, t * 0.12 + 7.);
    let f = broad(px + 0.6 * rx, py + 0.6 * ry, t * 0.15);
    let seam = (-((f - 0.08) / (0.08 + 0.04 * pulse)).powi(2)).exp() * (0.12 + 0.75 * pulse);
    (0.62 + 0.55 * f + 0.12 * pulse + 0.3 * seam).clamp(0., 1.)
}

/// Two octaves of noise: broad shapes with a little detail.
fn broad(x: f32, y: f32, z: f32) -> f32 {
    0.65 * simplex(x, y, z) + 0.25 * simplex(x * 1.9 + 1.7, y * 1.9 + 1.7, z * 1.9 + 1.7)
}

/// The 4×4 ordered-dither threshold for a cell, in (0, 1).
fn bayer4(x: usize, y: usize) -> f32 {
    const MATRIX: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    (f32::from(MATRIX[y & 3][x & 3]) + 0.5) / 16.
}

/// 3D simplex noise in roughly -1..1 (Gustavson's formulation, hashed gradients).
fn simplex(x: f32, y: f32, z: f32) -> f32 {
    const F3: f32 = 1. / 3.;
    const G3: f32 = 1. / 6.;
    let s = (x + y + z) * F3;
    let (i, j, k) = ((x + s).floor(), (y + s).floor(), (z + s).floor());
    let t = (i + j + k) * G3;
    let (x0, y0, z0) = (x - (i - t), y - (j - t), z - (k - t));
    let (i1, j1, k1, i2, j2, k2) = if x0 >= y0 {
        if y0 >= z0 {
            (1., 0., 0., 1., 1., 0.)
        } else if x0 >= z0 {
            (1., 0., 0., 1., 0., 1.)
        } else {
            (0., 0., 1., 1., 0., 1.)
        }
    } else if y0 < z0 {
        (0., 0., 1., 0., 1., 1.)
    } else if x0 < z0 {
        (0., 1., 0., 0., 1., 1.)
    } else {
        (0., 1., 0., 1., 1., 0.)
    };
    let corners = [
        (0., 0., 0., x0, y0, z0),
        (i1, j1, k1, x0 - i1 + G3, y0 - j1 + G3, z0 - k1 + G3),
        (
            i2,
            j2,
            k2,
            x0 - i2 + 2. * G3,
            y0 - j2 + 2. * G3,
            z0 - k2 + 2. * G3,
        ),
        (
            1.,
            1.,
            1.,
            x0 - 1. + 3. * G3,
            y0 - 1. + 3. * G3,
            z0 - 1. + 3. * G3,
        ),
    ];
    let mut n = 0.;
    for (di, dj, dk, cx, cy, cz) in corners {
        let falloff = 0.6 - cx * cx - cy * cy - cz * cz;
        if falloff > 0. {
            let (gx, gy, gz) = gradient(i + di, j + dj, k + dk);
            n += falloff.powi(4) * (gx * cx + gy * cy + gz * cz);
        }
    }
    32. * n
}

/// One of the twelve cube-edge gradients, chosen by hashing the lattice point.
fn gradient(i: f32, j: f32, k: f32) -> (f32, f32, f32) {
    const GRADIENTS: [(f32, f32, f32); 12] = [
        (1., 1., 0.),
        (-1., 1., 0.),
        (1., -1., 0.),
        (-1., -1., 0.),
        (1., 0., 1.),
        (-1., 0., 1.),
        (1., 0., -1.),
        (-1., 0., -1.),
        (0., 1., 1.),
        (0., -1., 1.),
        (0., 1., -1.),
        (0., -1., -1.),
    ];
    let mut h = (i as i32 as u32).wrapping_mul(0x8da6_b343)
        ^ (j as i32 as u32).wrapping_mul(0xd816_3841)
        ^ (k as i32 as u32).wrapping_mul(0xcb1a_b31f);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    GRADIENTS[(h % 12) as usize]
}
