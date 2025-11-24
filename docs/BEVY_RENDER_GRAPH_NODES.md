# Bevy Render Graph Node Reference

This document provides a comprehensive reference for all built-in nodes in Bevy's render graph. This complements [RENDER_GRAPH.md](RENDER_GRAPH.md), which explains how to extend the render graph for your own needs.

## Table of Contents

- [Overview](#overview)
- [Render Graph Structure](#render-graph-structure)
- [Core3d Sub-Graph](#core3d-sub-graph)
  - [Node Execution Order](#core3d-execution-order)
  - [Prepass Nodes](#prepass-nodes)
  - [Main Pass Nodes](#main-pass-nodes)
  - [Post-Processing Nodes](#post-processing-nodes)
- [Core2d Sub-Graph](#core2d-sub-graph)
  - [Node Execution Order](#core2d-execution-order)
  - [2D Rendering Nodes](#2d-rendering-nodes)
- [Understanding Node Types](#understanding-node-types)
- [Integration Points](#integration-points)
- [References](#references)

---

## Overview

Bevy's render graph is organized into **sub-graphs**, each responsible for a specific rendering pipeline. The two primary sub-graphs are:

- **`Core3d`**: 3D rendering pipeline (meshes, lighting, shadows, advanced post-processing)
- **`Core2d`**: 2D rendering pipeline (sprites, UI elements, simpler post-processing)

Each sub-graph contains a series of **nodes** that execute sequentially to produce the final rendered image. Understanding these nodes and their execution order is crucial for:

- Knowing where to insert custom post-processing effects
- Understanding the rendering pipeline's performance characteristics
- Debugging rendering issues
- Optimizing your game's graphics

---

## Render Graph Structure

### Sub-Graph Architecture

```
RenderGraph (Root)
│
├─ Core3d (3D Rendering)
│  ├─ Prepass nodes (depth, normals, motion vectors)
│  ├─ Main pass nodes (opaque, transmissive, transparent)
│  └─ Post-processing nodes (bloom, TAA, tonemapping, etc.)
│
├─ Core2d (2D Rendering)
│  ├─ Main pass nodes (opaque, transparent)
│  └─ Post-processing nodes (bloom, tonemapping, upscaling)
│
└─ [Other sub-graphs for UI, etc.]
```

### Node Types

Nodes are defined as `enum` variants representing labels in the render graph:

```rust
// From bevy_core_pipeline/src/core_3d/mod.rs
#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub enum Node3d {
    MsaaWriteback,
    EarlyPrepass,
    EarlyDownsampleDepth,
    LatePrepass,
    // ... etc
}
```

---

## Core3d Sub-Graph

The **Core3d** sub-graph handles all 3D rendering for cameras with the `Camera3d` component.

### Core3d Execution Order

The complete execution order of nodes in Core3d (as configured in `Core3dPlugin`):

```
EarlyPrepass
  ↓
EarlyDeferredPrepass
  ↓
LatePrepass
  ↓
LateDeferredPrepass
  ↓
CopyDeferredLightingId
  ↓
EndPrepasses (marker)
  ↓
StartMainPass (marker)
  ↓
MainOpaquePass
  ↓
MainTransmissivePass
  ↓
MainTransparentPass
  ↓
EndMainPass (marker)
  ↓
StartMainPassPostProcessing (marker)
  ↓
MotionBlur (optional)
  ↓
Taa (optional)
  ↓
Bloom (optional)
  ↓
AutoExposure (optional)
  ↓
DepthOfField (optional)
  ↓
PostProcessing (reserved)
  ↓
Tonemapping
  ↓
Fxaa (optional)
  ↓
Smaa (optional)
  ↓
Upscaling
  ↓
ContrastAdaptiveSharpening (optional)
  ↓
EndMainPassPostProcessing (marker)
```

**Note**: Some nodes are only added to the graph when their corresponding plugins are enabled (marked as "optional"). Others are marker nodes (`EmptyNode`) for organizational purposes.

---

### Prepass Nodes

Prepasses generate auxiliary textures (depth, normals, motion vectors) used by later rendering stages and effects like TAA, SSAO, and motion blur.

#### `EarlyPrepass`
**Type**: `ViewNodeRunner<EarlyPrepassNode>`
**Source**: `bevy_core_pipeline/src/prepass/`
**Purpose**: Early depth prepass for opaque geometry

**What it does**:
- Renders opaque meshes to generate depth buffer
- Optionally generates normal vectors texture
- Optionally generates motion vectors texture
- Runs **before** main lighting pass to enable early-Z optimization

**Enabled by**: Adding `DepthPrepass`, `NormalPrepass`, or `MotionVectorPrepass` components to camera

**Outputs**:
- Depth texture (`ViewPrepassTextures::depth`)
- Normal texture (`ViewPrepassTextures::normal`) if enabled
- Motion vectors texture (`ViewPrepassTextures::motion_vectors`) if enabled

**Common use cases**:
- Screen-space ambient occlusion (SSAO)
- Temporal anti-aliasing (TAA)
- Motion blur
- Depth-based edge detection
- Early-Z culling optimization

---

#### `EarlyDeferredPrepass`
**Type**: `ViewNodeRunner<EarlyDeferredGBufferPrepassNode>`
**Source**: `bevy_core_pipeline/src/deferred/`
**Purpose**: Early deferred rendering G-buffer generation

**What it does**:
- Renders opaque meshes to generate G-buffer textures
- Stores material properties (albedo, roughness, metallic, etc.)
- Enables deferred rendering path for complex lighting

**Enabled by**: Adding `DeferredPrepass` component to camera

**Outputs**:
- Deferred G-buffer texture (`ViewPrepassTextures::deferred`)
- Deferred lighting pass ID texture

**Trade-offs**:
- ✅ Allows many lights with minimal performance cost
- ✅ Better performance for scenes with complex lighting
- ❌ Incompatible with MSAA (forces `Msaa::Off`)
- ❌ Higher memory bandwidth usage

---

#### `EarlyDownsampleDepth`
**Type**: Not explicitly added in base config (plugin-specific)
**Purpose**: Downsamples depth texture for optimization

**What it does**:
- Creates lower-resolution versions of depth buffer
- Used for hierarchical Z-buffer (Hi-Z) optimizations

---

#### `LatePrepass`
**Type**: `ViewNodeRunner<LatePrepassNode>`
**Source**: `bevy_core_pipeline/src/prepass/`
**Purpose**: Late depth prepass for alpha-masked geometry

**What it does**:
- Renders alpha-masked meshes (materials with alpha cutoff)
- Updates depth, normal, and motion vector textures
- Runs after early prepass to handle geometry with transparency testing

**Why separate from early prepass**:
- Alpha-masked geometry requires fragment shader execution (expensive)
- Separating allows opaque geometry to benefit from early-Z optimization first

---

#### `LateDeferredPrepass`
**Type**: `ViewNodeRunner<LateDeferredGBufferPrepassNode>`
**Purpose**: Deferred rendering for alpha-masked geometry

**What it does**:
- Extends deferred G-buffer with alpha-masked meshes
- Similar to `LatePrepass` but for deferred rendering path

---

#### `CopyDeferredLightingId`
**Type**: `ViewNodeRunner<CopyDeferredLightingIdNode>`
**Purpose**: Copies deferred lighting identifiers

**What it does**:
- Manages lighting pass IDs for deferred rendering
- Ensures correct lighting is applied to deferred geometry

---

#### `EndPrepasses`
**Type**: `EmptyNode` (marker)
**Purpose**: Marks the end of all prepass operations

**Why it exists**:
- Provides a clear insertion point for custom nodes that need to run after prepasses
- Organizational clarity in the render graph

---

### Main Pass Nodes

The main rendering passes produce the actual visible geometry and lighting.

#### `StartMainPass`
**Type**: `EmptyNode` (marker)
**Purpose**: Marks the beginning of main rendering passes

**Why it exists**:
- Clear insertion point for custom rendering before main geometry
- Organizational boundary in the render graph

---

#### `MainOpaquePass`
**Type**: `ViewNodeRunner<MainOpaquePass3dNode>`
**Source**: `bevy_core_pipeline/src/core_3d/main_opaque_pass_3d_node.rs`
**Purpose**: Renders opaque 3D geometry with lighting

**What it does**:
- Renders all opaque meshes (front-to-back for efficiency)
- Applies PBR lighting (directional, point, spot lights)
- Applies shadows (cascaded shadow maps for directional lights)
- Writes to main color and depth targets
- Supports material extensions (like our toon shader!)

**Render phases**:
- `Opaque3d` (binned phase, sorted front-to-back)
- `AlphaMask3d` (binned phase with alpha testing)

**Performance notes**:
- Front-to-back sorting maximizes early-Z rejection
- GPU batching enabled for meshes with same materials
- Supports GPU instancing for repeated meshes

---

#### `MainTransmissivePass`
**Type**: `ViewNodeRunner<MainTransmissivePass3dNode>`
**Source**: `bevy_core_pipeline/src/core_3d/main_transmissive_pass_3d_node.rs`
**Purpose**: Renders meshes with screen-space transmission (glass, water)

**What it does**:
- Renders meshes with `TransmittedShadowReceiver` component
- Implements screen-space refraction effects
- Copies background before rendering for refraction sampling
- Renders in multiple steps for quality (`screen_space_specular_transmission_steps`)

**Render phases**:
- `Transmissive3d` (sorted back-to-front)

**Enabled by**: Meshes with transmission materials

**Visual effects**:
- Realistic glass refraction
- Water surface distortion
- Transparent volumetric materials

---

#### `MainTransparentPass`
**Type**: `ViewNodeRunner<MainTransparentPass3dNode>`
**Source**: `bevy_core_pipeline/src/core_3d/main_transparent_pass_3d_node.rs`
**Purpose**: Renders transparent 3D geometry with alpha blending

**What it does**:
- Renders all transparent meshes (back-to-front for correct blending)
- Applies alpha blending to combine with opaque background
- Reads depth buffer but does not write to it (usually)
- Applies lighting based on material settings

**Render phases**:
- `Transparent3d` (sorted back-to-front by distance)

**Common uses**:
- Glass windows (simple transparency)
- Particles and effects
- UI elements in 3D space
- Transparent overlays

**Limitations**:
- Order-dependent transparency (sorting artifacts possible)
- Cannot write depth (causes issues with TAA and motion blur)
- No self-sorting within same mesh

---

#### `EndMainPass`
**Type**: `EmptyNode` (marker)
**Purpose**: Marks the end of main geometry rendering

---

#### `Wireframe`
**Type**: Not in default execution order (conditionally added)
**Purpose**: Renders wireframe overlay

**What it does**:
- Renders mesh edges as lines
- Used for debugging and wireframe visualization
- Added by `WireframePlugin` when enabled

---

### Post-Processing Nodes

Post-processing nodes operate on the rendered image, applying effects and corrections.

#### `StartMainPassPostProcessing`
**Type**: `EmptyNode` (marker)
**Purpose**: Marks the beginning of post-processing

**Critical insertion point**: This is where you typically insert custom post-processing effects (like our edge detection!)

**Example** (from our implementation):
```rust
.add_render_graph_edges(
    Core3d,
    (
        Node3d::Tonemapping,
        EdgeDetectionLabel,  // Our custom node
        Node3d::EndMainPassPostProcessing,
    ),
)
```

---

#### `LateDownsampleDepth`
**Type**: Not in base execution (conditionally added)
**Purpose**: Downsamples depth buffer after main passes

**What it does**:
- Creates mipmap chain of depth buffer
- Used for depth-aware effects at multiple scales

---

#### `MotionBlur`
**Type**: `ViewNodeRunner<MotionBlurNode>` (optional)
**Source**: `bevy_post_process/src/motion_blur/`
**Plugin**: `MotionBlurPlugin`
**Purpose**: Per-pixel motion blur effect

**What it does**:
- Uses motion vectors to blur moving objects
- Samples along motion vector path
- Simulates camera/film exposure blur

**Enabled by**: Adding `MotionBlur` component to camera

**Settings**:
```rust
MotionBlur {
    shutter_angle: 0.5,  // 0.0-1.0, strength of blur
    samples: 1,          // Quality (1*2+1 = 3 samples)
}
```

**Requirements**:
- `MotionVectorPrepass` must be enabled
- `DepthPrepass` must be enabled

**Trade-offs**:
- ✅ Realistic motion blur for moving objects/camera
- ✅ Relatively inexpensive
- ❌ Doesn't blur edges of fast-moving objects
- ❌ Transparent objects cannot be blurred

**Placement**: Runs **before** TAA to reduce TAA artifacts

---

#### `Taa` (Temporal Anti-Aliasing)
**Type**: `ViewNodeRunner<TemporalAntiAliasNode>` (optional)
**Source**: `bevy_anti_alias/src/taa/`
**Plugin**: `TemporalAntiAliasPlugin`
**Purpose**: Temporal anti-aliasing by blending frames

**What it does**:
- Blends current frame with previous frames
- Uses motion vectors to track pixels between frames
- Applies sub-pixel jittering to camera
- Reduces aliasing on edges, textures, and specular highlights

**Enabled by**: Adding `TemporalAntiAliasing` component to camera

**Settings**:
```rust
TemporalAntiAliasing {
    reset: false,  // Set true to clear history (e.g., on camera cuts)
}
```

**Requirements**:
- `Msaa::Off` (incompatible with MSAA)
- `MotionVectorPrepass` must be enabled
- `DepthPrepass` must be enabled

**Trade-offs**:
- ✅ Excellent anti-aliasing quality
- ✅ Smooths out stochastic effects (SSAO noise, etc.)
- ✅ Cost scales with resolution, not geometry
- ❌ Ghosting artifacts on fast motion
- ❌ Thin geometry may flicker
- ❌ Requires depth-writing (issues with transparents)

**Placement**: Runs **after** motion blur, **before** bloom and tonemapping

---

#### `DlssSuperResolution`
**Type**: Not in open-source Bevy (NVIDIA proprietary)
**Purpose**: NVIDIA DLSS super-resolution upscaling

**What it does** (when available):
- AI-powered upscaling from lower resolution
- Renders at lower resolution, upscales to target
- Massive performance improvement on NVIDIA RTX GPUs

---

#### `DlssRayReconstruction`
**Type**: Not in open-source Bevy (NVIDIA proprietary)
**Purpose**: NVIDIA DLSS ray reconstruction for ray-traced rendering

---

#### `Bloom`
**Type**: `ViewNodeRunner<BloomNode>` (optional)
**Source**: `bevy_post_process/src/bloom/`
**Plugin**: `BloomPlugin`
**Purpose**: HDR bloom (glow) effect

**What it does**:
- Extracts bright areas from HDR image
- Applies Gaussian blur via downsampling/upsampling passes
- Composites blurred bright areas back onto image
- Creates realistic glow around bright objects

**Enabled by**: Adding `Bloom` component to camera

**Settings**:
```rust
Bloom {
    intensity: 0.3,              // Strength of bloom
    low_frequency_boost: 0.0,    // Boost large-area blooms
    low_frequency_boost_curvature: 0.0,
    high_pass_frequency: 1.0,    // Controls bloom threshold
    prefilter: BloomPrefilter {  // Pre-filter settings
        threshold: 0.0,
        threshold_softness: 0.0,
    },
    composite_mode: BloomCompositeMode::Additive,
}
```

**Implementation details**:
- Uses custom textures (not ViewTarget)
- Multiple downsampling passes (mipmap chain)
- Multiple upsampling passes (tent filter)
- Format: `Rg11b10Ufloat` (compact HDR format)

**Performance**:
- Cost increases with resolution and intensity
- ~6-8 render passes total

**Placement**: After TAA, before tonemapping (operates in HDR space)

---

#### `AutoExposure`
**Type**: Not in base Bevy 0.17 (may be added in future versions)
**Purpose**: Automatic exposure adjustment

**What it does** (when implemented):
- Analyzes scene brightness
- Adjusts exposure to maintain good visibility
- Simulates eye/camera adaptation to light levels

---

#### `DepthOfField`
**Type**: Not in base Bevy 0.17 (may be added in future versions)
**Purpose**: Depth-of-field blur effect

**What it does** (when implemented):
- Blurs objects outside focal plane
- Simulates camera lens focus
- Uses depth buffer to determine blur amount

---

#### `PostProcessing`
**Type**: Reserved node (not currently used)
**Purpose**: Placeholder for future built-in post-processing

**When to use**:
- This slot is reserved by Bevy for future effects
- Insert custom effects **before** or **after** this node
- Recommended: Use `StartMainPassPostProcessing` to `Tonemapping` range

---

#### `Tonemapping`
**Type**: `ViewNodeRunner<TonemappingNode>`
**Source**: `bevy_core_pipeline/src/tonemapping/`
**Purpose**: HDR to LDR conversion and color grading

**What it does**:
- Converts HDR (high dynamic range) to LDR (displayable range)
- Applies tonemapping curve (Reinhard, ACES, etc.)
- Applies color grading/correction
- Applies dithering to reduce banding

**Always enabled**: Yes (but can be set to `Tonemapping::None`)

**Tonemapping modes**:
```rust
pub enum Tonemapping {
    None,                    // Pass-through (for LDR)
    Reinhard,               // Simple, preserves hue
    ReinhardLuminance,      // Variant on Reinhard
    AcesFitted,             // Film-like (ACES approximation)
    AgX,                    // Neutral, good contrast
    SomewhatBoringDisplayTransform, // Minimal color shift
    TonyMcMapface,          // Aggressively neutral
    BlenderFilmic,          // Blender's default
}
```

**Why it's critical**:
- HDR values (0-∞) cannot be displayed on screen (0-1)
- Different curves produce different "looks"
- Our toon shader does its own tonemapping in the fragment shader!

**Dithering**:
```rust
pub enum DebandDither {
    Enabled,   // Default, reduces banding
    Disabled,  // Slight performance gain
}
```

**Placement**: After bloom, before anti-aliasing (FXAA/SMAA)

---

#### `Fxaa` (Fast Approximate Anti-Aliasing)
**Type**: `ViewNodeRunner<FxaaNode>` (optional)
**Source**: `bevy_anti_alias/src/fxaa/`
**Plugin**: `FxaaPlugin`
**Purpose**: Fast post-process anti-aliasing

**What it does**:
- Detects edges by analyzing luminance
- Applies directional blur along edges
- Smooths jagged edges in final image

**Enabled by**: Adding `Fxaa` component to camera

**Settings**:
```rust
Fxaa {
    enabled: true,
    edge_threshold: Sensitivity::High,    // Edge detection sensitivity
    edge_threshold_min: Sensitivity::High, // Minimum threshold
}
```

**Trade-offs**:
- ✅ Very fast (single post-process pass)
- ✅ Works with any geometry
- ✅ No special requirements
- ❌ Can blur textures and text
- ❌ Less effective than TAA for thin geometry

**Placement**: After tonemapping (works in LDR space)

---

#### `Smaa` (Subpixel Morphological Anti-Aliasing)
**Type**: `ViewNodeRunner<SmaaNode>` (optional)
**Source**: `bevy_anti_alias/src/smaa/`
**Plugin**: `SmaaPlugin`
**Purpose**: High-quality post-process anti-aliasing

**What it does**:
- Multi-pass edge detection and blending
- Morphological pattern detection
- Better quality than FXAA, similar cost

**Enabled by**: Adding `Smaa` component to camera

**Settings**:
```rust
pub enum Smaa {
    Low,
    Medium,
    High,
    Ultra,
}
```

**Trade-offs**:
- ✅ Better quality than FXAA
- ✅ Less texture blurring
- ✅ Works with any geometry
- ❌ Slightly slower than FXAA
- ❌ Still not as good as TAA for temporal coherence

**Note**: Mutually exclusive with FXAA (use one or the other)

**Placement**: After tonemapping, alternative to FXAA

---

#### `Upscaling`
**Type**: `ViewNodeRunner<UpscalingNode>`
**Source**: `bevy_core_pipeline/src/upscaling/`
**Purpose**: Upscales render to window resolution

**What it does**:
- Upscales from render resolution to window resolution
- Uses `Camera::render_scale` setting
- Applies bilinear filtering for smooth upscale

**Always enabled**: Yes (no-op if `render_scale = 1.0`)

**Use cases**:
- Performance: Render at 0.5× resolution, upscale to full
- Super-sampling: Render at 2.0× resolution, downscale (expensive!)

**Example**:
```rust
Camera3d {
    render_scale: 0.75,  // Render at 75% resolution, upscale
    ..default()
}
```

**Performance impact**:
- Lower render_scale = better FPS but softer image
- Values like 0.75 often provide good quality/performance balance

---

#### `ContrastAdaptiveSharpening`
**Type**: `ViewNodeRunner<CasNode>` (optional)
**Purpose**: AMD FidelityFX CAS sharpening

**What it does**:
- Applies adaptive sharpening filter
- Restores detail after upscaling
- Particularly useful with `render_scale < 1.0`

**Enabled by**: Adding `ContrastAdaptiveSharpening` component to camera

---

#### `EndMainPassPostProcessing`
**Type**: `EmptyNode` (marker)
**Purpose**: Marks the end of post-processing

**Typical insertion point**: Custom effects that should run **after** tonemapping but **before** final output

---

#### `MsaaWriteback`
**Type**: Not in default execution order (conditionally added)
**Purpose**: Resolves MSAA textures to single-sampled textures

**What it does**:
- Resolves multi-sample anti-aliasing buffers
- Converts MSAA render target to standard texture
- Only active when `Msaa` is enabled (not `Msaa::Off`)

---

## Core2d Sub-Graph

The **Core2d** sub-graph handles all 2D rendering for cameras with the `Camera2d` component.

### Core2d Execution Order

The complete execution order of nodes in Core2d:

```
StartMainPass (marker)
  ↓
MainOpaquePass
  ↓
MainTransparentPass
  ↓
EndMainPass (marker)
  ↓
StartMainPassPostProcessing (marker)
  ↓
Bloom (optional)
  ↓
PostProcessing (reserved)
  ↓
Tonemapping
  ↓
Fxaa (optional)
  ↓
Smaa (optional)
  ↓
Upscaling
  ↓
ContrastAdaptiveSharpening (optional)
  ↓
EndMainPassPostProcessing (marker)
```

**Differences from Core3d**:
- No prepasses (no depth/normal/motion vectors by default)
- No deferred rendering
- No transmissive pass
- No TAA or motion blur (require motion vectors)
- Simpler, faster pipeline

---

### 2D Rendering Nodes

#### `MainOpaquePass` (2D)
**Type**: `ViewNodeRunner<MainOpaquePass2dNode>`
**Source**: `bevy_core_pipeline/src/core_2d/main_opaque_pass_2d_node.rs`
**Purpose**: Renders opaque 2D elements

**What it does**:
- Renders sprites, 2D meshes with opaque materials
- No depth testing by default (painter's algorithm)
- Uses z-ordering for layering

**Render phases**:
- `Opaque2d` (binned phase)
- `AlphaMask2d` (binned phase with alpha testing)

---

#### `MainTransparentPass` (2D)
**Type**: `ViewNodeRunner<MainTransparentPass2dNode>`
**Source**: `bevy_core_pipeline/src/core_2d/main_transparent_pass_2d_node.rs`
**Purpose**: Renders transparent 2D elements

**What it does**:
- Renders sprites with alpha blending
- Sorted by z-order
- Back-to-front rendering

**Render phases**:
- `Transparent2d` (sorted phase)

---

#### Post-Processing Nodes (2D)

The same post-processing nodes as Core3d, but:
- **Bloom**: Available (simpler version)
- **Tonemapping**: Available (default is `Tonemapping::None` for 2D)
- **Fxaa/Smaa**: Available
- **Upscaling**: Available
- **TAA/Motion Blur**: Not available (no motion vectors in 2D)

---

## Understanding Node Types

### EmptyNode (Marker Nodes)

Marker nodes provide organizational structure without doing any rendering:

```rust
pub struct EmptyNode;

impl Node for EmptyNode {
    fn run(&self, ...) -> Result<(), NodeRunError> {
        Ok(())  // Does nothing
    }
}
```

**Examples**: `StartMainPass`, `EndMainPass`, `StartMainPassPostProcessing`, `EndMainPassPostProcessing`

**Why they exist**:
- Clear insertion points for custom nodes
- Logical grouping of rendering stages
- Documentation of render pipeline structure
- Future extensibility

---

### ViewNode vs Node

**`ViewNode`** (Recommended for most use cases):
```rust
pub trait ViewNode {
    type ViewQuery: ReadOnlyQueryData;

    fn run(
        &self,
        graph: &mut RenderGraphContext,
        render_context: &mut RenderContext,
        view_query: QueryItem<Self::ViewQuery>,  // Per-camera data
        world: &World,
    ) -> Result<(), NodeRunError>;
}
```

- Runs once **per camera/view**
- Automatic ECS query for view-specific components
- Type-safe access to `ViewTarget`, camera settings, etc.
- **Use for**: Post-processing, per-camera effects

**`Node`** (Lower-level):
```rust
pub trait Node {
    fn run(
        &self,
        graph: &mut RenderGraphContext,
        render_context: &mut RenderContext,
        world: &World,
    ) -> Result<(), NodeRunError>;
}
```

- Runs once per frame (not per view)
- Manual iteration over views if needed
- More control, more complexity
- **Use for**: Global effects, multi-view operations

---

### ViewNodeRunner

Wraps a `ViewNode` to make it compatible with the render graph:

```rust
// In plugin code:
.add_render_graph_node::<ViewNodeRunner<MyNode>>(
    Core3d,
    MyNodeLabel,
)
```

This automatically:
- Queries for active cameras
- Runs the node once per view
- Handles the ECS query specified in `ViewNode::ViewQuery`

---

## Integration Points

### Where to Insert Custom Nodes

#### For Post-Processing Effects (Recommended)

**After main rendering, before tonemapping** (HDR space):
```rust
.add_render_graph_edges(
    Core3d,
    (
        Node3d::StartMainPassPostProcessing,
        MyCustomEffectLabel,  // Your node here
        Node3d::Tonemapping,
    ),
)
```

**Use cases**: Bloom-like effects, HDR color grading, custom blur

---

**After tonemapping, before end** (LDR space):
```rust
.add_render_graph_edges(
    Core3d,
    (
        Node3d::Tonemapping,
        MyCustomEffectLabel,  // Your node here
        Node3d::EndMainPassPostProcessing,
    ),
)
```

**Use cases**: Edge detection (our implementation!), vignette, color filters, retro effects

---

#### For Custom Geometry Rendering

**Before main passes**:
```rust
.add_render_graph_edges(
    Core3d,
    (
        Node3d::EndPrepasses,
        MyCustomGeometryLabel,
        Node3d::StartMainPass,
    ),
)
```

**Use cases**: Custom prepass effects, auxiliary rendering

---

**After transparent pass**:
```rust
.add_render_graph_edges(
    Core3d,
    (
        Node3d::MainTransparentPass,
        MyCustomOverlayLabel,
        Node3d::EndMainPass,
    ),
)
```

**Use cases**: Debug overlays, UI in 3D space, custom blending

---

### Common Patterns

#### Multi-Pass Effect

```rust
.add_render_graph_node::<ViewNodeRunner<Pass1Node>>(Core3d, Pass1Label)
.add_render_graph_node::<ViewNodeRunner<Pass2Node>>(Core3d, Pass2Label)
.add_render_graph_edges(
    Core3d,
    (
        Node3d::Tonemapping,
        Pass1Label,
        Pass2Label,
        Node3d::EndMainPassPostProcessing,
    ),
)
```

**ViewTarget** automatically handles texture swapping between passes!

---

#### Conditional Node (Quality Settings)

```rust
impl ViewNode for MyEffectNode {
    fn run(...) -> Result<(), NodeRunError> {
        if settings.quality == Quality::Off {
            return Ok(());  // Early exit, no rendering
        }

        // ... normal rendering
    }
}
```

---

#### Reading Prepass Textures

```rust
impl ViewNode for MyEffectNode {
    type ViewQuery = (
        &'static ViewTarget,
        &'static ViewPrepassTextures,  // Depth, normals, motion vectors
    );

    fn run(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext,
        (view_target, prepass_textures): QueryItem<Self::ViewQuery>,
        world: &World,
    ) -> Result<(), NodeRunError> {
        let Some(depth_texture) = &prepass_textures.depth else {
            return Ok(());  // Depth prepass not enabled
        };

        // Use depth_texture.default_view in bind group
        // ...
    }
}
```

**Requirements**: Camera must have `DepthPrepass`, `NormalPrepass`, or `MotionVectorPrepass` components

---

## References

### Bevy Source Files

**Core Render Graph**:
- [bevy_render/src/render_graph/](https://github.com/bevyengine/bevy/tree/main/crates/bevy_render/src/render_graph) - Core graph implementation

**Core 3D Pipeline**:
- [bevy_core_pipeline/src/core_3d/mod.rs](https://github.com/bevyengine/bevy/blob/main/crates/bevy_core_pipeline/src/core_3d/mod.rs) - Node3d definitions and setup
- [bevy_core_pipeline/src/core_3d/main_opaque_pass_3d_node.rs](https://github.com/bevyengine/bevy/blob/main/crates/bevy_core_pipeline/src/core_3d/main_opaque_pass_3d_node.rs)
- [bevy_core_pipeline/src/core_3d/main_transparent_pass_3d_node.rs](https://github.com/bevyengine/bevy/blob/main/crates/bevy_core_pipeline/src/core_3d/main_transparent_pass_3d_node.rs)

**Core 2D Pipeline**:
- [bevy_core_pipeline/src/core_2d/mod.rs](https://github.com/bevyengine/bevy/blob/main/crates/bevy_core_pipeline/src/core_2d/mod.rs) - Node2d definitions and setup

**Post-Processing**:
- [bevy_post_process/src/bloom/](https://github.com/bevyengine/bevy/tree/main/crates/bevy_post_process/src/bloom)
- [bevy_post_process/src/motion_blur/](https://github.com/bevyengine/bevy/tree/main/crates/bevy_post_process/src/motion_blur)
- [bevy_anti_alias/src/taa/](https://github.com/bevyengine/bevy/tree/main/crates/bevy_anti_alias/src/taa)
- [bevy_anti_alias/src/fxaa/](https://github.com/bevyengine/bevy/tree/main/crates/bevy_anti_alias/src/fxaa)
- [bevy_core_pipeline/src/tonemapping/](https://github.com/bevyengine/bevy/tree/main/crates/bevy_core_pipeline/src/tonemapping)

**Prepasses**:
- [bevy_core_pipeline/src/prepass/](https://github.com/bevyengine/bevy/tree/main/crates/bevy_core_pipeline/src/prepass)
- [bevy_core_pipeline/src/deferred/](https://github.com/bevyengine/bevy/tree/main/crates/bevy_core_pipeline/src/deferred)

### Our Implementation

- [docs/RENDER_GRAPH.md](RENDER_GRAPH.md) - General render graph guide and extension patterns
- [src/game/post_process.rs](../src/game/post_process.rs) - Edge detection implementation
- [assets/shaders/edge_detection_post.wgsl](../assets/shaders/edge_detection_post.wgsl)

---

## Summary

### Quick Reference Table

| Node | Type | Purpose | Requires | Optional |
|------|------|---------|----------|----------|
| **Core3d Prepasses** |
| `EarlyPrepass` | ViewNode | Depth/normals/motion prepass | - | Component |
| `LatePrepass` | ViewNode | Alpha-mask prepass | - | Component |
| `EarlyDeferredPrepass` | ViewNode | Deferred G-buffer (opaque) | - | Component |
| `LateDeferredPrepass` | ViewNode | Deferred G-buffer (alpha-mask) | - | Component |
| **Core3d Main Passes** |
| `MainOpaquePass` | ViewNode | Opaque geometry + lighting | - | Always |
| `MainTransmissivePass` | ViewNode | Transmissive materials | - | Auto |
| `MainTransparentPass` | ViewNode | Transparent geometry | - | Always |
| **Core3d Post-Processing** |
| `MotionBlur` | ViewNode | Motion blur effect | Prepass | Component |
| `Taa` | ViewNode | Temporal anti-aliasing | Prepass, Msaa::Off | Component |
| `Bloom` | ViewNode | HDR bloom glow | - | Component |
| `Tonemapping` | ViewNode | HDR → LDR conversion | - | Always |
| `Fxaa` | ViewNode | Fast anti-aliasing | - | Component |
| `Smaa` | ViewNode | Quality anti-aliasing | - | Component |
| `Upscaling` | ViewNode | Resolution upscaling | - | Always |
| **Core2d** |
| `MainOpaquePass` | ViewNode | Opaque 2D elements | - | Always |
| `MainTransparentPass` | ViewNode | Transparent 2D elements | - | Always |
| (Same post-processing as Core3d, minus TAA/MotionBlur) |

---

### Key Takeaways

1. **Render Graph is Organized**: Prepasses → Main Passes → Post-Processing
2. **Many Nodes are Optional**: Only included when corresponding components/plugins are active
3. **Marker Nodes Provide Structure**: Use `StartMainPassPostProcessing`/`EndMainPassPostProcessing` as insertion points
4. **ViewNode for Post-Processing**: Almost all post-processing uses `ViewNode` pattern
5. **ViewTarget is Key**: Automatic double-buffering for multi-pass effects
6. **Order Matters**: HDR effects before tonemapping, LDR effects after
7. **Prepasses Enable Advanced Effects**: TAA, motion blur, depth-based effects need prepasses

---

*Last updated: 2025-11-22*
*Bevy Version: 0.17.2*
