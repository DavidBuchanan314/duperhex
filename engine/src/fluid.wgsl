// A fluid, simulated at a fraction of the frame's size: stirred by the walls as they move,
// carrying smoke they shed in their own colours, and kicked outwards on the beat. Velocities are
// in the simulation's texels per second.
//
// Each step: the scene's walls, shrunk, into a mask (the last step's mask kept); the velocity's curl; the
// velocity carried along itself, curled up tighter, pushed by the walls' moving edges and kicked
// on the beat; its divergence; the pressure that cancels that, by Jacobi iteration; that
// pressure's gradient taken off; and the smoke carried along, with more shed by the walls.

struct Fluid {
    // one texel, in uv
    texel: vec2<f32>,
    // seconds this step
    dt: f32,
    // how much the beat has just risen, 0 to 1, shared between the frame's steps
    kick: f32,
    // seconds this frame, over which the mask changed, in one or more steps
    frame_dt: f32,
}

@group(0) @binding(0) var t0: texture_2d<f32>;
@group(0) @binding(1) var t1: texture_2d<f32>;
@group(0) @binding(2) var t2: texture_2d<f32>;
@group(0) @binding(3) var t3: texture_2d<f32>;
@group(0) @binding(4) var t4: texture_2d<f32>;
@group(0) @binding(5) var samp: sampler;
@group(0) @binding(6) var<uniform> fluid: Fluid;

// velocity kept per second
const DRAG: f32 = 0.85;
// how hard eddies are wound tighter
const VORTICITY: f32 = 4.0;
// how quickly the fluid at a wall's edge takes up (this much of) the wall's speed, per second
const GRIP: f32 = 30.0;
const PUSH: f32 = 1.0;
// the fastest a wall's edge is taken to move
const MAX_EDGE: f32 = 400.0;
// the beat's kick at the centre, in texels per second, and how it falls off with distance (in uv)
const KICK: f32 = 1200.0;
const KICK_FALLOFF: f32 = 3.0;
// smoke left per second, and the most there can be
const SMOKE_KEPT: f32 = 0.3;
const SMOKE_MAX: f32 = 1.0;
// smoke shed by walls: at their moving edges, and from the whole wall per second
const SHED_EDGE: f32 = 0.2;
const SHED: f32 = 0.0;

fn tap(t: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(t, samp, uv, 0.0);
}

// the screen's edges are solid: nothing flows through them. (Otherwise a flow straight across
// the whole screen, having neither divergence nor curl, would never be taken off by the pressure,
// and would carry on for ever as a wind.)
fn contained(uv: vec2<f32>, v: vec2<f32>) -> vec2<f32> {
    let t = fluid.texel;
    var w = v;
    if uv.x < t.x || uv.x > 1.0 - t.x {
        w.x = 0.0;
    }
    if uv.y < t.y || uv.y > 1.0 - t.y {
        w.y = 0.0;
    }
    return w;
}

// the scene's walls, averaged over each texel (however many of the scene's texels that covers, so
// they don't alias and crawl as they move): rgb their colour times coverage, and a their coverage
@fragment
fn fs_down(in: Out) -> @location(0) vec4<f32> {
    // taps two of the scene's texels apart, each filtered over 2x2 of them
    let n = clamp(vec2<i32>(ceil(vec2<f32>(textureDimensions(t0)) * fluid.texel * 0.5)), vec2<i32>(1), vec2<i32>(8));
    var sum = vec4<f32>(0.0);
    for (var y = 0; y < n.y; y++) {
        for (var x = 0; x < n.x; x++) {
            let o = (vec2<f32>(f32(x), f32(y)) + 0.5) / vec2<f32>(n) - 0.5;
            let c = tap(t0, in.uv + o * fluid.texel);
            sum += vec4<f32>(c.rgb * c.a, c.a);
        }
    }
    return sum / f32(n.x * n.y);
}

