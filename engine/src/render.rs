//! The playfield: the original's 3D scene and camera, as real 3D with near-plane clipping, drawn
//! into buffers kept from frame to frame.

use sdl3::pixels::FColor;
use sdl3::render::{FPoint, Vertex};

use crate::pack::Rgb;
use crate::world::{self, Colours, PAL_FULL, SLOTS, Scene, Wall, WallKind, World, slot};

/// The original's desktop view: 768x480, seen through a 60 degree horizontal and 37.5 degree
/// vertical field of view (so its pixels aren't quite square).
const VIEW_W: f64 = 768.0;
const VIEW_H: f64 = 480.0;
const HALF_FOV_X: f64 = 30.0;
const HALF_FOV_Y: f64 = 18.75;
const CAM_DIST: f64 = 850.0;
const CENTRE_BORDER: f64 = 6.0;
/// Background wedges reach this far out, which is effectively infinity.
const FAR: f64 = 20000.0;
const NEAR: f64 = 1.0;
const MAX_SIDES: usize = world::MAX_SIDES as usize;
/// Wall distances are in fifths of a playfield unit.
const WALL_UNITS: f64 = 5.0;
/// The player: its distance out from the shape's edge, its size, and its shadow's depth.
const PLAYER_OUT: f64 = 14.0;
const PLAYER_SIZE: f64 = 8.0;
const PLAYER_SHADOW: f64 = -6.0;
/// In the ending, walls come out of the centre, starting this far out.
const ENDING_SPAWN: f64 = 700.0;

type V3 = [f64; 3];

struct Camera {
    cam: (f64, f64),
    xrot: (f64, f64),
    /// The title screen's view: pushed back and tipped away.
    title: Option<(f64, f64)>,
    /// The stage select's sideways lean.
    lean: Option<(f64, f64)>,
    depth: f64,
    cx: f64,
    cy: f64,
    px: f64,
    py: f64,
}

impl Camera {
    /// Playfield coordinates to view space (z away from the eye).
    fn view(&self, x: f64, y: f64, z: f64) -> V3 {
        let (s, c) = self.cam;
        let z1 = s * y + c * z;
        let y1 = y * c - z * s;
        let (s, c) = self.xrot;
        let (mut x, mut y, mut z) = (s * z1 + c * x, y1, z1 * c - x * s);
        if let Some((s, c)) = self.title {
            z += 1500.0;
            (y, z) = (y * c - z * s, s * y + c * z);
        }
        if let Some((s, c)) = self.lean {
            (x, z) = (s * z + c * x, z * c - x * s);
        }
        [x, y, z + self.depth]
    }

    fn project(&self, v: V3) -> FPoint {
        FPoint::new((self.cx + v[0] * self.px / v[2]) as f32, (self.cy + v[1] * self.py / v[2]) as f32)
    }
}

/// Clips a triangle to z >= NEAR: a fan of up to 4 points.
fn clip(t: [V3; 3]) -> ([V3; 4], usize) {
    let mut out = [[0.0; 3]; 4];
    let mut n = 0;
    for i in 0..3 {
        let (a, b) = (t[i], t[(i + 1) % 3]);
        if a[2] >= NEAR {
            out[n] = a;
            n += 1;
        }
        if (a[2] >= NEAR) != (b[2] >= NEAR) {
            let k = (NEAR - a[2]) / (b[2] - a[2]);
            out[n] = [a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k, NEAR];
            n += 1;
        }
    }
    (out, n)
}

fn polar(deg: f64, r: f64) -> (f64, f64) {
    let (s, c) = deg.to_radians().sin_cos();
    (s * r, c * r)
}

pub fn fcolor(c: Rgb) -> FColor {
    FColor::RGBA((c[0] / 255.0) as f32, (c[1] / 255.0) as f32, (c[2] / 255.0) as f32, 1.0)
}

/// The colours on screen.
pub fn palette(w: &World) -> Colours {
    if w.view().flash > 0.0 {
        [[255.0; 3]; SLOTS]
    } else if !w.alive() {
        // after death the colours pulse with the glow
        w.pal().colours_at(PAL_FULL - w.view().glow)
    } else {
        w.pal().colours()
    }
}

/// The playfield's triangles, in buffers reused from frame to frame.
pub struct Scene3d {
    pub clear: FColor,
    pub verts: Vec<Vertex>,
    laid: Vec<(usize, Wall)>,
    scratch: Vec<f64>,
}

impl Default for Scene3d {
    fn default() -> Scene3d {
        Scene3d { clear: FColor::RGBA(0.0, 0.0, 0.0, 1.0), verts: Vec::new(), laid: Vec::new(), scratch: Vec::new() }
    }
}

