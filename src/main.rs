use anyhow::Result;

mod assets;
mod compositor;

const RECORDING_DURATION_SECS: u64 = 600;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter("error,smelter_vulkan=info")
        .init();

    let input_path = assets::download_input_asset()?;
    let compositor = compositor::Compositor::new(input_path)?;

    let folder_path = "./recordings/".into();
    std::fs::create_dir_all(&folder_path)?;

    compositor.start_recording(folder_path, std::time::Duration::from_secs(RECORDING_DURATION_SECS))?;

    Ok(())
}
