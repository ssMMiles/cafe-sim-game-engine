//! Compile the material shaders' WESL in `cargo test`.
//!
//! The material shaders only become WGSL inside bevy's resolver at runtime,
//! so a typo used to surface as a pipeline error and a missing mesh, never in
//! a test. This runs the same `wesl::compile` bevy does (same options), with
//! a resolver that serves our shader files plus small stand-ins for the few
//! `bevy_pbr` modules they import, and validates the result with naga for
//! **every combination** of the shader defs each shader branches on.
//!
//! The stand-ins copy the declarations we use verbatim from bevy 0.20
//! (bevy_pbr's `render/forward_io.wesl`, `prepass/io.wesl`,
//! `render/mesh_types.wesl`, `render/mesh_bindings.wesl`; bevy_render's
//! `view.wesl`); if a bevy upgrade changes those, update them here. What this
//! can't catch: WebGPU's uniformity rule (naga skips it for fragment shaders —
//! set `DUMP_WGSL=<dir>` and run the output through `tools/tint-check.mjs`),
//! and a stand-in that no longer matches bevy.
//!
//! Module paths mirror the asset layout, because that is what the runtime
//! resolver derives them from: `assets/shaders/scene/terrain_io.wesl` is
//! `package::shaders::scene::terrain_io`, and our shaders import each other by
//! that name. Only bevy's own modules are stubbed.

use std::borrow::Cow;

use wesl::{
    CompileOptions, Feature, Resolver,
    error::ResolveError,
    syntax::{ModulePath, PathOrigin},
};

const TOON: &str = include_str!("../../../assets/shaders/toon_lighting.wesl");
const TOON_PREPASS: &str = include_str!("../../../assets/shaders/toon_prepass.wesl");
const TERRAIN: &str = include_str!("../../../assets/shaders/terrain_toon.wesl");
const SCENE_BINDINGS: &str = include_str!("../../../assets/shaders/scene/bindings.wesl");
const TERRAIN_IO: &str = include_str!("../../../assets/shaders/scene/terrain_io.wesl");
const TERRAIN_VERTEX: &str = include_str!("../../../assets/shaders/scene/terrain_vertex.wesl");
const TERRAIN_PREPASS: &str = include_str!("../../../assets/shaders/scene/terrain_prepass.wesl");

/// bevy_render's `view.wesl`, cut down to the fields the scene pass reads.
/// The *layout* can't drift unnoticed: the bind group entry is built from the
/// Rust `ViewUniform` and the runtime shader imports bevy's real module, so
/// only this test sees the short version.
const RENDER_VIEW: &str = "
struct View {
    clip_from_world: mat4x4<f32>,
    world_position: vec3<f32>,
}
";

const FORWARD_IO: &str = "
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @if(VERTEX_UVS_A)
    @location(2) uv: vec2<f32>,
    @if(VERTEX_UVS_B)
    @location(3) uv_b: vec2<f32>,
    @if(VERTEX_TANGENTS)
    @location(4) world_tangent: vec4<f32>,
    @if(VERTEX_COLORS)
    @location(5) color: vec4<f32>,
    @if(VERTEX_OUTPUT_INSTANCE_INDEX)
    @location(6) @interpolate(flat) instance_index: u32,
    @if(VISIBILITY_RANGE_DITHER)
    @location(7) @interpolate(flat) visibility_range_dither: i32,
}

struct FragmentOutput {
    @location(0) color: vec4<f32>,
}
";

const PREPASS_IO: &str = "
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @if(VERTEX_UVS_A)
    @location(0) uv: vec2<f32>,
    @if(VERTEX_UVS_B)
    @location(1) uv_b: vec2<f32>,
    @if(NORMAL_PREPASS_OR_DEFERRED_PREPASS)
    @location(2) world_normal: vec3<f32>,
    @if(NORMAL_PREPASS_OR_DEFERRED_PREPASS && VERTEX_TANGENTS)
    @location(3) world_tangent: vec4<f32>,
    @location(4) world_position: vec4<f32>,
    @if(MOTION_VECTOR_PREPASS)
    @location(5) previous_world_position: vec4<f32>,
    @if(UNCLIPPED_DEPTH_ORTHO_EMULATION)
    @location(6) unclipped_depth: f32,
    @if(VERTEX_OUTPUT_INSTANCE_INDEX)
    @location(7) @interpolate(flat) instance_index: u32,
    @if(VERTEX_COLORS)
    @location(8) color: vec4<f32>,
    @if(VISIBILITY_RANGE_DITHER)
    @location(9) @interpolate(flat) visibility_range_dither: i32,
}