impl Scene3d {
    pub fn build(&mut self, w: &World, width: f64, height: f64) {
        let pal = palette(w);
        let col = |slot: usize| fcolor(pal[slot]);
        self.clear = col(slot::BACKGROUND);
        self.verts.clear();
        let v = w.view();
        let (shape, player) = (w.shape(), w.player());

        let scale = height / VIEW_H;
        let mut depth = CAM_DIST - 5.0 * v.camangle + v.zoompulsedepth;
        if v.zoom > 250.0 {
            depth += v.zoom * 4.0 - 1000.0;
        }
        let cam = Camera {
            cam: (-v.camangle).to_radians().sin_cos(),
            xrot: (v.wobbleangle + if w.scene() == Scene::Run { v.pitch } else { 0.0 }).to_radians().sin_cos(),
            title: (w.scene() == Scene::Title).then(|| (-55.0f64).to_radians().sin_cos()),
            lean: (w.scene() == Scene::StageSelect).then(|| (-player.tilt).to_radians().sin_cos()),
            depth,
            cx: width / 2.0,
            cy: height / 2.0 + if w.scene() == Scene::StageSelect { 130.0 * scale } else { 0.0 },
            px: VIEW_W / 2.0 / HALF_FOV_X.to_radians().tan() * scale,
            py: VIEW_H / 2.0 / HALF_FOV_Y.to_radians().tan() * scale,
        };

        let verts = &mut self.verts;
        let mut tri = |a: V3, b: V3, c: V3, slot: usize| {
            let color = col(slot);
            let (poly, n) = clip([a, b, c]);
            for i in 1..n.saturating_sub(1) {
                for p in [poly[0], poly[i], poly[i + 1]] {
                    verts.push(Vertex { position: cam.project(p), color, tex_coord: FPoint::new(0.0, 0.0) });
                }
            }
        };
        let pt = |deg: f64, r: f64, z: f64| {
            let (x, y) = polar(deg, r);
            cam.view(x, y, z)
        };

        let n = (shape.sides as usize).min(MAX_SIDES);
        let deg = 360.0 / (shape.sides as f64 - shape.morph_amount);
        let r = v.zoom + v.pulse;

        // centre and background wedges
        let mut start = v.spin;
        if v.centre_flip && (w.survival() / 45) % 2 == 1 {
            start += deg;
        }
        let corner = |i: usize| start + (i % n) as f64 * deg;
        let (mut inner, mut far, mut border) = ([[0.0; 3]; MAX_SIDES], [[0.0; 3]; MAX_SIDES], [[0.0; 3]; MAX_SIDES]);
        for i in 0..n {
            inner[i] = pt(corner(i), r, 0.0);
            far[i] = pt(corner(i), FAR, 0.0);
            border[i] = pt(corner(i), r - CENTRE_BORDER, 0.0);
        }
        let mut wedge = |a: usize, b: usize, slot: usize| {
            tri(inner[a], inner[b], far[a], slot);
            tri(inner[b], far[b], far[a], slot);
        };
        match n {
            4 => {
                wedge(1, 2, slot::WEDGE);
                wedge(3, 0, slot::WEDGE);
            }
            5 => {
                wedge(1, 2, slot::WEDGE);
                wedge(3, 4, slot::WEDGE);
                wedge(4, 0, slot::ODD_WEDGE);
            }
            6 => {
                wedge(1, 2, slot::WEDGE);
                wedge(3, 4, slot::WEDGE);
                wedge(5, 0, slot::WEDGE);
            }
            _ => {}
        }
        if (4..=6).contains(&n) {
            for i in 0..n {
                let j = (i + 1) % n;
                tri(inner[i], inner[j], border[i], slot::CENTRE);
                tri(inner[j], border[j], border[i], slot::CENTRE);
            }
        }

        // walls, with the waves laid out ahead on their way in
        let pside = w.player_side();
        w.pending_walls(&mut self.laid, &mut self.scratch);
        let ending = w.ending();
        for (i, wl) in w.walls().iter().copied().enumerate().chain(self.laid.iter().copied()) {
            if !wl.active || wl.kind != WallKind::Wall || wl.side >= shape.sides {
                continue;
            }
            let a0 = v.spin + wl.side as f64 * deg;
            let a1 = if wl.side == shape.sides - 1 { v.spin + 360.0 } else { v.spin + (wl.side + 1) as f64 * deg };
            let (r0, r1) = if ending {
                let out = ENDING_SPAWN - wl.dist / WALL_UNITS;
                if out < 0.0 {
                    continue;
                }
                let len = wl.len / WALL_UNITS;
                if out < len { (r, r + out) } else { (r + out - len, r + out) }
            } else {
                (r + wl.dist / WALL_UNITS, r + (wl.dist + wl.len) / WALL_UNITS)
            };
            let colour = if wl.side == pside { slot::NEAR_WALLS } else { slot::WALLS } + (i & 1);
            let (p0, p1, p2, p3) = (pt(a0, r0, 0.0), pt(a1, r0, 0.0), pt(a1, r1, 0.0), pt(a0, r1, 0.0));
            tri(p0, p1, p3, colour);
            tri(p1, p2, p3, colour);
        }
        if ending {
            return;
        }

        // player
        let a = player.angle + v.spin;
        let (px, py) = polar(a, r + PLAYER_OUT);
        let ta = a - player.tilt * 4.0;
        let corner = |k: usize, rr: f64, z: f64| {
            let (x, y) = polar(ta + 120.0 * k as f64, rr);
            cam.view(px + x, py + y, z)
        };
        if !w.alive() {
            // the player scatters on death
            let gt = v.gameover;
            let out = [0, 1, 2].map(|k| corner(k, gt % 15.0 + 10.0, 0.0));
            let inn = [0, 1, 2].map(|k| corner(k, gt % 15.0 + 2.0, 0.0));
            for k in 0..3 {
                let j = (k + 1) % 3;
                tri(out[k], out[j], inn[k], slot::HIGHLIGHT);
                tri(out[j], inn[j], inn[k], slot::HIGHLIGHT);
            }
        }
        let body = |z| [0, 1, 2].map(|k| corner(k, PLAYER_SIZE, z));
        let [a, b, c] = body(0.0);
        tri(a, b, c, slot::PLAYER);
        let [a, b, c] = body(PLAYER_SHADOW);
        tri(a, b, c, slot::HIGHLIGHT);
    }
}
