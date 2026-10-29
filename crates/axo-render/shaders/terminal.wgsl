// Terminal quads: solid rectangles and atlas glyphs, one instance each.
// Positions arrive in NDC; atlas coordinates in texels. The target is sRGB,
// so blending happens in linear light on premultiplied colors.

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) kind: u32,
    @location(3) @interpolate(flat) contrast: f32,
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
    // Same coverage contrast as aegle text: dark text gains weight, light loses some.
    out.contrast = 1.0 - 2.0 * dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    return out;
}

@group(0) @binding(0) var atlas: texture_2d<f32>;

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    if in.kind == 0u {
        return vec4<f32>(in.color.rgb * in.color.a, in.color.a);
    }
    let texel = textureLoad(atlas, vec2<i32>(floor(in.uv)), 0);
    if in.kind == 1u {
        let c = texel.a + texel.a * (1.0 - texel.a) * in.contrast;
        let a = in.color.a * c;
        return vec4<f32>(in.color.rgb * a, a);
    }
    // Color glyphs store straight sRGB.
    return vec4<f32>(linear(texel.rgb) * texel.a, texel.a) * in.color.a;
}
