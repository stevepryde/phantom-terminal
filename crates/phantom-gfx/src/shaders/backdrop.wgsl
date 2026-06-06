// Terminal backdrop image quads. The image is sampled as sRGB and composited
// with a per-instance opacity so terminal cell backgrounds can still draw over it.

struct Uniforms {
    screen: vec2<f32>,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(1) @binding(0) var backdrop_tex: texture_2d<f32>;
@group(1) @binding(1) var backdrop_samp: sampler;

struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv_min: vec2<f32>,
    @location(3) uv_max: vec2<f32>,
    @location(4) opacity: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) opacity: f32,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, in: VsIn) -> VsOut {
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let corner = corners[vi];
    let px = in.pos + corner * in.size;
    let ndc = vec2<f32>(px.x / u.screen.x * 2.0 - 1.0, 1.0 - px.y / u.screen.y * 2.0);

    var out: VsOut;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = in.uv_min + corner * (in.uv_max - in.uv_min);
    out.opacity = in.opacity;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let texel = textureSample(backdrop_tex, backdrop_samp, in.uv);
    let image_alpha = texel.a * in.opacity;
    return vec4<f32>(texel.rgb * image_alpha, 1.0);
}
