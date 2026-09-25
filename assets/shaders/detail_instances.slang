// Instanced ground detail -- grass and the like -- deformed into a vertex stream.
//
// `detail.wgsl` did this in a vertex shader. A ray tracer has no vertex stage: geometry
// has to exist in a buffer before the BLAS is built over it, so the animation moves into a
// compute kernel exactly as aurora's own `terrain_vertices` and `skinning.slang` do. One
// thread per output vertex; the host rebuilds or refits the BLAS afterwards.
//
// What the move changes, and it is worth knowing: the distance fade reads the CAMERA's
// position, so the geometry itself is view-dependent. In a raster pass that is invisible.
// In a path tracer a reflection or a shadow ray sees blades sized for the camera rather
// than for itself -- correct enough at grazing distances, wrong if a mirror looks at
// far-away grass. Keeping the fade means accepting that; dropping it means paying for
// every blade to the cull distance in every ray.

// Deformed vertex stream: `Vertex` in aurora's blas.rs (32 bytes, scalar layout).
struct Vertex {
    float px, py, pz;
    float nx, ny, nz;
    float u, v;
}

/// One blade's placement. `packed` carries four 0..255 variation dials: height, width,
/// yaw and tint.
struct Instance {
    float3 position;
    uint packed;
    /// Ground normal's XZ; Y is recovered as `sqrt(1 - dot(t, t))`.
    float2 tilt;
    float2 pad;
}

/// Matches the raster path's cap; the host never queues more than this.
static const uint MAX_PRESSERS = 16;
/// Darkest the tint byte takes an instance, as a fraction of its own colour.
static const float DIMMEST = 0.7;
/// Fraction of the cull distance the instances spend shrinking into the ground.
static const float FADE_BAND = 0.25;
static const float TAU = 6.2831855;
/// How far a breeze of strength 1 leans a tip responding at 1, in world units.
/// `DetailLayer::BREEZE_LEAN` on the Rust side is the same number.
static const float BREEZE_LEAN = 0.12;
/// How far a tip bobs up and down against how far it leans sideways.
static const float VERTICAL_SHARE = 0.333;
/// How much wider than the sway the gust swell is read, and how much slower it travels.
static const float GUST_SPAN = 0.25;
static const float GUST_DRIFT = 0.5;

/// The dials that change when the layer is edited, not per frame. In a buffer because
/// push constants are 128 bytes total (`compute::COMPUTE_PUSH_CONSTANT_SIZE`) and the
/// pointers alone take a third of that.
struct DetailSettings {
    float3 color_base;
    float pad0;
    float3 color_tip;
    float pad1;

    float2 wind_direction;
    float wind_strength;
    float wind_gust;

    float wind_gust_speed;
    float wind_turbulence_scale;
    float wind_response;
    float bend;

    float2 height_range;
    float2 width_range;
}

/// Exactly 96 bytes: pointers, the per-frame scalars, and the counts. Anything that is not
/// one of those belongs in `settings` -- the whole budget is 128.
struct DetailParams {
    /// The blade template: `verts_per_instance` vertices, reused by every instance.
    Vertex* mesh;
    Instance* instances;
    /// Deformed output, `instance_count * verts_per_instance` long.
    Vertex* out_vertices;
    /// Wind noise as a raw R8 byte buffer, `noise_res` square, wrapped.
    uint* wind_noise;
    /// `presser_count` entries: xyz = where, w = reach.
    float4* pressers;
    DetailSettings* settings;

    /// Where the camera stands, for the distance fade.
    float3 viewer;
    float time;

    uint instance_count;
    uint verts_per_instance;
    uint presser_count;
    uint base;

    uint noise_res;
    float cull_distance;
    float push_strength;
    uint pad;
}

[[vk::push_constant]] DetailParams params;

float3 rotate_y(float3 v, float yaw) {
    let s = sin(yaw);
    let c = cos(yaw);
    return float3(v.x * c + v.z * s, v.y, v.z * c - v.x * s);
}

/// `v` turned by the rotation that takes straight up onto `up`.
float3 tilt_toward(float3 v, float3 up) {
    let axis = cross(float3(0.0, 1.0, 0.0), up);
    let span = length(axis);
    if (span < 1e-5) {
        return v;
    }
    let unit_axis = axis / span;
    let angle = atan2(span, clamp(up.y, -1.0, 1.0));
    let c = cos(angle);
    return v * c + cross(unit_axis, v) * sin(angle) + unit_axis * dot(unit_axis, v) * (1.0 - c);
}

/// Bilinear read of the wrapped noise buffer, standing in for the sampler the raster path
/// had. Point sampling would show the noise grid as banding in the sway.
float noise_at(float2 uv) {
    let res = max(params.noise_res, 1u);
    let p = uv * float(res) - 0.5;
    let i = floor(p);
    let f = p - i;
    float c[4];
    for (int k = 0; k < 4; ++k) {
        let x = uint(((int(i.x) + (k & 1)) % int(res) + int(res)) % int(res));
        let y = uint(((int(i.y) + (k >> 1)) % int(res) + int(res)) % int(res));
        c[k] = float(params.wind_noise[y * res + x] & 0xff) / 255.0;
    }
    return lerp(lerp(c[0], c[1], f.x), lerp(c[2], c[3], f.x), f.y);
}

