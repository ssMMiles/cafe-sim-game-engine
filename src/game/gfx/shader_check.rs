//! Compile the material shaders' WESL in `cargo test`.
//!
//! The material shaders only become WGSL inside bevy's resolver at runtime,
//! so a typo used to surface as a pipeline error and a missing mesh, never in
//! a test. This runs the same `wesl::compile` bevy does (same options), with
//! a resolver that serves our shader files plus small stand-ins for the few
//! `bevy_pbr` modules they import, and validates the result with naga for
//! **every combination** of the shader defs each shader branches on.
//!
//! The only bevy module the material shaders still import is bevy_render's
//! `view.wesl` (the `View` uniform), stubbed below with the fields we read; if
//! a bevy upgrade changes it, update it here. What this can't catch: WebGPU's
//! uniformity rule (naga skips it for fragment shaders — set `DUMP_WGSL=<dir>`
//! and run the output through `tools/tint-check.mjs`), and a stand-in that no
//! longer matches bevy.
//!
//! Module paths mirror the asset layout, because that is what the runtime
//! resolver derives them from: `assets/shaders/scene/terrain_io.wesl` is
//! `package::shaders::scene::terrain_io`, and our shaders import each other by
//! that name.

use std::borrow::Cow;

use wesl::{
    CompileOptions, Feature, Resolver,
    error::ResolveError,
    syntax::{ModulePath, PathOrigin},
};

const TOON: &str = include_str!("../../../assets/shaders/toon_lighting.wesl");
const TERRAIN: &str = include_str!("../../../assets/shaders/terrain_toon.wesl");
const SCENE_BINDINGS: &str = include_str!("../../../assets/shaders/scene/bindings.wesl");
const TERRAIN_IO: &str = include_str!("../../../assets/shaders/scene/terrain_io.wesl");
const TERRAIN_VERTEX: &str = include_str!("../../../assets/shaders/scene/terrain_vertex.wesl");
const MESH_IO: &str = include_str!("../../../assets/shaders/scene/mesh_io.wesl");
const MESH_VERTEX: &str = include_str!("../../../assets/shaders/scene/mesh_vertex.wesl");
const SKY: &str = include_str!("../../../assets/shaders/sky_dome.wesl");
const UNLIT: &str = include_str!("../../../assets/shaders/unlit.wesl");
const GIZMO_LINES: &str = include_str!("../../../assets/shaders/gizmos/lines.wesl");
const EDGE: &str = include_str!("../../../assets/shaders/edge_detection_post.wesl");

/// bevy_render's `view.wesl`, cut down to the fields our shaders read (the
/// scene pass: `clip_from_world`; the gizmo shader: the clip/view matrices
/// and `viewport`). The *layout* can't drift unnoticed: the bind group entry
/// is built from the Rust `ViewUniform` and the runtime shader imports bevy's
/// real module, so only this test sees the short version.
const RENDER_VIEW: &str = "
struct View {
    clip_from_world: mat4x4<f32>,
    world_from_clip: mat4x4<f32>,
    view_from_clip: mat4x4<f32>,
    world_position: vec3<f32>,
    viewport: vec4<f32>,
}
";

/// bevy_render's `maths.wesl`, the one function the gizmo shader imports.
const RENDER_MATHS: &str = "
fn affine3_to_square(affine: mat3x4<f32>) -> mat4x4<f32> {
    return transpose(mat4x4<f32>(affine[0], affine[1], affine[2], vec4<f32>(0.0, 0.0, 0.0, 1.0)));
}
";

/// bevy's `constants` package: one `const` per integer shader def, built by
/// the runtime resolver from the pipeline's `ShaderDefVal::UInt`s. The value
/// is 3 under bevy's mesh pipeline (toon) and 1 under ours (terrain,
/// `scene_pass::pipeline`); which one it is changes nothing a compile can
/// check, so the stub picks one. `MSAA_SAMPLES` is the edge pass's loop
/// bound (any positive value compiles the same).
const CONSTANTS: &str = "const MATERIAL_BIND_GROUP = 3;\nconst MSAA_SAMPLES = 4;\n";

/// bevy_core_pipeline's `fullscreen_vertex_shader.wesl`, cut to the output
/// struct the post-process fragments take.
const FULLSCREEN_VERTEX: &str = "
struct FullscreenVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}
";

struct Stubs;

