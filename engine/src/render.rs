//! The playfield: the original's 3D scene and camera, as real 3D with near-plane clipping, drawn
//! into buffers kept from frame to frame.

use crate::colour::Linear;
use crate::gpu::Vertex;
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

// 3D walls
const WALL_H: f64 = 26.0;
const CENTRE_H: f64 = 18.0;
/// The camera tips back this many degrees more than the original's, swaying this much either way,
/// and turns this much either way.
const TILT: (f64, f64) = (28.0, 8.0);
const TURN: f64 = 7.0;
/// How fast the light circles, in radians per tick, and how high it is (the sine of its angle).
const LIGHT_SPEED: f64 = 0.006;
const LIGHT_UP: f64 = 0.55;
/// The light on walls' sides: how much falls on them facing away from it, and how much reaches
/// their feet.
const SIDE_AMBIENT: f32 = 0.18;
const FOOT: f32 = 0.3;
/// How bright the floor's reflections are, and how far down they fade out.
const MIRROR: f32 = 0.45;
const MIRROR_FADE: f64 = 60.0;
/// Shadows multiply the floor by this sRGB grey.
const SHADE: f64 = 150.0;

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

    fn project(&self, v: V3) -> [f32; 2] {
        [(self.cx + v[0] * self.px / v[2]) as f32, (self.cy + v[1] * self.py / v[2]) as f32]
    }
}

/// Clips a triangle, with a colour at each corner, to z >= NEAR: a fan of up to 4 points.
fn clip(t: [(V3, Linear); 3]) -> ([(V3, Linear); 4], usize) {
    let mut out = [t[0]; 4];
    let mut n = 0;
    for i in 0..3 {
        let ((a, ca), (b, cb)) = (t[i], t[(i + 1) % 3]);
        if a[2] >= NEAR {
            out[n] = t[i];
            n += 1;
        }
        if (a[2] >= NEAR) != (b[2] >= NEAR) {
            let k = (NEAR - a[2]) / (b[2] - a[2]);
            out[n] = ([a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k, NEAR], ca.lerp(cb, k as f32));
            n += 1;
        }
    }
    (out, n)
}

/// A triangle in view space, clipped, projected, and with its depth in uv.x.
fn push(out: &mut Vec<Vertex>, cam: &Camera, t: [(V3, Linear); 3]) {
    let (poly, n) = clip(t);
    for i in 1..n.saturating_sub(1) {
        for (p, color) in [poly[0], poly[i], poly[i + 1]] {
            out.push(Vertex { pos: cam.project(p), uv: [(1.0 - NEAR / p[2]) as f32, 0.0], color });
        }
    }
}

fn quad(out: &mut Vec<Vertex>, cam: &Camera, q: [(V3, Linear); 4]) {
    push(out, cam, [q[0], q[1], q[2]]);
    push(out, cam, [q[0], q[2], q[3]]);
}

/// 3D walls' parts, as in gpu::Frame.
struct Solids<'a> {
    mirror: &'a mut Vec<Vertex>,
    shadow: &'a mut Vec<Vertex>,
    solid: &'a mut Vec<Vertex>,
    cam: &'a Camera,
    /// Towards the light, in playfield space with z up.
    light: V3,
    shade: Linear,
}

