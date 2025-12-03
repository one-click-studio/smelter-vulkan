use std::{fs, path::PathBuf, sync::Arc, thread, time::Duration};

use anyhow::Result;
use smelter_render::OutputId;

mod assets;
mod compositor;

/// How long each recording lasts.
const RECORDING_DURATION: Duration = Duration::from_secs(5);
/// Number of parallel recordings per cycle.
const NUM_RECORDINGS: u32 = 1;
/// Total number of record/stop cycles to perform.
const NUM_CYCLES: u32 = 10;
/// Wait time between consecutive cycles.
const PAUSE_BETWEEN_CYCLES: Duration = Duration::from_secs(5);

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter("error,smelter_vulkan=info")
        .init();

    let input_path = assets::download_input_asset()?;
    let compositor = compositor::Compositor::new(input_path)?;

    let folder_path: PathBuf = "./recordings/".into();
    fs::create_dir_all(&folder_path)?;

    for cycle in 0..NUM_CYCLES {
        tracing::info!("Starting cycle {}/{}", cycle + 1, NUM_CYCLES);

        // Start all recordings in parallel
        let mut output_ids = Vec::new();
        for i in 0..NUM_RECORDINGS {
            let output_id = OutputId(Arc::from(format!("recording_output_{}_{}", cycle, i)));
            let file_path = folder_path.join(format!("recording_{}_{}.mp4", cycle, i));
            compositor.start_record(file_path, output_id.clone())?;
            output_ids.push(output_id);
        }

        // Wait for the recording duration
        tracing::info!("Recording for {:?}", RECORDING_DURATION);
        thread::sleep(RECORDING_DURATION);

        // Stop all recordings
        for output_id in output_ids {
            compositor.stop_record(output_id)?;
        }
        tracing::info!("Cycle {}/{} completed", cycle + 1, NUM_CYCLES);

        // Pause between cycles (except after the last one)
        if cycle + 1 < NUM_CYCLES {
            tracing::info!("Pausing for {:?} before next cycle", PAUSE_BETWEEN_CYCLES);
            thread::sleep(PAUSE_BETWEEN_CYCLES);
        }
    }

    tracing::info!("All cycles completed");

    Ok(())
}
