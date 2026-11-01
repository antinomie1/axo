// Terminal quads: solid rectangles and atlas glyphs, one instance each.
// Positions arrive in NDC; atlas coordinates in texels. The target is sRGB,
// so blending happens in linear light on premultiplied colors.
//
// Coverage glyphs sample a single-channel page; color glyphs (emoji) sample an
// RGBA page. Keeping them apart makes the common case an R8 atlas. Images have
// their own entry point and texture, and are sampled filtered: they scale, and
// their sRGB format decodes to linear before filtering.

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) kind: u32,
    @location(3) @interpolate(flat) contrast: f32,
    // Texel centers the image filter may reach: the corners of its crop.
    @location(4) @interpolate(flat) crop: vec4<f32>,
}

fn linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@vertex
fn vs_main(
    @builtin(vertex_index) vertex: u32,
    @location(0) rect: vec4<f32>,
    @location(1) uv: vec4<u32>,
    @location(2) color: vec4<f32>,
    @location(3) kind: u32,
) -> Out {
    let corner = vec2<f32>(f32(vertex & 1u), f32(vertex >> 1u));
    var out: Out;
    out.position = vec4<f32>(rect.xy + rect.zw * corner, 0.0, 1.0);
    out.uv = vec2<f32>(uv.xy) + vec2<f32>(uv.zw) * corner;
    out.color = vec4<f32>(linear(color.rgb), color.a);
    out.kind = kind;
    out.crop = vec4<f32>(vec2<f32>(uv.xy) + 0.5, vec2<f32>(uv.xy + uv.zw) - 0.5);
    // Same coverage contrast as aegle text: dark text gains weight, light loses some.
    out.contrast = 1.0 - 2.0 * dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    return out;
}

@group(0) @binding(0) var masks: texture_2d<f32>;
@group(0) @binding(1) var colors: texture_2d<f32>;
@group(0) @binding(2) var bilinear: sampler;
@group(1) @binding(0) var image: texture_2d<f32>;

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    if in.kind == 0u {
        return vec4<f32>(in.color.rgb * in.color.a, in.color.a);
    }
    let at = vec2<i32>(floor(in.uv));
    if in.kind == 1u {
        let coverage = textureLoad(masks, at, 0).r;
        let c = coverage + coverage * (1.0 - coverage) * in.contrast;
        let a = in.color.a * c;
        return vec4<f32>(in.color.rgb * a, a);
    }
    // Color glyphs store straight sRGB.
    let texel = textureLoad(colors, at, 0);
    return vec4<f32>(linear(texel.rgb) * texel.a, texel.a) * in.color.a;
}

@fragment
fn fs_image(in: Out) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(image));
    let at = clamp(in.uv, in.crop.xy, in.crop.zw);
    let texel = textureSampleLevel(image, bilinear, at / size, 0.0);
    return vec4<f32>(texel.rgb * texel.a, texel.a);
}
