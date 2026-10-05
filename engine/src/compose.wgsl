// The playfield into the frame, with its effects: bloom added, and colours split by chromatic
// aberration.

@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var scene_sampler: sampler;
@group(0) @binding(2) var bloom: texture_2d<f32>;

fn color(uv: vec2<f32>) -> vec3<f32> {
    return textureSample(scene, scene_sampler, uv).rgb + textureSample(bloom, scene_sampler, uv).rgb * effects.bloom;
}

@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
    let d = in.uv - vec2<f32>(0.5);
    let r = color(0.5 + d * (1.0 - effects.aberration)).r;
    let g = color(0.5 + d * (1.0 - effects.aberration * 0.5)).g;
    let b = color(in.uv).b;
    return vec4<f32>(r, g, b, 1.0);
}
