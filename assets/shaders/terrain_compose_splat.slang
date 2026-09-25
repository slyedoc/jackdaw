// Composes jackdaw's splat into a terrain tile's diffuse texture, one thread per texel.
//
// This is the same output contract as aurora's `terrain.slang` `terrain_compose` -- write
// RGBA into `dst` for a dirty rect -- with jackdaw's blending law in place of WoW-style
// sequential alpha lerp. Composing rather than evaluating per hit keeps the ray-traced
// path a plain textured mesh through the opaque closest-hit shader: no terrain-specific
// hit group, no per-bounce splat cost.
//
// The trade against jackdaw's raster original is texel density. A clipmap evaluated per
// fragment has no fixed resolution; a composed texture does. `out_px` is that choice.

import terrain_splat;

/// One layer's textures as raw RGBA byte buffers, matching aurora's `PaletteEntry`.
struct Layer {
    uint* albedo;
    uint* height;
    uint w;
    uint h;
    float uv_scale;
    float detile;
    uint pad0;
    uint pad1;
}

struct ComposeSplatParams {
    /// Packed control words, `control_res` square.
    uint* control;
    /// Ground slope in radians per control texel, same grid.
    float* slope;
    Layer* layers;
    /// Destination diffuse, `out_px` square.
    uint* dst;

    uint control_res;
    uint out_px;
    uint layer_count;
    uint base;

    /// Dirty rect in destination texels.
    uint x0;
    uint y0;
    uint x1;
    uint y1;

    float blend_sharpness;
    uint autoterrain_enabled;
    uint autoterrain_base_slot;
    uint autoterrain_slope_slot;

    float autoterrain_slope_start;
    float autoterrain_slope_end;
    uint pad0;
    uint pad1;
}

[[vk::push_constant]] ComposeSplatParams params;

float3 layer_fetch(Layer t, float2 uv) {
    // Wrapped, point-sampled: the caller's `uv_scale` already decides the repeat, and a
    // composed texel is its own final resolution so there is nothing to filter against.
    let x = uint(frac(uv.x) * float(t.w)) % t.w;
    let y = uint(frac(uv.y) * float(t.h)) % t.h;
    let p = t.albedo[y * t.w + x];
    return float3(float(p & 0xff), float((p >> 8) & 0xff), float((p >> 16) & 0xff));
}

float layer_height(Layer t, float2 uv) {
    if (t.height == nullptr) {
        return 0.0;
    }
    let x = uint(frac(uv.x) * float(t.w)) % t.w;
    let y = uint(frac(uv.y) * float(t.h)) % t.h;
    return float(t.height[y * t.w + x] & 0xff) / 255.0;
}

/// The control word a texel draws: its own, or the slope's where no hand has claimed it.
uint control_at(uint cx, uint cy) {
    let word = params.control[cy * params.control_res + cx];
    let c = control_decode(word);
    if (params.autoterrain_enabled == 0 || c.manual) {
        return word;
    }
    return autoterrain_control(
        params.slope[cy * params.control_res + cx],
        params.autoterrain_base_slot,
        params.autoterrain_slope_slot,
        params.autoterrain_slope_start,
        params.autoterrain_slope_end);
}

[shader("compute")]
[numthreads(64, 1, 1)]
void terrain_compose_splat(uint3 gid: SV_DispatchThreadID) {
    let rw = params.x1 - params.x0;
    let i = params.base + gid.x;
    if (i >= rw * (params.y1 - params.y0)) {
        return;
    }
    let rx = params.x0 + i % rw;
    let ry = params.y0 + i / rw;

    // Destination texel -> control grid, keeping the fraction for the bilinear corners.
    let uv = float2((float(rx) + 0.5) / float(params.out_px),
                    (float(ry) + 0.5) / float(params.out_px));
    let cf = uv * float(params.control_res) - 0.5;
    let c0 = int2(floor(cf));
    let fr = cf - float2(c0);
    let maxc = int(params.control_res) - 1;

    // Four control texels and their bilinear shares, in the order the weights follow.
    uint words[4];
    float4 corner = float4(
        (1.0 - fr.x) * (1.0 - fr.y),
        fr.x * (1.0 - fr.y),
        (1.0 - fr.x) * fr.y,
        fr.x * fr.y);
    for (int k = 0; k < 4; ++k) {
        let ox = clamp(c0.x + (k & 1), 0, maxc);
        let oy = clamp(c0.y + (k >> 1), 0, maxc);
        words[k] = control_at(uint(ox), uint(oy));
    }

    let last = last_layer(params.layer_count);

    // Every id the four corners name, height-blended against each other. Two ids per
    // corner (base and overlay), so at most eight contenders; the winner-takes-most curve
    // is what makes a cliff face meet grass along the rock's own silhouette rather than a
    // straight bilinear seam.
    float3 color = float3(0.0);
    float total = 0.0;
    for (int k = 0; k < 4; ++k) {
        let c = control_decode(words[k]);
        for (int which = 0; which < 2; ++which) {
            let id = min(which == 0 ? c.base : c.overlay, last);
            let layer_weight = which == 0 ? (1.0 - c.blend) : c.blend;
            if (layer_weight <= 0.0) {
                continue;
            }
            let layer = params.layers[id];
            let luv = detile_uv(uv * layer.uv_scale, layer.detile);
            let w = height_blend_weight(
                float4(corner[k], 0.0, 0.0, 0.0),
                layer_weight,
                layer_height(layer, luv),
                params.blend_sharpness);
            color += layer_fetch(layer, luv) * w;
            total += w;
        }
    }
    color /= max(total, 1e-8);

    params.dst[ry * params.out_px + rx] =
        0xff000000 | (uint(color.z) << 16) | (uint(color.y) << 8) | uint(color.x);
}
