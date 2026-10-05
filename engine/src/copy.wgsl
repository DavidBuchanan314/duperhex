// The frame to the window.

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;

@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(src, src_sampler, in.uv).rgb, 1.0);
}
