#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::alpha_discard,
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

struct ToonMaterial {
    color: vec4<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> toon_material: ToonMaterial;

// Helper function to quantize a color into discrete bands
fn quantize_color(color: vec3<f32>, bands: f32) -> vec3<f32> {
    return vec3<f32>(
        floor(color.r * bands) / bands,
        floor(color.g * bands) / bands,
        floor(color.b * bands) / bands
    );
}

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    // Generate a PbrInput struct from the StandardMaterial bindings
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    // Override base color with our toon color
    pbr_input.material.base_color = toon_material.color;

    // Alpha discard
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

#ifdef PREPASS_PIPELINE
    // In deferred mode, we can't modify anything after this
    let out = deferred_output(in, pbr_input);
#else
    var out: FragmentOutput;

    // Custom view-independent toon lighting
    let base_color = pbr_input.material.base_color.rgb;
    var lit_color = vec3<f32>(0.0);

    // Calculate view space Z for shadow sampling
    let view_z = dot(vec4<f32>(
        view_bindings::view.view_from_world[0].z,
        view_bindings::view.view_from_world[1].z,
        view_bindings::view.view_from_world[2].z,
        view_bindings::view.view_from_world[3].z
    ), pbr_input.world_position);

    // Process directional lights (view-independent Lambertian diffuse)
    let n_directional_lights = view_bindings::lights.n_directional_lights;
    for (var i: u32 = 0u; i < n_directional_lights; i = i + 1u) {
        let light = &view_bindings::lights.directional_lights[i];

        // Simple Lambertian diffuse (N·L) - no view dependency
        let L = (*light).direction_to_light;
        let N = normalize(pbr_input.N);
        let NdotL = max(dot(N, L), 0.0);

        // Calculate diffuse lighting
        let diffuse = base_color * (*light).color.rgb * NdotL;

        // Sample shadows
        var shadow = 1.0;
        if ((*light).flags & mesh_view_types::DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u {
            shadow = shadows::fetch_directional_shadow(
                i,
                pbr_input.world_position,
                pbr_input.world_normal,
                view_z
            );
        }

        // Quantize this light's contribution before accumulating
        let light_contribution = diffuse * shadow;
        lit_color += quantize_color(light_contribution, 5.0);
    }

    // Process point lights using clustering
    let cluster_index = clustering::fragment_cluster_index(
        pbr_input.frag_coord.xy,
        view_z,
        pbr_input.is_orthographic
    );
    var clusterable_object_index_ranges =
        clustering::unpack_clusterable_object_index_ranges(cluster_index);

    // Iterate through point lights in this cluster
    for (var i: u32 = clusterable_object_index_ranges.first_point_light_index_offset;
         i < clusterable_object_index_ranges.first_spot_light_index_offset;
         i = i + 1u) {
        let light_id = clustering::get_clusterable_object_id(i);
        let light = &view_bindings::clusterable_objects.data[light_id];

        // Calculate light direction and distance
        let light_position = (*light).position_radius.xyz;
        let light_to_frag = light_position - pbr_input.world_position.xyz;
        let distance_squared = dot(light_to_frag, light_to_frag);
        let L = normalize(light_to_frag);
        let N = normalize(pbr_input.N);

        // Simple Lambertian diffuse
        let NdotL = max(dot(N, L), 0.0);

        // Distance attenuation
        let light_color = (*light).color_inverse_square_range.rgb;
        let inverse_range_squared = (*light).color_inverse_square_range.w;
        let attenuation = 1.0 / max(distance_squared, 0.0001);
        let smooth_attenuation = 1.0 - min(distance_squared * inverse_range_squared, 1.0);
        let range_attenuation = attenuation * smooth_attenuation * smooth_attenuation;

        // Calculate diffuse lighting
        let diffuse = base_color * light_color * NdotL * range_attenuation;

        // Sample shadows
        var shadow = 1.0;
        if ((*light).flags & mesh_view_types::POINT_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u {
            shadow = shadows::fetch_point_shadow(
                light_id,
                pbr_input.world_position,
                pbr_input.world_normal
            );
        }

        // Quantize this light's contribution before accumulating
        let light_contribution = diffuse * shadow;
        lit_color += quantize_color(light_contribution, 5.0);
    }

    // Add ambient light (quantized separately)
    let ambient_contribution = base_color * view_bindings::lights.ambient_color.rgb;
    lit_color += quantize_color(ambient_contribution, 5.0);

    // Note: Each light is now quantized individually before accumulation,
    // so we don't need to quantize the final result. This creates more
    // distinct light layers and reduces muddy transitions.

    out.color = vec4<f32>(lit_color, pbr_input.material.base_color.a);
#endif

    return out;
}