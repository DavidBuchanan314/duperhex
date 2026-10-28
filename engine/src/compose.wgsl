// The playfield into the frame, with its effects: bent by the fluid's flow, bloom and the fluid's
// smoke added, and colours split by chromatic aberration.

@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var scene_sampler: sampler;
@group(0) @binding(2) var bloom: texture_2d<f32>;
@group(0) @binding(3) var smoke: texture_2d<f32>;
@group(0) @binding(4) var flow: texture_2d<f32>;

// the scene's shown displaced by where the flow would carry it in this many seconds, up to this
// far (in uv)
const BEND: f32 = 0.02;
const MAX_BEND: f32 = 0.012;
// the smoke's thickest, how much brighter it is than the walls that shed it, and how much
// thinner it is over walls
const SMOKE: f32 = 0.45;
const SMOKE_GLOW: f32 = 1.4;
const SMOKE_OVER_WALLS: f32 = 0.6;

fn color(uv: vec2<f32>) -> vec3<f32> {
    var bend = textureSample(flow, scene_sampler, uv).xy / vec2<f32>(textureDimensions(flow)) * BEND;
    let far = length(bend);
    if far > MAX_BEND {
        bend *= MAX_BEND / far;
    }
    let at = uv - bend * effects.fluid;
    let c = textureSample(scene, scene_sampler, at);
    let lit = c.rgb + textureSample(bloom, scene_sampler, at).rgb * effects.bloom;
    // the smoke's a haze of its colour over the scene, as thick as it's bright
    let s = textureSample(smoke, scene_sampler, at).rgb;
    let thick = smoothstep(0.1, 1.0, max(s.r, max(s.g, s.b))) * SMOKE * effects.fluid * (1.0 - SMOKE_OVER_WALLS * c.a);
    return mix(lit, s / max(max(s.r, max(s.g, s.b)), 1e-3) * SMOKE_GLOW, thick);
}

@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
    let d = in.uv - vec2<f32>(0.5);
    let r = color(0.5 + d * (1.0 - effects.aberration)).r;
    let g = color(0.5 + d * (1.0 - effects.aberration * 0.5)).g;
    let b = color(in.uv).b;
    return vec4<f32>(r, g, b, 1.0);
}