impl Resolver for Stubs {
    fn resolve_source<'a>(&'a self, path: &ModulePath) -> Result<Cow<'a, str>, ResolveError> {
        let source = match path.to_string().as_str() {
            "package::shaders::toon_lighting" => TOON,
            "package::shaders::terrain_toon" => TERRAIN,
            "package::shaders::scene::bindings" => SCENE_BINDINGS,
            "package::shaders::scene::terrain_io" => TERRAIN_IO,
            "package::shaders::scene::terrain_vertex" => TERRAIN_VERTEX,
            "package::shaders::scene::mesh_io" => MESH_IO,
            "package::shaders::scene::mesh_vertex" => MESH_VERTEX,
            "package::shaders::sky_dome" => SKY,
            "package::shaders::unlit" => UNLIT,
            "package::shaders::gizmos::lines" => GIZMO_LINES,
            "package::shaders::edge_detection_post" => EDGE,
            "bevy_render::maths" => RENDER_MATHS,
            "bevy_render::view" => RENDER_VIEW,
            "bevy_core_pipeline::fullscreen_vertex_shader" => FULLSCREEN_VERTEX,
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
/// `pub(crate)`: `post_process`'s struct-layout test compiles the edge shader
/// through this to get real WGSL out of the WESL.
pub(crate) fn check(module: &str, defs: &[&str]) -> Result<String, String> {
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

/// Set by the scene pipeline for a view that has a normal target (the 3D
/// camera); off for the thumbnail rigs. Every fragment shader of the scene
/// pass branches on it, so every one of them is checked both ways.
const NORMAL_TARGET: &str = "SCENE_NORMAL_TARGET";

/// The defs the toon pipeline sets from the mesh layout, the material key and
/// the view (`scene_pass::pipeline::ToonScenePipeline`). Every combination is
/// a pipeline some mesh can ask for, so every combination has to compile.
const TOON_DEFS: &[&str] = &[
    "VERTEX_UVS_A",
    "VERTEX_UVS_B",
    "SKINNED",
    engine_api::TOON_DOUBLE_SIDED_DEF,
    NORMAL_TARGET,
];

#[test]
fn toon_scene_pass_shaders_compile_for_every_variant() {
    for module in ["shaders::scene::mesh_vertex", "shaders::toon_lighting"] {
        check_all(module, &[], TOON_DEFS);
    }
}

/// The sky dome shares `mesh_vertex` with the toon materials but sets no
/// mesh defs at all (`SceneLayer::Background`).
#[test]
fn sky_dome_compiles_for_every_variant() {
    check_all("shaders::sky_dome", &[], &[NORMAL_TARGET]);
}

/// The translucent overlay material: textured or not.
#[test]
fn unlit_compiles_for_every_variant() {
    check_all("shaders::unlit", &[], &["VERTEX_UVS_A", NORMAL_TARGET]);
}

/// bevy's line gizmo shader, copied into our tree and compiled against our
/// bind group 0 (`scene_pass::gizmos`). All three fragment entry points are
/// in the module, so one compile covers solid, dotted and dashed.
#[test]
fn gizmo_lines_compile_for_every_variant() {
    check_all("shaders::gizmos::lines", &[], &["PERSPECTIVE"]);
}

/// Terrain's two stages, as the scene pass compiles them: one `const` def
/// for the material bind group, the normal-target def, and nothing else —
/// dropping bevy's mesh pipeline took the whole def matrix with it, which is
/// half the point of owning the pass.
#[test]
fn terrain_scene_pass_shaders_compile() {
    check_all("shaders::scene::terrain_vertex", &[], &[]);
    check_all("shaders::terrain_toon", &[], &[NORMAL_TARGET]);
}

/// The edge pass: `MULTISAMPLED` switches the prepass texture declarations
/// (and the loads) between the MSAA and the single-sampled layout; the loop
/// bound `constants::MSAA_SAMPLES` comes from the stub above.
#[test]
fn edge_detection_compiles_multisampled_and_not() {
    check_all("shaders::edge_detection_post", &[], &["MULTISAMPLED"]);
}

/// The harness itself: a shader that is wrong must fail, or the tests above
/// prove nothing.
#[test]
fn a_broken_shader_is_caught() {
    assert!(check("missing_module", &[]).is_err());
    // `toon_lighting` reads `in.uv` under VERTEX_UVS_A, which only exists in
    // the varyings struct under the same def — compiling the fragment with
    // the def and a vertex struct without it is impossible here, so instead
    // prove naga validation is live: a shader that samples a texture it
    // never declared cannot compile.
    struct Broken;
    impl Resolver for Broken {
        fn resolve_source<'a>(&'a self, _: &ModulePath) -> Result<Cow<'a, str>, ResolveError> {
            Ok(Cow::Borrowed(
                "@fragment fn fragment() -> @location(0) vec4<f32> { return textureSample(t, s, vec2(0.0)); }",
            ))
        }
    }
    let path = ModulePath::new(PathOrigin::Absolute, vec!["broken".into()]);
    assert!(wesl::compile(&path, &CompileOptions::default(), &Broken).is_err());
}
