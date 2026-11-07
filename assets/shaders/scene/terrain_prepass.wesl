// Terrain prepass fragment: the world normal the edge detector reads
// (docs/EDGE_DETECTION.md). Depth needs no fragment work — this runs only
// because the camera has a normal prepass.
//
// Terrain is never double-sided, so there is no front/back flip to do here
// (contrast `toon_prepass.wesl`). The skirt normal is fixed up in the vertex
// stage, which is where it has to happen: the skirts' zero normals come out
// of the mesh generator.
//
// Packing matches every other prepass writer: world space, [-1, 1] mapped
// into the `Rgb10a2Unorm` target's [0, 1]. The reader normalises.

import package::shaders::scene::terrain_io::{VertexOutput, PrepassOutput};

@fragment
fn fragment(in: VertexOutput) -> PrepassOutput {
    var out: PrepassOutput;
    out.normal = vec4<f32>(in.world_normal * 0.5 + vec3<f32>(0.5), 1.0);
    return out;
}
