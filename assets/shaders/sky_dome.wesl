// The sky dome: a gradient between two colours derived from the level's
// lighting, a sun disc with a halo, and a dark floor below the horizon. All
// the colour decisions are made on the CPU (`SkyColors::from_environment`,
// world/sky.rs), already exposed through the toon curve's `white`; this
// shader only places them on the sphere.
//
// Drawn by our own scene pass (`src/game/gfx/scene_pass`) in the *background*
// layer: after the opaque geometry, writing no depth, and with no prepass
// variant at all — which is what keeps the fog and god-ray pass off the sky
// (it reconstructs position from prepass depth, and the sky leaves none).

import package::shaders::scene::mesh_io::{VertexOutput, FragmentOutput};

// Mirrors `SkyColors` in world/sky.rs field for field.
struct SkyColors {
    horizon: vec4<f32>,
    zenith: vec4<f32>,
    ground: vec4<f32>,
    // Unit direction toward the sun.
    sun_dir: vec4<f32>,
    // rgb: the disc's colour; w: halo strength.
    sun: vec4<f32>,
};
@group(constants::MATERIAL_BIND_GROUP) @binding(0)
var<uniform> sky: SkyColors;

// Cosine of the disc's angular radius: a stylised ~2°, several times the real
// sun's, with the edge resolved over a tenth of that.
const SUN_DISC_OUTER: f32 = 0.9990;
const SUN_DISC_INNER: f32 = 0.9996;

@fragment
fn fragment(in: VertexOutput) -> FragmentOutput {
    // The dome is a UV sphere centred on the camera, and a UV sphere's
    // normals are radial, so the world normal *is* the view direction. The
    // old `world_position.y / radius` put the horizon at world y = 0 rather
    // than at eye level, which tilted the sky whenever the camera climbed.
    let dir = normalize(in.world_normal);
    let up = clamp(dir.y, 0.0, 1.0);

    // Most of a sky's change happens low down: horizon colour holds for a
    // while, then the zenith takes over.
    var color = mix(sky.horizon.rgb, sky.zenith.rgb, pow(up, 0.6));

    // A hard-edged disc, in keeping with the banded surfaces, and a soft
    // glow around it that the horizon haze blends into.
    let c = dot(dir, sky.sun_dir.xyz);
    let disc = smoothstep(SUN_DISC_OUTER, SUN_DISC_INNER, c);
    let halo = pow(max(c, 0.0), 16.0) * sky.sun.w;
    color = color + sky.sun.rgb * (disc + halo);

    // Below the horizon: hidden by terrain almost everywhere, dark where it
    // isn't. `smoothstep` edges must be ordered low < high or the result is
    // undefined, so the fade is written in terms of -dir.y.
    color = mix(color, sky.ground.rgb, smoothstep(0.0, 0.08, -dir.y));

    var out: FragmentOutput;
    out.color = vec4<f32>(color, 1.0);
    // Never written: the background layer's pipeline masks the normal target
    // off, so the sky leaves no normal (and no depth) for the edge detector
    // or the atmosphere to see.
@if(SCENE_NORMAL_TARGET)
    out.normal = vec4<f32>(0.0);
    return out;
}
