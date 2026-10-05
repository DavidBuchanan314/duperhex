// The effects' settings this frame, shared by the bloom and compose passes. Prepended to their
// shaders.

struct Effects {
    // how far red is pulled in towards the centre, as a fraction of the distance; green half as far
    aberration: f32,
    // how much of the bloom is added
    bloom: f32,
    // the lightness of the lighter background colour, which glowing parts must be lighter than
    backdrop: f32,
}

@group(1) @binding(0) var<uniform> effects: Effects;
