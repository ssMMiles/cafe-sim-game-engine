use bevy::prelude::*;
use std::collections::VecDeque;

use crate::game::{
    gfx::camera::{RENDER_TEXTURE_HEIGHT, RENDER_TEXTURE_WIDTH},
    world::terrain::HeightmapData,
};

const WINDOW_SIZE: usize = 300;

#[derive(Resource, Default)]
pub struct PerfSummary {
    pub frame_times: VecDeque<f32>,
    pub frame_count: u64,
}

/// Written by instrumented systems each frame they run.
#[derive(Resource, Default)]
pub struct SystemTimings {
    // finalize_computed_chunks
    pub finalize_sum_secs: f32,
    pub finalize_max_secs: f32,
    pub finalize_calls: u32,
    // update_streaming_ring
    pub ring_sum_secs: f32,
    pub ring_max_secs: f32,
    pub ring_calls: u32,
    // update_terrain_lod (only frames where it actually ran)
    pub lod_sum_secs: f32,
    pub lod_max_secs: f32,
    pub lod_calls: u32,
    // launch_chunk_tasks
    pub launch_sum_secs: f32,
    pub launch_max_secs: f32,
    pub launch_calls: u32,
    // streaming state snapshot (written by ring system)
    pub chunks_loaded: usize,
    pub chunks_computing: usize,
    pub chunks_pending: usize,
}

fn collect_frame_time(time: Res<Time<Real>>, mut summary: ResMut<PerfSummary>) {
    if summary.frame_times.len() >= WINDOW_SIZE {
        summary.frame_times.pop_front();
    }
    summary.frame_times.push_back(time.delta_secs());
    summary.frame_count += 1;
}

fn log_perf_summary(
    summary: Res<PerfSummary>,
    mut timings: ResMut<SystemTimings>,
    heightmap: Option<Res<HeightmapData>>,
    entities: Query<Entity>,
) {
    if summary.frame_count % WINDOW_SIZE as u64 != 0 || summary.frame_times.is_empty() {
        return;
    }

    let n = summary.frame_times.len() as f32;
    let avg = summary.frame_times.iter().sum::<f32>() / n;
    let mut sorted: Vec<f32> = summary.frame_times.iter().copied().collect();
    sorted.sort_by(f32::total_cmp);
    let p95 = sorted[((sorted.len() as f32 * 0.95) as usize).min(sorted.len() - 1)];
    let max = sorted.last().copied().unwrap_or(0.0);

    let finalize_avg = timings.finalize_sum_secs / timings.finalize_calls.max(1) as f32;
    let ring_avg = timings.ring_sum_secs / timings.ring_calls.max(1) as f32;
    let lod_avg = timings.lod_sum_secs / timings.lod_calls.max(1) as f32;
    let launch_avg = timings.launch_sum_secs / timings.launch_calls.max(1) as f32;

    let tile_count = heightmap.as_ref().map(|h| h.tile_count()).unwrap_or(0);
    let entity_count = entities.iter().count();

    info!("[PERF] ── Frame Stats ({WINDOW_SIZE} frames) ──────────────────────────");
    info!(
        "[PERF]   avg={:.1}ms ({:.0}fps)  p95={:.0}ms  max={:.0}ms",
        avg * 1000.0,
        1.0 / avg.max(f32::EPSILON),
        p95 * 1000.0,
        max * 1000.0
    );
    info!("[PERF] ── System Timings (avg / max) ──────────────────────────────");
    info!(
        "[PERF]   finalize_computed_chunks : {:.2}ms / {:.1}ms  [{} calls]",
        finalize_avg * 1000.0,
        timings.finalize_max_secs * 1000.0,
        timings.finalize_calls
    );
    info!(
        "[PERF]   update_streaming_ring    : {:.3}ms / {:.2}ms",
        ring_avg * 1000.0,
        timings.ring_max_secs * 1000.0
    );
    info!(
        "[PERF]   launch_chunk_tasks       : {:.3}ms / {:.2}ms",
        launch_avg * 1000.0,
        timings.launch_max_secs * 1000.0
    );
    info!(
        "[PERF]   update_terrain_lod       : {:.3}ms / {:.2}ms  [ran {}/{WINDOW_SIZE} frames]",
        lod_avg * 1000.0,
        timings.lod_max_secs * 1000.0,
        timings.lod_calls
    );
    info!("[PERF] ── World State ───────────────────────────────────────────");
    info!(
        "[PERF]   chunks: loaded={}  computing={}  pending={}",
        timings.chunks_loaded, timings.chunks_computing, timings.chunks_pending
    );
    info!(
        "[PERF]   heightmap tiles: {}  entities: {}",
        tile_count, entity_count
    );
    info!("[PERF] ── Known suspects ────────────────────────────────────────");
    info!(
        "[PERF]   Render texture: {}x{} HDR (large GPU cost)",
        RENDER_TEXTURE_WIDTH, RENDER_TEXTURE_HEIGHT
    );
    info!("[PERF]   VolumetricFog step_count: 64 (GPU raymarching at 4K)");

    // Reset window accumulators; keep max values as rolling maximums.
    timings.finalize_sum_secs = 0.0;
    timings.finalize_calls = 0;
    timings.ring_sum_secs = 0.0;
    timings.ring_calls = 0;
    timings.launch_sum_secs = 0.0;
    timings.launch_calls = 0;
    timings.lod_sum_secs = 0.0;
    timings.lod_calls = 0;
}

pub struct PerfMonitorPlugin;

impl Plugin for PerfMonitorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PerfSummary>();
        app.init_resource::<SystemTimings>();
        app.add_plugins(bevy::diagnostic::EntityCountDiagnosticsPlugin::default());

        #[cfg(feature = "profiling")]
        app.add_plugins(bevy::diagnostic::SystemInformationDiagnosticsPlugin);

        // app.add_systems(Last, (collect_frame_time, log_perf_summary).chain());
    }
}
