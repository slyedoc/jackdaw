// Terrain splat blending, ported from `terrain_splat.wgsl`.
//
// This is the ALGORITHM, not the plumbing: the control-word decode, the autoterrain
// slope rule, the height-blend weight and the detiling hash. Where it gets evaluated is
// the open question -- aurora's terrain composes a diffuse texture on the GPU
// (`terrain.slang`'s `terrain_compose`), while jackdaw's raster path evaluated it per
// fragment -- and the law is the same either way, so it lives here and both callers
// import it.
//
// What does NOT port directly: the wgsl reads its layers through `textureSampleGrad`
// with derivatives taken once in uniform control flow, because the per-layer branches
// make implicit-derivative sampling illegal in a fragment shader. Neither destination
// has that problem. A compute kernel has no derivatives to be illegal, and a closest-hit
// picks its LOD from the ray cone (`surface_common.glsl`'s `lod_base`), so both callers
// choose an explicit level and the uniform-control-flow dance goes away.

module terrain_splat;

// The remap of a 0..1 authoring dial onto a power exponent. 4 is a wide, soft
// cross-fade; 64 is a near-binary height cutout.
public static const float SHARPNESS_MIN = 4.0;
public static const float SHARPNESS_RANGE = 60.0;

public static const uint BASE_SHIFT = 0;
public static const uint BASE_MASK = 0x1F;
public static const uint OVERLAY_SHIFT = 5;
public static const uint OVERLAY_MASK = 0x1F;
public static const uint BLEND_SHIFT = 10;
public static const uint BLEND_MASK = 0xFF;
// A hand painted this cell, so its own word is what it draws.
public static const uint MANUAL_BIT = 0x40000;

public static const uint MAX_LAYERS = 16;

/// One control texel, unpacked.
public struct Control {
    public uint base;
    public uint overlay;
    /// 0..1: how much of `overlay` this cell asks for before height blending.
    public float blend;
    public bool manual;
}

public Control control_decode(uint word) {
    Control c;
    c.base = (word >> BASE_SHIFT) & BASE_MASK;
    c.overlay = (word >> OVERLAY_SHIFT) & OVERLAY_MASK;
    c.blend = float((word >> BLEND_SHIFT) & BLEND_MASK) / 255.0;
    c.manual = (word & MANUAL_BIT) != 0;
    return c;
}

public uint control_encode(uint base, uint overlay, float blend) {
    let b = uint(clamp(blend, 0.0, 1.0) * 255.0 + 0.5);
    return ((base & BASE_MASK) << BASE_SHIFT)
        | ((overlay & OVERLAY_MASK) << OVERLAY_SHIFT)
        | ((b & BLEND_MASK) << BLEND_SHIFT);
}

/// The word a cell no hand has claimed draws, from the ground's own slope.
///
/// The slope comes from the terrain's slope map, never from the surface being drawn: that
/// surface is a clipmap, so a slope taken from the mesh normal reads gentler with distance
/// and the texturing would drift as the camera moves.
public uint autoterrain_control(
    float slope,
    uint base_slot,
    uint slope_slot,
    float slope_start,
    float slope_end,
) {
    let t = smoothstep(slope_start, slope_end, slope);
    return control_encode(base_slot, slope_slot, t);
}

/// The height-blend weight for one layer id.
///
/// `corner_weights` holds the four bilinear corner shares, with zero in the slots that did
/// not name this id at this layer weight. `corner_w` rides INSIDE the power so a near
/// corner gets a height advantage, and multiplies OUTSIDE it so a far corner cannot win on
/// height alone. Taking the corners as a vector rather than one call each is what makes a
/// uniform-word fast path exact: a zeroed slot contributes `pow(k, s) * 0`, so summing the
/// slots here is identical to summing four separate calls.
public float height_blend_weight(
    float4 corner_weights,
    float layer_weight,
    float height,
    float sharpness_dial,
) {
    let sharpness = SHARPNESS_MIN + SHARPNESS_RANGE * sharpness_dial;
    let contested = float4(layer_weight + height);
    return dot(pow(corner_weights + contested, float4(sharpness)), corner_weights);
}

/// Two uncorrelated values in 0..1 from a tile coordinate. Integer-valued input, so the
/// hash only has to separate whole numbers.
public float2 tile_hash(float2 tile) {
    float3 h = frac(float3(tile.x, tile.y, tile.x + tile.y) * float3(0.1031, 0.1030, 0.0973));
    h += dot(h, h.yzx + 33.33);
    return frac((h.xx + h.yz) * h.zy);
}

/// Detiled UVs: the repeat grid is broken up by offsetting each whole tile by a hash of
/// its own coordinate, so a texture with an obvious motif does not read as a lattice.
/// `strength` 0 leaves the UVs alone.
public float2 detile_uv(float2 uv, float strength) {
    if (strength <= 0.0) {
        return uv;
    }
    let tile = floor(uv);
    return uv + (tile_hash(tile) - 0.5) * strength;
}

/// The last id this material can address: one short of the bound set's layer count, and
/// never past the UV-scale table. Every id is clamped through here exactly once, so the
/// layer sampled and the UV scale applied can never disagree.
public uint last_layer(uint layer_count) {
    return min(max(layer_count, 1), MAX_LAYERS) - 1;
}
