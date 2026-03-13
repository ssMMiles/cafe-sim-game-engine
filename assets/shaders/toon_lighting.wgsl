#import bevy_pbr::{
    pbr_fragment::pbr_input_from_vertex_output,
    pbr_bindings,
    mesh_view_bindings as view_bindings,
    mesh_view_types,
    shadows,
    clustered_forward as clustering,
}

#ifdef PREPASS_PIPELINE
#import bevy_pbr::{
    prepass_io::{VertexOutput, FragmentOutput},
    pbr_deferred_functions::deferred_output,
}
#else
#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::main_pass_post_lighting_processing,
}
#endif

@group(#{MATERIAL_BIND_GROUP}) @binding(100)
var lightmap_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101)
var lightmap_sampler: sampler;

// Perceptual luminance from a linear RGB color
fn luminance(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Quantize a scalar value into discrete toon bands
fn toon_band(value: f32, bands: f32) -> f32 {
    return floor(clamp(value, 0.0, 1.0) * bands) / bands;
}

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    let double_sided = false;
    var pbr_input = pbr_input_from_vertex_output(in, is_front, double_sided);
    // Read base color from the material uniform directly to avoid
    // pbr_input_from_standard_material's dependency on uv (VERTEX_UVS_A) when
    // only VERTEX_UVS_B is defined on the mesh.
    pbr_input.material.base_color = pbr_bindings::material.base_color;
    pbr_input.material.flags = pbr_bindings::material.flags;

#ifdef PREPASS_PIPELINE
    let out = deferred_output(in, pbr_input);
#else
    var out: FragmentOutput;

    let base_color = pbr_input.material.base_color.rgb;
    let N = normalize(pbr_input.N);

    let view_z = dot(vec4<f32>(
        view_bindings::view.view_from_world[0].z,
        view_bindings::view.view_from_world[1].z,
        view_bindings::view.view_from_world[2].z,
        view_bindings::view.view_from_world[3].z
    ), pbr_input.world_position);

    // Accumulate lighting in normalised [0, 1] space so toon bands span the full
    // angular range of the sun rather than saturating immediately.
    // Directional lights use pure NdotL (independent of physical lux units).
    var lighting = 0.0;

    // Directional lights
    let n_directional_lights = view_bindings::lights.n_directional_lights;
    for (var i: u32 = 0u; i < n_directional_lights; i = i + 1u) {
        let light = &view_bindings::lights.directional_lights[i];
        let NdotL = max(dot(N, (*light).direction_to_light), 0.0);

        var shadow = 1.0;
        if ((*light).flags & mesh_view_types::DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u {
            shadow = shadows::fetch_directional_shadow(i, pbr_input.world_position, pbr_input.world_normal, view_z);
        }

        lighting += NdotL * shadow;
    }

    // Point lights — use smooth range falloff in [0, 1] instead of raw lux attenuation
    let cluster_index = clustering::fragment_cluster_index(pbr_input.frag_coord.xy, view_z, pbr_input.is_orthographic);
    var clusterable_object_index_ranges = clustering::unpack_clusterable_object_index_ranges(cluster_index);

    for (var i: u32 = clusterable_object_index_ranges.first_point_light_index_offset;
         i < clusterable_object_index_ranges.first_spot_light_index_offset;
         i = i + 1u) {
        let light_id = clustering::get_clusterable_object_id(i);
        let light = &view_bindings::clusterable_objects.data[light_id];

        let light_to_frag = (*light).position_radius.xyz - pbr_input.world_position.xyz;
        let distance_squared = dot(light_to_frag, light_to_frag);
        let NdotL = max(dot(N, normalize(light_to_frag)), 0.0);

        let inverse_range_squared = (*light).color_inverse_square_range.w;
        let smooth_attenuation = 1.0 - min(distance_squared * inverse_range_squared, 1.0);

        var shadow = 1.0;
        if ((*light).flags & mesh_view_types::POINT_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u {
            shadow = shadows::fetch_point_shadow(light_id, pbr_input.world_position, pbr_input.world_normal);
        }

        lighting += NdotL * smooth_attenuation * smooth_attenuation * shadow;
    }

    // Sample the baked lightmap (shadow + AO packed into a single R channel).
    // uv_b is only present when VERTEX_UVS_B is defined (i.e. mesh has ATTRIBUTE_UV_1).
    // Before baking, meshes lack UV2 so we fall back to 1.0 (no-op).
#ifdef VERTEX_UVS_B
    let lm = textureSample(lightmap_texture, lightmap_sampler, in.uv_b).r;
#else
    let lm = 1.0;
#endif

    // Scale accumulated real-time lighting by the baked factor before quantization.
    let toon_factor = toon_band(lighting * lm, 5.0);

    // Ambient is a continuous floor so unlit areas aren't pitch black
    let ambient_floor = luminance(view_bindings::lights.ambient_color.rgb) * view_bindings::view.exposure;

    out.color = vec4<f32>(base_color * max(toon_factor, ambient_floor), pbr_input.material.base_color.a);
#endif

    return out;
}