/// The wind pattern where an instance stands, in -1..1. Sampled at the instance's FOOT,
/// not per vertex: a blade leans as one piece.
///
/// The gust dial mixes in a second, wider reading of the same field travelling the other
/// way, which swells and drops the sway rather than speeding it up.
float wind_at(float2 world_xz, DetailSettings cfg) {
    let travel = cfg.wind_direction * params.time * cfg.wind_gust_speed;
    let uv = world_xz / max(cfg.wind_turbulence_scale, 0.001) + travel;
    let sway = noise_at(uv) * 2.0 - 1.0;
    let swell = noise_at(uv * GUST_SPAN - travel * GUST_DRIFT);
    return sway * lerp(1.0, swell, cfg.wind_gust);
}

[shader("compute")]
[numthreads(64, 1, 1)]
void detail_vertices(uint3 gid: SV_DispatchThreadID) {
    let i = params.base + gid.x;
    let total = params.instance_count * params.verts_per_instance;
    if (i >= total) {
        return;
    }
    let inst_index = i / params.verts_per_instance;
    let vert_index = i % params.verts_per_instance;

    let cfg = params.settings[0];
    let inst = params.instances[inst_index];
    let src = params.mesh[vert_index];

    let variation = float4(
        float(inst.packed & 0xff),
        float((inst.packed >> 8) & 0xff),
        float((inst.packed >> 16) & 0xff),
        float((inst.packed >> 24) & 0xff)) / 255.0;

    // How far up the blade this vertex sits. The template is authored 0..1 in Y, so its
    // own height IS the fraction.
    let up_the_mesh = src.py;
    let base = inst.position;

    let to_viewer = distance(params.viewer, base);
    let fade_start = params.cull_distance * (1.0 - FADE_BAND);
    let fade = 1.0 - smoothstep(fade_start, params.cull_distance, to_viewer);

    let height = lerp(cfg.height_range.x, cfg.height_range.y, variation.x) * fade;
    let width = lerp(cfg.width_range.x, cfg.width_range.y, variation.y);
    let yaw = variation.z * TAU;

    let bend_gathered_toward_the_top = up_the_mesh * up_the_mesh * cfg.bend;
    let local = float3(
        src.px * width,
        up_the_mesh * height,
        src.pz * width + bend_gathered_toward_the_top);
    // The blade's own normal bent toward straight up as it climbs, so a tip takes the
    // light the ground under it takes rather than the light its own facet would.
    let normal = normalize(lerp(float3(src.nx, src.ny, src.nz), float3(0.0, 1.0, 0.0), up_the_mesh));

    let ground_normal = float3(inst.tilt.x,
                               sqrt(max(1.0 - dot(inst.tilt, inst.tilt), 0.0)),
                               inst.tilt.y);
    float3 offset = tilt_toward(rotate_y(local, yaw), ground_normal);
    let world_normal = tilt_toward(rotate_y(normal, yaw), ground_normal);

    let wind = wind_at(base.xz, cfg);
    let along = normalize(cfg.wind_direction + float2(1e-6, 0.0));
    let leaned = wind * cfg.wind_strength * cfg.wind_response * BREEZE_LEAN * up_the_mesh;
    offset += float3(along.x * leaned, abs(leaned) * VERTICAL_SHARE, along.y * leaned);

    // Pressers flatten what stands under them -- a foot, a wheel, a dropped crate.
    let pressers = min(params.presser_count, MAX_PRESSERS);
    for (uint k = 0; k < pressers; ++k) {
        let presser = params.pressers[k];
        let away = base - presser.xyz;
        let flat_away = float2(away.x, away.z);
        let reach = max(presser.w, 0.001);
        let strength = 1.0 - smoothstep(0.0, reach, length(flat_away));
        if (strength > 0.0) {
            let direction = normalize(float3(flat_away.x, 0.0, flat_away.y) + float3(1e-6, 0.0, 0.0));
            let pushed = strength * params.push_strength * up_the_mesh;
            offset += direction * pushed;
            offset.y -= pushed * 0.5;
        }
    }

    let world = base + offset;
    let n = normalize(world_normal);

    Vertex outv;
    outv.px = world.x;
    outv.py = world.y;
    outv.pz = world.z;
    outv.nx = n.x;
    outv.ny = n.y;
    outv.nz = n.z;
    // The raster path carried the base-to-tip tint and the per-instance dimming down to
    // the fragment shader as an interpolated colour. There is no interpolator here, so it
    // rides in the UV channel's second slot and the material reads it as a gradient key --
    // `u` keeps the template's own coordinate for the blade texture.
    outv.u = src.u;
    outv.v = up_the_mesh * lerp(DIMMEST, 1.0, variation.w);
    params.out_vertices[i] = outv;
}