@if(PREPASS_FRAGMENT)
struct FragmentOutput {
    @if(NORMAL_PREPASS)
    @location(0) normal: vec4<f32>,
    @if(MOTION_VECTOR_PREPASS)
    @location(1) motion_vector: vec2<f32>,
    @if(DEFERRED_PREPASS)
    @location(2) deferred: vec4<u32>,
    @if(DEFERRED_PREPASS)
    @location(3) deferred_lighting_pass_id: u32,
    @if(UNCLIPPED_DEPTH_ORTHO_EMULATION)
    @builtin(frag_depth) frag_depth: f32,
}
";

const MESH_TYPES: &str = "
struct Mesh {
    world_from_local: mat3x4<f32>,
    previous_world_from_local: mat3x4<f32>,
    local_from_world_transpose_a: mat2x4<f32>,
    local_from_world_transpose_b: f32,
    flags: u32,
    lightmap_uv_rect: vec2<u32>,
    first_vertex_index: u32,
    current_skin_index: u32,
    material_and_lightmap_bind_group_slot: u32,
    tag: u32,
    morph_descriptor_index: u32,
    metadata_index: u32,
};

const MESH_FLAGS_SIGN_DETERMINANT_MODEL_3X3_BIT: u32 = 1u << 31u;
";

const MESH_BINDINGS: &str = "
import bevy_pbr::render::mesh_types::Mesh;
@group(2) @binding(0) var<storage> mesh: array<Mesh>;
";

/// bevy's `constants` package: one `const` per integer shader def, built by
/// the runtime resolver from the pipeline's `ShaderDefVal::UInt`s. The value
/// is 3 under bevy's mesh pipeline (toon) and 1 under ours (terrain,
/// `scene_pass::pipeline`); which one it is changes nothing a compile can
/// check, so the stub picks one.
const CONSTANTS: &str = "const MATERIAL_BIND_GROUP = 3;\n";

struct Stubs;

impl Resolver for Stubs {
    fn resolve_source<'a>(&'a self, path: &ModulePath) -> Result<Cow<'a, str>, ResolveError> {
        let source = match path.to_string().as_str() {
            "package::shaders::toon_lighting" => TOON,
            "package::shaders::toon_prepass" => TOON_PREPASS,
            "package::shaders::terrain_toon" => TERRAIN,
            "package::shaders::scene::bindings" => SCENE_BINDINGS,
            "package::shaders::scene::terrain_io" => TERRAIN_IO,
            "package::shaders::scene::terrain_vertex" => TERRAIN_VERTEX,
            "package::shaders::scene::terrain_prepass" => TERRAIN_PREPASS,
            "bevy_pbr::render::forward_io" => FORWARD_IO,
            "bevy_pbr::prepass::io" => PREPASS_IO,
            "bevy_pbr::render::mesh_types" => MESH_TYPES,
            "bevy_pbr::render::mesh_bindings" => MESH_BINDINGS,
            "bevy_render::view" => RENDER_VIEW,
            "constants" => CONSTANTS,
            other => {
                return Err(ResolveError::ModuleNotFound(
                    path.clone(),
                    format!("`{other}` has no stand-in in shader_check.rs"),
                ));
            }
        };
        Ok(Cow::Borrowed(source))
    }
}

/// Compile `module` with `defs` on (everything else off) and validate it.
fn check(module: &str, defs: &[&str]) -> Result<String, String> {
    let mut options = CompileOptions {
        imports: true,
        condcomp: true,
        visibility: false,
        ..Default::default()
    };
    for def in defs {
        options
            .features
            .flags
            .insert(def.to_string(), Feature::Enable);
    }
    let path = ModulePath::new(
        PathOrigin::Absolute,
        module.split("::").map(str::to_string).collect(),
    );
    let wgsl = wesl::compile(&path, &options, &Stubs)
        .map_err(|e| format!("WESL: {}", e.diagnostic().render_plain()))?
        .to_string();
    let parsed = naga::front::wgsl::parse_str(&wgsl).map_err(|e| e.emit_to_string(&wgsl))?;
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&parsed)
    .map_err(|e| format!("naga: {e:?}"))?;
    Ok(wgsl)
}

