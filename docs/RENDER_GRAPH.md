# Bevy Render Graph Guide

This document explains how the Bevy render graph works, how to extend it, and optimization strategies for our toon shader and edge detection implementation.

## Table of Contents

- [Bevy Render Graph Architecture](#bevy-render-graph-architecture)
- [Extension Patterns & Best Practices](#extension-patterns--best-practices)
- [Current Implementation Analysis](#current-implementation-analysis)
- [Optimization Recommendations](#optimization-recommendations)
- [Code Examples](#code-examples)
- [References](#references)

---

## Bevy Render Graph Architecture

### Core Concepts

The Bevy render graph is a **Directed Acyclic Graph (DAG)** that defines the execution order of rendering operations. It's the backbone of Bevy's rendering pipeline.

**Key Properties:**
- Stored as a resource in the `RenderApp` (separate from main `App`)
- Nodes represent rendering operations (e.g., shadow pass, main pass, post-processing)
- Edges define execution order and data dependencies
- Supports sub-graphs for organizing complex pipelines (e.g., `Core2d`, `Core3d`)

### Core Types

#### RenderGraph

```rust
bevy_render::render_graph::RenderGraph
```

The main graph structure containing all nodes and edges. Accessed as a resource in the `RenderApp`:

```rust
let render_graph = world.resource_mut::<RenderGraph>();
let sub_graph = render_graph.get_sub_graph_mut(Core3d).unwrap();
```

#### Node Trait

```rust
pub trait Node: Downcast + Send + Sync + 'static {
    fn input(&self) -> Vec<SlotInfo> { Vec::new() }
    fn output(&self) -> Vec<SlotInfo> { Vec::new() }
    fn update(&mut self, world: &mut World) {}
    fn run(
        &self,
        graph: &mut RenderGraphContext,
        render_context: &mut RenderContext,
        world: &World,
    ) -> Result<(), NodeRunError>;
}
```

- **`input()`** / **`output()`**: Define slots for receiving/passing data (textures, buffers, etc.)
- **`update()`**: Called before `run()` to update internal state from ECS world
- **`run()`**: Main execution method where GPU commands are issued

#### ViewNode Trait (Recommended for Camera-Specific Rendering)

```rust
pub trait ViewNode {
    type ViewQuery: ReadOnlyQueryData;

    fn run(
        &self,
        graph: &mut RenderGraphContext,
        render_context: &mut RenderContext,
        view_query: QueryItem<Self::ViewQuery>,
        world: &World,
    ) -> Result<(), NodeRunError>;
}
```

**Benefits:**
- Automatically runs per camera/view
- Type-safe ECS query for view-specific components
- Simpler than raw `Node` trait for common use cases
- Wrapped by `ViewNodeRunner<T>` which handles query management

**Best for**: Post-processing effects, view-specific rendering passes

### Edges & Data Flow

#### Edge Types

**1. Node Edges** (Ordering Only)
```rust
sub_graph.add_node_edge(NodeA, NodeB);
// NodeA runs before NodeB, no data transfer
```

**2. Slot Edges** (Ordering + Data Transfer)
```rust
sub_graph.add_slot_edge(
    NodeA, "output_texture",  // Output slot
    NodeB, "input_texture"    // Input slot
);
// NodeA runs before NodeB, texture passed via slots
```

#### Slot Types

```rust
pub enum SlotType {
    TextureView,  // GPU texture views
    Sampler,      // Texture samplers
    Buffer,       // GPU buffers
    Entity,       // ECS entities (indirect data access)
}
```

Slots enable type-safe data passing between nodes without manual synchronization.

### Execution Flow

1. **Topology Sort**: Graph nodes sorted to respect dependencies
2. **Per Node**:
   - Call `node.update(world)` to sync with ECS world
   - Call `node.run(...)` to execute GPU commands
   - Node reads input slots, performs rendering, writes output slots
3. **Sub-Graphs**: Executed recursively when queued
4. **View Nodes**: Run once per camera entity with matching components

### Sub-Graphs

Bevy organizes rendering into logical sub-graphs:

- **`Core2d`**: 2D rendering pipeline (sprites, UI)
- **`Core3d`**: 3D rendering pipeline (meshes, lighting, shadows)
- Custom sub-graphs can be created for specialized pipelines

**Our Implementation** uses the `Core3d` sub-graph for toon shading and edge detection.

---

## Extension Patterns & Best Practices

### Adding Custom Nodes

#### Method 1: RenderGraphExt Trait (Recommended)

```rust
use bevy::render::RenderApp;
use bevy::core_pipeline::core_3d::graph::{Core3d, Node3d};

impl Plugin for MyPostProcessPlugin {
    fn build(&self, app: &mut App) {
        let render_app = app.sub_app_mut(RenderApp);

        render_app
            // Add node to graph
            .add_render_graph_node::<ViewNodeRunner<MyPostProcessNode>>(
                Core3d,           // Sub-graph
                MyNodeLabel,      // Node identifier
            )
            // Define execution order
            .add_render_graph_edges(
                Core3d,
                (
                    Node3d::Tonemapping,      // Run after tonemapping
                    MyNodeLabel,              // Our node
                    Node3d::EndMainPassPostProcessing,  // Before post-processing end
                ),
            );
    }
}
```

**Benefits:**
- Concise syntax
- Automatic error handling
- Type-safe labels

#### Method 2: Direct Graph Access

```rust
let render_graph = world.resource_mut::<RenderGraph>();
let sub_graph = render_graph.get_sub_graph_mut(Core3d).unwrap();

sub_graph.add_node(MyLabel, MyNodeInstance);
sub_graph.add_node_edge(PreviousNode, MyLabel);
sub_graph.add_node_edge(MyLabel, NextNode);
```

Use this when you need more control or dynamic graph construction.

### ViewNode Implementation Pattern

**Recommended structure for post-processing effects:**

```rust
use bevy::render::render_graph::{ViewNode, NodeRunError};
use bevy::core_pipeline::core_3d::ViewTarget;

pub struct MyPostProcessNode;

impl ViewNode for MyPostProcessNode {
    type ViewQuery = (
        &'static ViewTarget,                          // Double-buffered render target
        &'static MySettings,                          // Per-camera settings
        &'static DynamicUniformIndex<MySettings>,    // GPU uniform buffer index
    );

    fn run(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext,
        (view_target, settings, uniform_index): QueryItem<Self::ViewQuery>,
        world: &World,
    ) -> Result<(), NodeRunError> {
        // Get cached pipeline
        let pipeline_cache = world.resource::<PipelineCache>();
        let pipeline = world.resource::<MyPostProcessPipeline>();
        let Some(pipeline_id) = pipeline_cache.get_render_pipeline(pipeline.id) else {
            return Ok(()); // Pipeline not ready yet
        };

        // Get ViewTarget textures for ping-pong rendering
        let post_process = view_target.post_process_write();
        // post_process.source = input texture (current frame)
        // post_process.destination = output texture (next frame)

        // Create bind group (must be done per-frame due to texture swapping)
        let bind_group = render_context.render_device().create_bind_group(
            "my_post_process_bind_group",
            &pipeline.layout,
            &BindGroupEntries::sequential((
                post_process.source,          // Input texture
                &pipeline.sampler,            // Texture sampler
                settings.binding(),           // Uniform buffer binding
            )),
        );

        // Render pass
        let mut render_pass = render_context.begin_tracked_render_pass(
            RenderPassDescriptor {
                label: Some("my_post_process_pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: post_process.destination,  // Output texture
                    resolve_target: None,
                    ops: Operations::default(),
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            },
        );

        render_pass.set_render_pipeline(pipeline_id);
        render_pass.set_bind_group(0, &bind_group, &[uniform_index.index()]);
        render_pass.draw(0..3, 0..1);  // Fullscreen triangle

        Ok(())
    }
}
```

### ViewTarget System (Critical for Post-Processing)

**ViewTarget** provides double-buffered textures for chaining multiple post-processing passes without extra allocations.

#### How It Works

```rust
pub struct ViewTarget {
    main_textures: MainTextures,
    // MainTextures contains two textures: .a and .b
}

pub struct PostProcessWrite<'a> {
    pub source: &'a TextureView,       // Current frame (read from)
    pub destination: &'a TextureView,  // Next frame (write to)
}
```

**Key Method**: `post_process_write()`
- Returns `PostProcessWrite` with source/destination textures
- **Automatically flips buffers** after the call
- Enables efficient multi-pass post-processing

#### Chaining Multiple Passes

```rust
// Pass 1: Reads A, writes B (now B is current)
let pp1 = view_target.post_process_write();
apply_effect_1(pp1.source, pp1.destination);

// Pass 2: Reads B, writes A (now A is current)
let pp2 = view_target.post_process_write();
apply_effect_2(pp2.source, pp2.destination);

// Pass 3: Reads A, writes B (now B is current)
let pp3 = view_target.post_process_write();
apply_effect_3(pp3.source, pp3.destination);
```

Each pass reads from the previous pass's output automatically!

### Component Extraction Pattern

To pass data from the main world to the render world:

```rust
use bevy::render::extract_component::ExtractComponent;

#[derive(Component, Clone, ShaderType)]
pub struct MySettings {
    pub intensity: f32,
    pub color: LinearRgba,
}

impl ExtractComponent for MySettings {
    type QueryData = &'static Self;
    type QueryFilter = With<Camera>;  // Only extract from cameras
    type Out = Self;

    fn extract_component(item: QueryItem<Self::QueryData>) -> Option<Self::Out> {
        Some(item.clone())
    }
}

// In plugin:
app.add_plugins(ExtractComponentPlugin::<MySettings>::default());
```

This automatically syncs components from main world entities to render world entities.

### Uniform Buffer Management

For GPU uniforms, use `UniformComponentPlugin`:

```rust
use bevy::render::render_resource::UniformBuffer;

app.add_plugins(UniformComponentPlugin::<MySettings>::default());
```

This automatically:
- Creates GPU uniform buffers
- Uploads data each frame
- Provides `DynamicUniformIndex` for bind group offsets
- Handles multi-camera scenarios

### Best Practices Checklist

- ✅ Use `ViewNode` for per-camera effects (simpler than raw `Node`)
- ✅ Cache `CachedRenderPipelineId` in a resource (don't recreate each frame)
- ✅ Create bind groups per-frame in `run()` (necessary for ViewTarget texture swapping)
- ✅ Use `ExtractComponentPlugin` for main→render world sync
- ✅ Use `UniformComponentPlugin` for automatic GPU buffer management
- ✅ Use `post_process_write()` for efficient multi-pass post-processing
- ✅ Position nodes correctly with `add_render_graph_edges()`
- ✅ Handle pipeline not ready (`get_render_pipeline()` returns `Option`)
- ✅ Use fullscreen triangle (0..3 vertices) for post-processing

---

## Current Implementation Analysis

### Architecture Overview

Our toon rendering system consists of two main components:

1. **Toon Lighting Shader** ([assets/shaders/toon_lighting.wgsl](../assets/shaders/toon_lighting.wgsl))
   - Material extension shader (fragment stage)
   - Quantizes colors to create cel-shaded look
   - Integrated into standard PBR pipeline

2. **Edge Detection Post-Process** ([src/game/post_process.rs](../src/game/post_process.rs))
   - ViewNode-based post-processing effect
   - Sobel operator for edge detection
   - Runs after tonemapping

### Toon Lighting Shader

**File**: [assets/shaders/toon_lighting.wgsl](../assets/shaders/toon_lighting.wgsl)

**Integration**: Extends `StandardMaterial` via `MaterialExtension` pattern

```rust
// In world.rs
impl MaterialExtension for ToonMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/toon_lighting.wgsl".into()
    }
}
```

**Shader Pipeline**:

1. **Lighting Calculation** (View-Independent Lambertian)
   ```wgsl
   // Directional lights with shadows
   for light in directional_lights {
       diffuse += N·L * light.color * shadow
   }

   // Point lights with clustering + attenuation
   for light in point_lights {
       diffuse += N·L * light.color * attenuation(distance)
   }

   // Ambient lighting
   diffuse += ambient
   ```

2. **Tone Mapping** (Reinhard)
   ```wgsl
   color = color / (color + 1.0)
   ```
   Prevents overbright issues before quantization

3. **Color Quantization**
   ```wgsl
   let bands = 25.0;
   color.r = floor(color.r * bands) / bands;
   color.g = floor(color.g * bands) / bands;
   color.b = floor(color.b * bands) / bands;
   ```
   - 25 bands per channel = **15,625 possible colors** (25³)
   - Per-channel quantization allows full color spectrum
   - Creates distinctive toon "banding" effect

**Characteristics**:
- ✅ View-independent (no specular, Fresnel, or view-dependent terms)
- ✅ Proper shadow integration
- ✅ Works with Bevy's PBR lighting infrastructure
- ✅ Outputs quantized colors directly to ViewTarget

### Edge Detection Post-Process

**File**: [src/game/post_process.rs](../src/game/post_process.rs)

**Plugin Structure**:

```rust
impl Plugin for EdgeDetectionPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            // Main world → Render world sync
            ExtractComponentPlugin::<EdgeDetectionSettings>::default(),
            // CPU → GPU uniform buffer
            UniformComponentPlugin::<EdgeDetectionSettings>::default(),
        ));

        let render_app = app.sub_app_mut(RenderApp);
        render_app
            .add_systems(Render, prepare_edge_detection_pipelines.in_set(RenderSet::Prepare))
            .add_render_graph_node::<ViewNodeRunner<EdgeDetectionNode>>(
                Core3d,
                EdgeDetectionLabel,
            )
            .add_render_graph_edges(
                Core3d,
                (
                    Node3d::Tonemapping,
                    EdgeDetectionLabel,
                    Node3d::EndMainPassPostProcessing,
                ),
            );
    }
}
```

**Settings Component**:

```rust
#[derive(Component, Clone, Copy, ExtractComponent, ShaderType)]
pub struct EdgeDetectionSettings {
    pub edge_thickness: f32,          // Sobel kernel scaling (pixel units)
    pub luminance_sensitivity: f32,   // Brightness edge detection strength
    pub color_sensitivity: f32,       // Color gradient detection strength
    pub edge_color: LinearRgba,       // Edge overlay color
}
```

**Node Implementation**: See [ViewNode Pattern](#viewnode-implementation-pattern) above for full structure

**Shader Algorithm** ([assets/shaders/edge_detection_post.wgsl](../assets/shaders/edge_detection_post.wgsl)):

1. **Sample 3×3 Neighborhood** (9 texture samples)
   ```wgsl
   for (var y: i32 = -1; y <= 1; y++) {
       for (var x: i32 = -1; x <= 1; x++) {
           let sample_uv = uv + vec2<f32>(f32(x), f32(y)) * texel_size;
           let sample_color = textureSample(screen_texture, screen_sampler, sample_uv);
           // Store in 3x3 grid
       }
   }
   ```

2. **Sobel Operator** (Gradient Magnitude)
   ```wgsl
   // Horizontal kernel: [-1 0 1]
   //                    [-2 0 2]
   //                    [-1 0 1]
   let gx = sobel_horizontal(samples);

   // Vertical kernel: [-1 -2 -1]
   //                  [ 0  0  0]
   //                  [ 1  2  1]
   let gy = sobel_vertical(samples);

   let magnitude = sqrt(gx*gx + gy*gy);
   ```

3. **Dual-Channel Detection** (Luminance + Color)
   ```wgsl
   let luma_edge = sobel(luminance) * luminance_sensitivity;
   let color_edge = sobel(color) * color_sensitivity;
   let edge = max(luma_edge, color_edge);
   ```
   - **Luminance**: Detects brightness discontinuities
   - **Color**: Detects hue/saturation changes
   - Works well with toon-quantized colors

4. **Thresholding & Anti-Aliasing**
   ```wgsl
   let edge_strength = smoothstep(0.05, 0.15, edge);
   ```
   Soft threshold prevents aliasing

5. **Compositing**
   ```wgsl
   output_color = mix(base_color, edge_color, edge_strength);
   ```

### Data Flow in Render Graph

**Complete Pipeline** (Core3d sub-graph):

```
┌─────────────────────────────────────────────────────────────┐
│ Main 3D Rendering Passes                                    │
│ - Shadow maps                                               │
│ - Prepass (optional depth/normals)                          │
│ - Opaque meshes → Toon shader quantizes colors             │
│ - Transparent meshes                                        │
│ - Transmissive meshes                                       │
└──────────────────────┬──────────────────────────────────────┘
                       │ ViewTarget (HDR, quantized colors)
                       ↓
┌─────────────────────────────────────────────────────────────┐
│ Node3d::Tonemapping                                         │
│ - HDR → LDR conversion                                      │
│ - Color grading                                             │
│ (Note: Our toon colors already tonemapped in shader)       │
└──────────────────────┬──────────────────────────────────────┘
                       │ ViewTarget (LDR, quantized colors)
                       ↓
┌─────────────────────────────────────────────────────────────┐
│ EdgeDetectionLabel (OUR NODE)                               │
│ - Sobel edge detection                                      │
│ - Reads: post_process.source                                │
│ - Writes: post_process.destination                          │
└──────────────────────┬──────────────────────────────────────┘
                       │ ViewTarget (with edges)
                       ↓
┌─────────────────────────────────────────────────────────────┐
│ Node3d::EndMainPassPostProcessing                           │
│ - Marker node for post-processing ordering                  │
└──────────────────────┬──────────────────────────────────────┘
                       │
                       ↓
┌─────────────────────────────────────────────────────────────┐
│ Node3d::Upscaling                                           │
│ - Upscale to window resolution (if using render scale)     │
└──────────────────────┬──────────────────────────────────────┘
                       │
                       ↓ Final output to screen
```

**Key Points**:
- ✅ Toon shader runs during main 3D passes (parallel for all meshes)
- ✅ Edge detection is a single post-process pass (sequential)
- ✅ Edges are detected on tonemapped, quantized colors (intentional for toon aesthetic)
- ✅ ViewTarget double-buffering prevents extra allocations

### Why This Architecture Works

1. **Separation of Concerns**
   - Toon shading: Material shader (GPU-parallel per fragment)
   - Edge detection: Post-process (full-screen effect)
   - Each optimized for its task

2. **Correct Positioning**
   - Edges detected after tonemapping (correct for quantized colors)
   - Before upscaling (avoids edge artifacts from scaling)

3. **Efficient Resource Usage**
   - ViewTarget double-buffering (no extra texture allocations)
   - Pipeline caching (no per-frame compilation)
   - Proper uniform buffer management

---

## Optimization Recommendations

### Priority 1: Separable Sobel Filter ⭐

**Current**: 9 texture samples per pixel (3×3 Sobel kernel)

**Optimized**: 6 texture samples per pixel (separable 1D kernels)

**How It Works**:

The 2D Sobel operator can be factored into two 1D convolutions:

```
Horizontal Sobel = [-1 0 1] ⊗ [1 2 1]ᵀ
Vertical Sobel   = [1 2 1] ⊗ [-1 0 1]ᵀ
```

**Implementation Strategy**:

1. **Pass 1: Horizontal Edges**
   ```wgsl
   // Convolve with [-1, 0, 1]
   gx = -1*left + 0*center + 1*right  // 3 samples

   // Apply Gaussian [1, 2, 1]
   gx = 1*gx_above + 2*gx_center + 1*gx_below  // 3 samples
   ```

2. **Pass 2: Vertical Edges** (same process, transposed)

3. **Pass 3: Combine & Composite**
   ```wgsl
   edge = sqrt(gx*gx + gy*gy)
   output = mix(color, edge_color, edge)
   ```

**Alternative**: Store horizontal/vertical gradients in intermediate render target, combine in final pass.

**Expected Performance Gain**:
- **Texture Samples**: 9 → 6 (33% reduction)
- **Overall Edge Detection**: 10-20% faster
- **Quality**: Mathematically equivalent to full Sobel

**Implementation Notes**:
- Requires creating intermediate render target or modifying shader to multi-pass
- Consider storing gradients in RG channels (gx, gy) then combining
- Could also store edge strength in single channel for further optimization

### Priority 2: Depth-Based Edge Detection (Advanced)

**Current**: Color-based Sobel operator

**Alternative**: Depth discontinuities for geometric edges

**How It Works**:

Depth buffer already contains geometric information. Sharp changes in depth = object boundaries.

**Implementation**:

```rust
// Add to camera
commands.spawn((
    Camera3d::default(),
    EdgeDetectionSettings::default(),
    DepthPrepass,  // Enable depth prepass
));
```

```wgsl
// In edge detection shader
@group(1) @binding(0) var depth_texture: texture_depth_2d;

fn detect_depth_edge(uv: vec2<f32>, texel_size: vec2<f32>) -> f32 {
    let center = textureSample(depth_texture, sampler, uv);

    // Sample 4-connected neighbors
    let left   = textureSample(depth_texture, sampler, uv + vec2(-texel_size.x, 0.0));
    let right  = textureSample(depth_texture, sampler, uv + vec2( texel_size.x, 0.0));
    let top    = textureSample(depth_texture, sampler, uv + vec2(0.0, -texel_size.y));
    let bottom = textureSample(depth_texture, sampler, uv + vec2(0.0,  texel_size.y));

    // Depth discontinuity
    let diff = abs(center - left) + abs(center - right) +
               abs(center - top) + abs(center - bottom);

    return saturate(diff * depth_sensitivity);
}
```

**Benefits**:
- **Much faster**: 5 depth samples vs 9 color samples + Sobel computation
- **Precise geometric edges**: Detects object boundaries perfectly
- **View-space awareness**: Edges scale properly with distance

**Drawbacks**:
- Requires depth prepass (small additional cost)
- Doesn't detect texture/color edges (e.g., painted lines on flat surface)
- Best combined with color-based detection

**Hybrid Approach** (Recommended):
```wgsl
let depth_edge = detect_depth_edge(uv, texel_size);
let color_edge = detect_color_edge(uv, texel_size);
let final_edge = max(depth_edge, color_edge * 0.5);  // Prioritize depth
```

### Priority 3: Adaptive Quality Settings

**Current**: Always runs full-quality edge detection

**Optimization**: Skip or reduce quality based on settings

```rust
#[derive(Component)]
pub struct EdgeDetectionSettings {
    pub edge_thickness: f32,
    pub luminance_sensitivity: f32,
    pub color_sensitivity: f32,
    pub edge_color: LinearRgba,
    pub quality: EdgeQuality,  // NEW
}

pub enum EdgeQuality {
    Off,      // Skip edge detection entirely
    Low,      // Depth-based only (fast)
    Medium,   // Color-based only (current)
    High,     // Depth + Color hybrid (best)
}
```

```rust
impl ViewNode for EdgeDetectionNode {
    fn run(...) -> Result<(), NodeRunError> {
        let (view_target, settings, uniform_index) = view_query;

        // Early exit for disabled edges
        if matches!(settings.quality, EdgeQuality::Off) {
            return Ok(());
        }

        // Select appropriate pipeline
        let pipeline_id = match settings.quality {
            EdgeQuality::Low => pipeline.depth_only_id,
            EdgeQuality::Medium => pipeline.color_only_id,
            EdgeQuality::High => pipeline.hybrid_id,
            _ => unreachable!(),
        };

        // ... rest of rendering
    }
}
```

**Benefits**:
- User can toggle edges for performance
- Different quality levels for different hardware
- Could auto-adjust based on framerate

### Priority 4: Downsampled Edge Detection

**Current**: Edge detection at full resolution

**Optimization**: Detect edges at lower resolution, upscale mask

**Strategy**:

1. **Downsample**: Detect edges at 0.5× resolution (4× fewer pixels)
2. **Upscale**: Bilinear upscale edge mask to full resolution
3. **Composite**: Apply upscaled mask to full-res color

**Implementation** (requires additional render targets):

```rust
// Create half-resolution target
let half_res_texture = texture_cache.get(&TextureDescriptor {
    size: Extent3d {
        width: full_width / 2,
        height: full_height / 2,
        depth_or_array_layers: 1,
    },
    format: TextureFormat::R8Unorm,  // Single channel for edge mask
    // ...
});

// Pass 1: Detect edges at half resolution
// Pass 2: Upscale + composite at full resolution
```

**Expected Gain**: 75% fewer pixels processed in edge detection

**Trade-off**: Slightly less precise edges (usually acceptable for stylized look)

### Performance Optimization Checklist

**Current Implementation** (already optimized):

- ✅ Cached pipeline (no per-frame compilation)
- ✅ Single fullscreen triangle (efficient)
- ✅ ViewTarget double-buffering (no extra allocations)
- ✅ Dynamic uniforms (multi-camera support)

**Potential Improvements**:

- ⚠️ **Bind Group Caching**: Currently created per-frame (necessary for ViewTarget, but could cache sampler/uniforms)
- ⚠️ **Early Exit**: No early exit for low-intensity settings
- ⚠️ **Compute Shader**: Could use compute with shared memory for better cache coherency

### Not Recommended

**Combined Toon + Edge Pass**:
- ❌ Extremely complex (need neighbor fragments in fragment shader)
- ❌ Requires compute shader or major refactoring
- ❌ Minimal performance gain vs. current architecture
- ❌ Loss of modularity

**Custom Texture Formats**:
- ❌ Current formats are appropriate (Rgba8 sufficient for 25-band quantization)
- ❌ No precision issues with quantized colors

---

## Code Examples

### Example 1: Basic ViewNode Post-Process

```rust
use bevy::prelude::*;
use bevy::render::render_graph::{ViewNode, NodeRunError, RenderGraphContext};
use bevy::render::renderer::RenderContext;
use bevy::core_pipeline::core_3d::{ViewTarget, graph::{Core3d, Node3d}};

pub struct MyEffectPlugin;

impl Plugin for MyEffectPlugin {
    fn build(&self, app: &mut App) {
        use bevy::render::RenderApp;

        app.add_plugins((
            ExtractComponentPlugin::<MyEffectSettings>::default(),
            UniformComponentPlugin::<MyEffectSettings>::default(),
        ));

        let render_app = app.sub_app_mut(RenderApp);
        render_app
            .init_resource::<MyEffectPipeline>()
            .add_systems(Render, prepare_my_effect_pipeline.in_set(RenderSet::Prepare))
            .add_render_graph_node::<ViewNodeRunner<MyEffectNode>>(
                Core3d,
                MyEffectLabel,
            )
            .add_render_graph_edges(
                Core3d,
                (Node3d::Tonemapping, MyEffectLabel, Node3d::EndMainPassPostProcessing),
            );
    }
}

#[derive(Component, Clone, Copy, ExtractComponent, ShaderType)]
pub struct MyEffectSettings {
    pub intensity: f32,
}

impl Default for MyEffectSettings {
    fn default() -> Self {
        Self { intensity: 1.0 }
    }
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub struct MyEffectLabel;

pub struct MyEffectNode;

impl ViewNode for MyEffectNode {
    type ViewQuery = (
        &'static ViewTarget,
        &'static MyEffectSettings,
        &'static DynamicUniformIndex<MyEffectSettings>,
    );

    fn run(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext,
        (view_target, _settings, uniform_index): QueryItem<Self::ViewQuery>,
        world: &World,
    ) -> Result<(), NodeRunError> {
        let pipeline_cache = world.resource::<PipelineCache>();
        let pipeline = world.resource::<MyEffectPipeline>();

        let Some(pipeline_id) = pipeline_cache.get_render_pipeline(pipeline.pipeline_id) else {
            return Ok(()); // Pipeline not ready
        };

        let post_process = view_target.post_process_write();

        let bind_group = render_context.render_device().create_bind_group(
            "my_effect_bind_group",
            &pipeline.layout,
            &BindGroupEntries::sequential((
                post_process.source,
                &pipeline.sampler,
                uniform_index.binding(),
            )),
        );

        let mut render_pass = render_context.begin_tracked_render_pass(
            RenderPassDescriptor {
                label: Some("my_effect_pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: post_process.destination,
                    resolve_target: None,
                    ops: Operations::default(),
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            },
        );

        render_pass.set_render_pipeline(pipeline_id);
        render_pass.set_bind_group(0, &bind_group, &[uniform_index.index()]);
        render_pass.draw(0..3, 0..1);

        Ok(())
    }
}
```

### Example 2: Separable Sobel Pseudo-Code

```wgsl
// Pass 1: Horizontal Gradient

@fragment
fn horizontal_pass(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = position.xy / view.viewport.zw;
    let texel_size = settings.edge_thickness / view.viewport.zw;

    // Sample 3 pixels horizontally
    let left   = textureSample(screen_texture, screen_sampler, uv + vec2(-texel_size.x, 0.0));
    let center = textureSample(screen_texture, screen_sampler, uv);
    let right  = textureSample(screen_texture, screen_sampler, uv + vec2( texel_size.x, 0.0));

    // Sobel horizontal: [-1, 0, 1]
    let gx = (right.rgb - left.rgb) * 0.5;

    // Store gradient in RGB, original luminance in A
    return vec4(gx, luminance(center.rgb));
}

// Pass 2: Vertical Gradient + Combine

@fragment
fn vertical_combine_pass(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = position.xy / view.viewport.zw;
    let texel_size = settings.edge_thickness / view.viewport.zw;

    // Sample 3 pixels vertically from horizontal pass output
    let top    = textureSample(horizontal_texture, screen_sampler, uv + vec2(0.0, -texel_size.y));
    let center = textureSample(horizontal_texture, screen_sampler, uv);
    let bottom = textureSample(horizontal_texture, screen_sampler, uv + vec2(0.0,  texel_size.y));

    // Extract horizontal gradients (stored in RGB)
    let gx = center.rgb;

    // Sobel vertical on luminance: [-1, 0, 1]
    let gy = (bottom.a - top.a) * vec3(0.5);

    // Combine gradients
    let edge = length(gx) + length(gy);
    let edge_strength = smoothstep(0.05, 0.15, edge);

    // Get original color
    let base_color = textureSample(screen_texture, screen_sampler, uv);

    // Composite
    return mix(base_color, settings.edge_color, edge_strength);
}
```

### Example 3: Depth-Based Edges

```wgsl
@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var depth_texture: texture_depth_2d;
@group(0) @binding(2) var texture_sampler: sampler;

fn detect_depth_edge(uv: vec2<f32>, texel_size: vec2<f32>, sensitivity: f32) -> f32 {
    // Sample depth in cross pattern (5 samples)
    let d_c = textureSample(depth_texture, texture_sampler, uv);
    let d_l = textureSample(depth_texture, texture_sampler, uv + vec2(-texel_size.x, 0.0));
    let d_r = textureSample(depth_texture, texture_sampler, uv + vec2( texel_size.x, 0.0));
    let d_t = textureSample(depth_texture, texture_sampler, uv + vec2(0.0, -texel_size.y));
    let d_b = textureSample(depth_texture, texture_sampler, uv + vec2(0.0,  texel_size.y));

    // Depth discontinuity (Roberts cross or simple diff)
    let edge = abs(d_c - d_l) + abs(d_c - d_r) + abs(d_c - d_t) + abs(d_c - d_b);

    // Scale by sensitivity and linearize
    // (Depth buffer is non-linear, might want to linearize first)
    return saturate(edge * sensitivity);
}

@fragment
fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = position.xy / view.viewport.zw;
    let texel_size = 1.0 / view.viewport.zw;

    // Detect edges from depth
    let depth_edge = detect_depth_edge(uv, texel_size, settings.depth_sensitivity);

    // Optional: Combine with color-based edges
    let color_edge = detect_color_edge(uv, texel_size);
    let combined_edge = max(depth_edge, color_edge * 0.5);

    // Composite
    let base_color = textureSample(screen_texture, texture_sampler, uv);
    return mix(base_color, settings.edge_color, combined_edge);
}
```

---

## References

### Bevy Source Files

**Core Render Graph**:
- [`bevy_render/src/render_graph/mod.rs`](https://github.com/bevyengine/bevy/blob/main/crates/bevy_render/src/render_graph/mod.rs) - Main graph types
- [`bevy_render/src/render_graph/node.rs`](https://github.com/bevyengine/bevy/blob/main/crates/bevy_render/src/render_graph/node.rs) - Node trait definitions

**Core 3D Pipeline**:
- [`bevy_core_pipeline/src/core_3d/main_opaque_pass_3d_node.rs`](https://github.com/bevyengine/bevy/blob/main/crates/bevy_core_pipeline/src/core_3d/main_opaque_pass_3d_node.rs) - Main 3D rendering
- [`bevy_core_pipeline/src/tonemapping/node.rs`](https://github.com/bevyengine/bevy/blob/main/crates/bevy_core_pipeline/src/tonemapping/node.rs) - Tonemapping node example

**ViewTarget System**:
- [`bevy_core_pipeline/src/core_3d/mod.rs`](https://github.com/bevyengine/bevy/blob/main/crates/bevy_core_pipeline/src/core_3d/mod.rs) - ViewTarget implementation

**Post-Processing Examples** (in Bevy examples):
- `examples/shader/post_processing.rs` - Basic post-process example
- `examples/shader/custom_post_process.rs` - Advanced techniques

### Our Implementation

**Source Files**:
- [src/game/post_process.rs](../src/game/post_process.rs) - Edge detection plugin & node
- [src/game/world.rs](../src/game/world.rs) - Toon material integration
- [assets/shaders/toon_lighting.wgsl](../assets/shaders/toon_lighting.wgsl) - Toon shader
- [assets/shaders/edge_detection_post.wgsl](../assets/shaders/edge_detection_post.wgsl) - Edge detection shader

### Further Reading

**Bevy Rendering Documentation**:
- [Unofficial Bevy Cheat Book - Render Pipelines](https://bevy-cheatbook.github.io/gpu/intro.html)
- [Bevy GPU Docs](https://docs.rs/bevy/latest/bevy/render/index.html)

**Graphics Techniques**:
- **Sobel Operator**: [Wikipedia - Sobel Operator](https://en.wikipedia.org/wiki/Sobel_operator)
- **Separable Filters**: [Wikipedia - Separable Filter](https://en.wikipedia.org/wiki/Separable_filter)
- **Cel Shading**: [Wikipedia - Cel Shading](https://en.wikipedia.org/wiki/Cel_shading)
- **Edge Detection Methods**: *Real-Time Rendering* (Chapter on Non-Photorealistic Rendering)

---

## Summary

### Current State ✅

Your implementation is **production-quality** and follows Bevy best practices:
- Proper ViewNode pattern
- Correct render graph positioning
- Efficient ViewTarget usage
- Clean separation of concerns (toon shader vs. edge detection)

### Quick Win Optimizations ⚡

1. **Separable Sobel** (Priority 1): 33% fewer texture samples, 10-20% performance gain
2. **Depth-based Edges** (Priority 2): Significant speedup, requires depth prepass
3. **Adaptive Quality** (Priority 3): User-configurable performance vs. quality

### Architecture Insights 🏗️

- Render graph is a DAG of GPU operations
- ViewNode simplifies per-camera effects
- ViewTarget enables efficient post-processing chains
- Your toon + edge system leverages all these effectively

### Next Steps 🚀

If implementing optimizations:
1. Start with separable Sobel (easiest, good gains)
2. Add adaptive quality settings (user control)
3. Experiment with depth-based edges (advanced optimization)

The render graph system is flexible enough to support all these without major refactoring!

---

*Last updated: 2025-11-20*
