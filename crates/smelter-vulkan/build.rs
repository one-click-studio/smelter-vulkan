use std::path::Path;

/// Checks that the `process_helper` binary exists in the target directory.
/// `process_helper` must be built before the main crate.
fn check_process_helper(target_path: &Path, profile: &str) -> Result<(), String> {
    let process_helper_path = target_path.join("process_helper");

    if !process_helper_path.exists() {
        let command = match profile {
            "debug" => "cargo build -p process_helper",
            "release" => "cargo build --release -p process_helper",
            _ => "cargo build -p process_helper",
        };
        return Err(format!(
            "Process helper not found at {:?}.\nBuild it first: `{}`",
            process_helper_path, command
        ));
    }

    println!("cargo:rerun-if-changed=../process_helper/src/main.rs");
    println!("Process helper found at: {:?}", process_helper_path);
    Ok(())
}

fn main() {
    // Only check on Linux where Chromium/CEF is supported
    #[cfg(target_os = "linux")]
    {
        let profile = std::env::var("PROFILE").unwrap();
        let out_dir = std::env::var("OUT_DIR").unwrap();
        let out_path = Path::new(&out_dir);

        // Navigate from OUT_DIR to target/{debug|release}
        // OUT_DIR is typically target/{debug|release}/build/{crate}/out
        let target_dir = out_path
            .ancestors()
            .nth(3)
            .expect("Failed to determine target directory");

        println!("Checking for process_helper in: {:?}", target_dir);

        if let Err(e) = check_process_helper(target_dir, &profile) {
            panic!("{}", e);
        }
    }
}