impl Solids<'_> {
    /// A prism over the convex polygon `base` on the floor, from height `lo` up to `hi`, its top
    /// `top` and its sides `colour`: lit, with its reflection and shadow.
    fn prism(&mut self, base: &[(f64, f64)], lo: f64, hi: f64, top: Linear, colour: Linear) {
        let cam = self.cam;
        // up is towards the eye, -z
        let at = |(x, y): (f64, f64), h: f64| cam.view(x, y, -h);
        let [lx, ly, lz] = self.light;
        let cast = |(x, y): (f64, f64), h: f64| cam.view(x - lx * h / lz, y - ly * h / lz, 0.0);
        let fade = |h: f64| MIRROR * (1.0 - h / MIRROR_FADE).max(0.0) as f32;
        let n = base.len();
        let (cx, cy) = base.iter().fold((0.0, 0.0), |(sx, sy), &(x, y)| (sx + x / n as f64, sy + y / n as f64));

        for i in 1..n - 1 {
            let (a, b, c) = (base[0], base[i], base[i + 1]);
            push(self.solid, cam, [(at(a, hi), top), (at(b, hi), top), (at(c, hi), top)]);
            let shade = self.shade;
            push(self.shadow, cam, [(cast(a, hi), shade), (cast(b, hi), shade), (cast(c, hi), shade)]);
            if lo > 0.0 {
                push(self.shadow, cam, [(cast(a, lo), shade), (cast(b, lo), shade), (cast(c, lo), shade)]);
            }
        }
        for i in 0..n {
            let (a, b) = (base[i], base[(i + 1) % n]);
            // the side's outward normal
            let (mut nx, mut ny) = (b.1 - a.1, a.0 - b.0);
            let len = nx.hypot(ny).max(1e-9);
            (nx, ny) = (nx / len, ny / len);
            if nx * ((a.0 + b.0) / 2.0 - cx) + ny * ((a.1 + b.1) / 2.0 - cy) < 0.0 {
                (nx, ny) = (-nx, -ny);
            }
            let lit = SIDE_AMBIENT + (1.0 - SIDE_AMBIENT) * (nx * lx + ny * ly).max(0.0) as f32;
            let foot = FOOT + (1.0 - FOOT) * (lo / hi) as f32;
            let (head, foot) = (colour.scaled(lit), colour.scaled(lit * foot));
            quad(self.solid, cam, [(at(a, lo), foot), (at(b, lo), foot), (at(b, hi), head), (at(a, hi), head)]);
            // its reflection hangs below the floor, fading with depth
            let below = |(x, y): (f64, f64), h: f64| cam.view(x, y, h);
            let (head_r, foot_r) = (head.scaled(fade(hi)), foot.scaled(fade(lo)));
            quad(self.mirror, cam, [(below(a, lo), foot_r), (below(b, lo), foot_r), (below(b, hi), head_r), (below(a, hi), head_r)]);
            let shade = self.shade;
            quad(self.shadow, cam, [(cast(a, lo), shade), (cast(b, lo), shade), (cast(b, hi), shade), (cast(a, hi), shade)]);
        }
    }
}

fn polar(deg: f64, r: f64) -> (f64, f64) {
    let (s, c) = deg.to_radians().sin_cos();
    (s * r, c * r)
}

/// The effects follow the centre's pulse to the beat, rising from nothing to full between these
/// two pulse values.
const BEAT_PULSE: (f64, f64) = (4.0, 20.0);
/// At full, chromatic aberration pulls red this fraction of the way in towards the centre of the
/// screen.
const MAX_ABERRATION: f64 = 0.03;
/// Bloom is added at this strength, and this much more at full.
const BLOOM: (f64, f64) = (0.25, 0.3);

/// How strong the beat is now, 0 to 1.
pub fn beat(w: &World) -> f64 {
    let (from, full) = BEAT_PULSE;
    ((w.view().pulse - from) / (full - from)).clamp(0.0, 1.0)
}

pub fn aberration(w: &World) -> f32 {
    (beat(w) * MAX_ABERRATION) as f32
}