// that, blurred over a few texels so the walls' edges have a slope to measure motion by
@fragment
fn fs_mask(in: Out) -> @location(0) vec4<f32> {
    var sum = vec4<f32>(0.0);
    var weight = 0.0;
    for (var y = -2; y <= 2; y++) {
        for (var x = -2; x <= 2; x++) {
            let o = vec2<f32>(f32(x), f32(y));
            let w = exp(-dot(o, o) * 0.35);
            sum += tap(t0, in.uv + o * fluid.texel * 1.5) * w;
            weight += w;
        }
    }
    return sum / weight;
}

@fragment
fn fs_curl(in: Out) -> @location(0) vec4<f32> {
    let t = fluid.texel;
    let l = tap(t0, in.uv - vec2<f32>(t.x, 0.0)).y;
    let r = tap(t0, in.uv + vec2<f32>(t.x, 0.0)).y;
    let b = tap(t0, in.uv - vec2<f32>(0.0, t.y)).x;
    let u = tap(t0, in.uv + vec2<f32>(0.0, t.y)).x;
    return vec4<f32>(0.5 * ((r - l) - (u - b)), 0.0, 0.0, 1.0);
}

// how the walls' edges move here, from the mask now (t1) and before (t2): the change in coverage
// over its slope; and how much edge there is to push with
fn edge(uv: vec2<f32>) -> vec3<f32> {
    let t = fluid.texel;
    let a = tap(t1, uv).a;
    let before = tap(t2, uv).a;
    let g = 0.5 * vec2<f32>(
        tap(t1, uv + vec2<f32>(t.x, 0.0)).a - tap(t1, uv - vec2<f32>(t.x, 0.0)).a,
        tap(t1, uv + vec2<f32>(0.0, t.y)).a - tap(t1, uv - vec2<f32>(0.0, t.y)).a,
    );
    let rate = (a - before) / fluid.frame_dt;
    var v = -rate * g / (dot(g, g) + 1e-4);
    let speed = length(v);
    if speed > MAX_EDGE {
        v *= MAX_EDGE / speed;
    }
    return vec3<f32>(v, smoothstep(0.01, 0.08, length(g)));
}

// t0 velocity, t1 mask now, t2 mask before, t3 curl
@fragment
fn fs_velocity(in: Out) -> @location(0) vec4<f32> {
    let t = fluid.texel;
    let dt = fluid.dt;
    // carried along itself
    let here = tap(t0, in.uv).xy;
    var v = tap(t0, in.uv - here * dt * t).xy * pow(DRAG, dt);

    // eddies wound tighter: pushed round them, away from where they're weaker
    let c = tap(t3, in.uv).x;
    let grad = 0.5 * vec2<f32>(
        abs(tap(t3, in.uv + vec2<f32>(t.x, 0.0)).x) - abs(tap(t3, in.uv - vec2<f32>(t.x, 0.0)).x),
        abs(tap(t3, in.uv + vec2<f32>(0.0, t.y)).x) - abs(tap(t3, in.uv - vec2<f32>(0.0, t.y)).x),
    );
    let n = grad / (length(grad) + 1e-5);
    v += VORTICITY * vec2<f32>(n.y, -n.x) * c * dt * 60.0;

    // the walls' edges drag the fluid along with them
    let e = edge(in.uv);
    v = mix(v, e.xy * PUSH, e.z * min(1.0, GRIP * dt));

    // the beat blows outwards from the centre
    let aspect = t.y / t.x;
    let p = (in.uv - 0.5) * vec2<f32>(aspect, 1.0);
    let r = length(p);
    // (texels are square, so directions in uv corrected for aspect are directions in texels)
    v += (p / max(r, 1e-3)) * fluid.kick * KICK * exp(-r * KICK_FALLOFF) * smoothstep(0.0, 0.05, r);
    return vec4<f32>(contained(in.uv, v), 0.0, 1.0);
}

