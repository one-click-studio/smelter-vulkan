use std::fs::{create_dir_all, File};
use std::io::copy;
use std::path::PathBuf;

const INPUT_URL: &str = "https://f003.backblazeb2.com/file/ocs-public/test-assets/input1.mp4";

pub fn assets_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/tmp/assets")
}

pub fn ensure_asset(path: &PathBuf, url: &str) -> anyhow::Result<()> {
    if !path.exists() {
        let client = reqwest::blocking::Client::new();
        let mut response = client.get(url).send()?.error_for_status()?;
        let mut file = File::create(path)?;
        copy(&mut response, &mut file)?;
        tracing::info!("Downloaded asset to {:?}", path);
    }
    Ok(())
}

pub fn download_input_asset() -> anyhow::Result<PathBuf> {
    let dir = assets_path();
    create_dir_all(&dir)?;
    let path = dir.join("input1.mp4");
    ensure_asset(&path, INPUT_URL)?;
    Ok(path)
}
