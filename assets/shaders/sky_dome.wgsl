#import bevy_pbr::forward_io::VertexOutput

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // world_position.y: 0 = horizon, 900 = zenith
    let sky_t = clamp(in.world_position.y / 900.0, 0.0, 1.0);

    let horizon_haze = vec3(0.85, 0.80, 0.72); // warm peachy haze
    let pale_sky     = vec3(0.65, 0.78, 0.92); // pale blue
    let mid_sky      = vec3(0.35, 0.60, 0.85); // mid blue
    let deep_sky     = vec3(0.15, 0.35, 0.75); // deep zenith blue

    // Smooth 4-stop gradient: horizon → pale → mid → deep
    var color: vec3<f32>;
    if sky_t < 0.33 {
        color = mix(horizon_haze, pale_sky, sky_t / 0.33);
    } else if sky_t < 0.66 {
        color = mix(pale_sky, mid_sky, (sky_t - 0.33) / 0.33);
    } else {
        color = mix(mid_sky, deep_sky, (sky_t - 0.66) / 0.34);
    }

    // Below horizon — earthy undertone (hidden by terrain)
    if in.world_position.y < -2000.0 {
        color = vec3(0.55, 0.45, 0.35);
    }

    return vec4(color, 1.0);
}