pub fn bloom(w: &World) -> f32 {
    (BLOOM.0 + beat(w) * BLOOM.1) as f32
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

/// The playfield's triangles, in buffers reused from frame to frame. Vertex alpha is the glow
/// weight: 0 for the background, 1 for everything else.
pub struct Scene3d {
    pub clear: Linear,
    /// The lightness of the lighter background colour, which glowing parts must be lighter than.
    pub backdrop: f32,
    pub verts: Vec<Vertex>,
    /// The player, kept apart so the effects leave it clear.
    pub player: Vec<Vertex>,
    /// With 3D walls: see gpu::Frame.
    pub mirror: Vec<Vertex>,
    pub shadow: Vec<Vertex>,
    pub solid: Vec<Vertex>,
    laid: Vec<(usize, Wall)>,
    scratch: Vec<f64>,
}

impl Default for Scene3d {
    fn default() -> Scene3d {
        Scene3d {
            clear: Linear::from_srgb([0.0; 3], 0.0),
            backdrop: 0.0,
            verts: Vec::new(),
            player: Vec::new(),
            mirror: Vec::new(),
            shadow: Vec::new(),
            solid: Vec::new(),
            laid: Vec::new(),
            scratch: Vec::new(),
        }
    }
}

impl Scene3d {
    /// With `extrude`, the walls, the centre and the player stand up in 3D.
    pub fn build(&mut self, w: &World, width: f64, height: f64, extrude: bool) {
        let pal = palette(w);
        let col = |slot: usize| {
            let glow = !matches!(slot, slot::BACKGROUND | slot::WEDGE | slot::ODD_WEDGE);
            Linear::from_srgb(pal[slot], glow as u8 as f32)
        };
        self.clear = col(slot::BACKGROUND);
        self.backdrop = col(slot::BACKGROUND).lightness().max(col(slot::WEDGE).lightness());
        self.verts.clear();
        self.player.clear();
        self.mirror.clear();
        self.shadow.clear();
        self.solid.clear();
        let v = w.view();
        let (shape, player) = (w.shape(), w.player());

        let scale = height / VIEW_H;
        let mut depth = CAM_DIST - 5.0 * v.camangle + v.zoompulsedepth;
        if v.zoom > 250.0 {
            depth += v.zoom * 4.0 - 1000.0;
        }
        let (tilt, turn) = if extrude {
            let t = w.t();
            (TILT.0 + TILT.1 * (t * 0.011).sin(), TURN * (t * 0.0067).sin())
        } else {
            (0.0, 0.0)
        };
        let cam = Camera {
            cam: (-v.camangle - tilt).to_radians().sin_cos(),
            xrot: (v.wobbleangle + turn + if w.scene() == Scene::Run { v.pitch } else { 0.0 }).to_radians().sin_cos(),
            title: (w.scene() == Scene::Title).then(|| (-55.0f64).to_radians().sin_cos()),
            lean: (w.scene() == Scene::StageSelect).then(|| (-player.tilt).to_radians().sin_cos()),
            depth,
            cx: width / 2.0,
            cy: height / 2.0 + if w.scene() == Scene::StageSelect { 130.0 * scale } else { 0.0 },
            px: VIEW_W / 2.0 / HALF_FOV_X.to_radians().tan() * scale,
            py: VIEW_H / 2.0 / HALF_FOV_Y.to_radians().tan() * scale,
        };

        let tri = |out: &mut Vec<Vertex>, a: V3, b: V3, c: V3, slot: usize| {
            let color = col(slot);
            push(out, &cam, [(a, color), (b, color), (c, color)]);
        };
        let la = w.t() * LIGHT_SPEED;
        let flat = (1.0 - LIGHT_UP * LIGHT_UP).sqrt();
        let mut solids = Solids {
            mirror: &mut self.mirror,
            shadow: &mut self.shadow,
            solid: &mut self.solid,
            cam: &cam,
            light: [la.cos() * flat, la.sin() * flat, LIGHT_UP],
            shade: Linear::from_srgb([SHADE; 3], 0.0),
        };
        let verts = &mut self.verts;
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
            tri(verts, inner[a], inner[b], far[a], slot);
            tri(verts, inner[b], far[b], far[a], slot);
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
        if (4..=6).contains(&n) && extrude {
            for i in 0..n {
                let j = (i + 1) % n;
                let q = [polar(corner(i), r), polar(corner(j), r), polar(corner(j), r - CENTRE_BORDER), polar(corner(i), r - CENTRE_BORDER)];
                solids.prism(&q, 0.0, CENTRE_H, col(slot::CENTRE), col(slot::CENTRE));
            }
        } else if (4..=6).contains(&n) {
            for i in 0..n {
                let j = (i + 1) % n;
                tri(verts, inner[i], inner[j], border[i], slot::CENTRE);
                tri(verts, inner[j], border[j], border[i], slot::CENTRE);
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
            let colour = if wl.side == pside && !ending { slot::NEAR_WALLS } else { slot::WALLS } + (i & 1);
            if extrude {
                solids.prism(&[polar(a0, r0), polar(a1, r0), polar(a1, r1), polar(a0, r1)], 0.0, WALL_H, col(colour), col(colour));
                continue;
            }
            let (p0, p1, p2, p3) = (pt(a0, r0, 0.0), pt(a1, r0, 0.0), pt(a1, r1, 0.0), pt(a0, r1, 0.0));
            tri(verts, p0, p1, p3, colour);
            tri(verts, p1, p2, p3, colour);
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
                tri(&mut self.player, out[k], out[j], inn[k], slot::HIGHLIGHT);
                tri(&mut self.player, out[j], inn[j], inn[k], slot::HIGHLIGHT);
            }
        }
        if extrude {
            // as tall as the walls, in its flat colours (its highlight on top), and not glowing
            let base = [0, 1, 2].map(|k| {
                let (x, y) = polar(ta + 120.0 * k as f64, PLAYER_SIZE);
                (px + x, py + y)
            });
            let dull = |slot: usize| Linear::from_srgb(pal[slot], 0.0);
            solids.prism(&base, 0.0, WALL_H, dull(slot::HIGHLIGHT), dull(w.player_slot()));
            return;
        }
        let body = |z| [0, 1, 2].map(|k| corner(k, PLAYER_SIZE, z));
        let [a, b, c] = body(0.0);
        tri(&mut self.player, a, b, c, w.player_slot());
        let [a, b, c] = body(PLAYER_SHADOW);
        tri(&mut self.player, a, b, c, slot::HIGHLIGHT);
    }
}