@fragment
fn fs_divergence(in: Out) -> @location(0) vec4<f32> {
    let t = fluid.texel;
    let l = tap(t0, in.uv - vec2<f32>(t.x, 0.0)).x;
    let r = tap(t0, in.uv + vec2<f32>(t.x, 0.0)).x;
    let b = tap(t0, in.uv - vec2<f32>(0.0, t.y)).y;
    let u = tap(t0, in.uv + vec2<f32>(0.0, t.y)).y;
    return vec4<f32>(0.5 * ((r - l) + (u - b)), 0.0, 0.0, 1.0);
}

// t0 pressure, t1 divergence
@fragment
fn fs_jacobi(in: Out) -> @location(0) vec4<f32> {
    let t = fluid.texel;
    let l = tap(t0, in.uv - vec2<f32>(t.x, 0.0)).x;
    let r = tap(t0, in.uv + vec2<f32>(t.x, 0.0)).x;
    let b = tap(t0, in.uv - vec2<f32>(0.0, t.y)).x;
    let u = tap(t0, in.uv + vec2<f32>(0.0, t.y)).x;
    return vec4<f32>((l + r + b + u - tap(t1, in.uv).x) * 0.25, 0.0, 0.0, 1.0);
}

// t0 velocity, t1 pressure
@fragment
fn fs_project(in: Out) -> @location(0) vec4<f32> {
    let t = fluid.texel;
    let l = tap(t1, in.uv - vec2<f32>(t.x, 0.0)).x;
    let r = tap(t1, in.uv + vec2<f32>(t.x, 0.0)).x;
    let b = tap(t1, in.uv - vec2<f32>(0.0, t.y)).x;
    let u = tap(t1, in.uv + vec2<f32>(0.0, t.y)).x;
    return vec4<f32>(contained(in.uv, tap(t0, in.uv).xy - 0.5 * vec2<f32>(r - l, u - b)), 0.0, 1.0);
}

// The smoke is finer than the velocity, and carried by MacCormack's method, which stays sharp
// where plain carrying blurs: carried ahead, then back again, and the error in that taken off.

// t0 smoke, t1 velocity: the smoke carried ahead
@fragment
fn fs_smoke_ahead(in: Out) -> @location(0) vec4<f32> {
    let v = tap(t1, in.uv).xy;
    return tap(t0, in.uv - v * fluid.dt * fluid.texel);
}

// t0 smoke, t1 velocity, t2 smoke carried ahead, t3 mask now, t4 mask before
@fragment
fn fs_smoke(in: Out) -> @location(0) vec4<f32> {
    let dt = fluid.dt;
    let step = tap(t1, in.uv).xy * dt * fluid.texel;
    let ahead = tap(t2, in.uv).rgb;
    let back = tap(t2, in.uv + step).rgb;
    var s = ahead + 0.5 * (tap(t0, in.uv).rgb - back);
    // kept within the smoke it came from, so the correction can't overshoot
    let texel = 1.0 / vec2<f32>(textureDimensions(t0));
    let source = (floor((in.uv - step) / texel - 0.5) + 0.5) * texel;
    let a = tap(t0, source).rgb;
    let b = tap(t0, source + vec2<f32>(texel.x, 0.0)).rgb;
    let c = tap(t0, source + vec2<f32>(0.0, texel.y)).rgb;
    let d = tap(t0, source + texel).rgb;
    s = clamp(s, min(min(a, b), min(c, d)), max(max(a, b), max(c, d)));
    s *= pow(SMOKE_KEPT, dt);

    let m = tap(t3, in.uv);
    let colour = m.rgb / max(m.a, 1e-3);
    let moved = abs(m.a - tap(t4, in.uv).a);
    s += colour * (moved * SHED_EDGE * dt / fluid.frame_dt + m.a * SHED * dt);
    return vec4<f32>(min(s, vec3<f32>(SMOKE_MAX)), 1.0);
}

@fragment
fn fs_copy(in: Out) -> @location(0) vec4<f32> {
    return tap(t0, in.uv);
}
