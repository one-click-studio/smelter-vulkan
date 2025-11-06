use std::error::Error;

// Subprocess used by chromium/CEF for renderer, GPU, and other browser processes
fn main() -> Result<(), Box<dyn Error>> {
    let exit_code = smelter_render::web_renderer::process_helper::run_process_helper()?;
    std::process::exit(exit_code);
}