/// Every subset of `toggled`, each with `always` on too.
fn check_all(module: &str, always: &[&str], toggled: &[&str]) {
    let mut failures = Vec::new();
    for mask in 0..(1u32 << toggled.len()) {
        let mut defs = always.to_vec();
        defs.extend(
            toggled
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, d)| *d),
        );
        match check(module, &defs) {
            Ok(wgsl) => {
                if let Some(dir) = std::env::var_os("DUMP_WGSL") {
                    let name = format!(
                        "{}-{}.wgsl",
                        module.replace("::", "-"),
                        defs.join("-").to_lowercase()
                    );
                    std::fs::write(std::path::Path::new(&dir).join(name), wgsl).unwrap();
                }
            }
            Err(e) => failures.push(format!("{module} with {defs:?}:\n{e}")),
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// Mesh-pipeline defs bevy always sets for these passes.
const MESH_PIPELINE: &[&str] = &["MESH_PIPELINE", "VERTEX_OUTPUT_INSTANCE_INDEX"];

#[test]
fn toon_colour_pass_compiles_for_every_variant() {
    check_all(
        "shaders::toon_lighting",
        MESH_PIPELINE,
        &[
            "VERTEX_UVS_A",
            "VERTEX_UVS_B",
            engine_api::TOON_DOUBLE_SIDED_DEF,
        ],
    );
}

/// The prepass fragment only exists when bevy sets `PREPASS_FRAGMENT`, which
/// it does alongside the normal prepass (our case), motion vectors, or
/// unclipped-depth emulation — never alone, so a variant with no output at
/// all is not a real pipeline. `NORMAL_PREPASS_OR_DEFERRED_PREPASS` comes
/// with the normal prepass.
#[test]
fn toon_prepass_compiles_for_every_variant() {
    let double_sided = engine_api::TOON_DOUBLE_SIDED_DEF;
    let with = |extra: &[&'static str]| {
        let mut defs = MESH_PIPELINE.to_vec();
        defs.push("PREPASS_FRAGMENT");
        defs.extend_from_slice(extra);
        defs
    };
    check_all(
        "shaders::toon_prepass",
        &with(&["NORMAL_PREPASS", "NORMAL_PREPASS_OR_DEFERRED_PREPASS"]),
        &[
            double_sided,
            "UNCLIPPED_DEPTH_ORTHO_EMULATION",
            "MOTION_VECTOR_PREPASS",
        ],
    );
    check_all(
        "shaders::toon_prepass",
        &with(&["UNCLIPPED_DEPTH_ORTHO_EMULATION"]),
        &[double_sided, "MOTION_VECTOR_PREPASS"],
    );
    check_all(
        "shaders::toon_prepass",
        &with(&["MOTION_VECTOR_PREPASS"]),
        &[double_sided],
    );
}

/// Terrain's three stages, as the scene pass compiles them: one `const` def
/// for the material bind group and nothing else. Every variant *is* the one
/// variant — dropping bevy's mesh pipeline took the whole def matrix with it,
/// which is half the point of owning the pass.
#[test]
fn terrain_scene_pass_shaders_compile() {
    for module in [
        "shaders::scene::terrain_vertex",
        "shaders::scene::terrain_prepass",
        "shaders::terrain_toon",
    ] {
        check_all(module, &[], &[]);
    }
}

/// The harness itself: a shader that is wrong must fail, or the tests above
/// prove nothing.
#[test]
fn a_broken_shader_is_caught() {
    assert!(check("missing_module", &[]).is_err());
    // `toon_prepass` reads `in.world_normal` under NORMAL_PREPASS, which only
    // exists with NORMAL_PREPASS_OR_DEFERRED_PREPASS: leaving that off must
    // fail to compile.
    assert!(
        check(
            "shaders::toon_prepass",
            &[
                "PREPASS_FRAGMENT",
                "VERTEX_OUTPUT_INSTANCE_INDEX",
                "NORMAL_PREPASS"
            ]
        )
        .is_err()
    );
}
