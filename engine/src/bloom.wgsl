// Bloom: the scene's glowing parts, shrunk through a chain of half-size textures and grown back
// up, each level added into the next larger one. The scene's alpha is the glow weight, and a part
// glows in proportion to how much lighter it is than the backdrop.

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;

// how much lighter than the backdrop (in perceived lightness, 0 to 1) colours start to glow, and
// glow fully
const CONTRAST: vec2<f32> = vec2<f32>(0.1, 0.42);

// perceived lightness: Oklab's L
fn lightness(c: vec3<f32>) -> f32 {
    let lms = vec3<f32>(
        dot(c, vec3<f32>(0.4122214708, 0.5363325363, 0.0514459929)),
        dot(c, vec3<f32>(0.2119034982, 0.6806995451, 0.1073969566)),
        dot(c, vec3<f32>(0.0883024619, 0.2817188376, 0.6299787005)),
    );
    return dot(pow(max(lms, vec3<f32>(0.0)), vec3<f32>(1.0 / 3.0)), vec3<f32>(0.2104542553, 0.7936177850, -0.0040720468));
}

fn tap(uv: vec2<f32>) -> vec3<f32> {
    return textureSample(src, src_sampler, uv).rgb;
}

// the colour at uv, weighted by how much it glows
fn bright(uv: vec2<f32>) -> vec3<f32> {
    let c = textureSample(src, src_sampler, uv);
    return c.rgb * c.a * smoothstep(CONTRAST.x, CONTRAST.y, lightness(c.rgb) - effects.backdrop);
}

// shrinking to half size samples the centre, and the four diagonal neighbours (each a bilinear 2x2)
const DIAGONALS = array<vec2<f32>, 4>(vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, 1.0));

@fragment
fn fs_bright(in: Out) -> @location(0) vec4<f32> {
    let t = 1.0 / vec2<f32>(textureDimensions(src));
    var c = bright(in.uv) * 4.0;
    for (var i = 0; i < 4; i++) {
        c += bright(in.uv + t * DIAGONALS[i]);
    }
    return vec4<f32>(c / 8.0, 1.0);
}

@fragment
fn fs_down(in: Out) -> @location(0) vec4<f32> {
    let t = 1.0 / vec2<f32>(textureDimensions(src));
    var c = tap(in.uv) * 4.0;
    for (var i = 0; i < 4; i++) {
        c += tap(in.uv + t * DIAGONALS[i]);
    }
    return vec4<f32>(c / 8.0, 1.0);
}

// the source at twice the size, through a 3x3 tent
@fragment
fn fs_up(in: Out) -> @location(0) vec4<f32> {
    let t = 1.0 / vec2<f32>(textureDimensions(src));
    var c = tap(in.uv) * 4.0;
    c += (tap(in.uv + t * vec2<f32>(-1.0, 0.0)) + tap(in.uv + t * vec2<f32>(1.0, 0.0))
        + tap(in.uv + t * vec2<f32>(0.0, -1.0)) + tap(in.uv + t * vec2<f32>(0.0, 1.0))) * 2.0;
    c += tap(in.uv + t * vec2<f32>(-1.0, -1.0)) + tap(in.uv + t * vec2<f32>(1.0, -1.0))
        + tap(in.uv + t * vec2<f32>(-1.0, 1.0)) + tap(in.uv + t * vec2<f32>(1.0, 1.0));
    return vec4<f32>(c / 16.0, 1.0);
}
