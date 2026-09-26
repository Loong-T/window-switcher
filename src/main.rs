#![windows_subsystem = "windows"]

use anyhow::{anyhow, bail, Result};
use std::{
    fs::{File, OpenOptions},
    path::Path,
    thread,
    time::Duration,
};

use window_switcher::{alert, load_config, open_settings, start, utils::SingleInstance};

const SINGLE_INSTANCE_NAME: &str = "WindowSwitcherMutex";

fn main() {
    if let Err(err) = run() {
        alert!("{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_SYSTEM_AWARE,
        );
    }

    let args: Vec<String> = std::env::args().collect();
    // Open the settings GUI without starting the background switcher.
    if args.get(1).map(String::as_str) == Some("settings") {
        return open_settings(None);
    }
    // Spawned by the settings dialog's restart: the previous instance may
    // still be shutting down, so wait for it to release the mutex first.
    let wait_for_previous = args.get(1).map(String::as_str) == Some("--restart");

    // Hold the single-instance lock until the app exits (`_` would drop it now).
    let _instance = acquire_single_instance(wait_for_previous)?;

    let config = load_config().unwrap_or_default();
    if let Some(log_file) = &config.log_file {
        let file = prepare_log_file(log_file).map_err(|err| {
            anyhow!(
                "Failed to prepare log file at {}, {err}",
                log_file.display()
            )
        })?;
        simple_logging::log_to(file, config.log_level);
    }
    start(&config)
}

fn acquire_single_instance(wait: bool) -> Result<SingleInstance> {
    if !wait {
        let instance = SingleInstance::create(SINGLE_INSTANCE_NAME)?;
        if !instance.is_single() {
            bail!("Another instance is running. This instance will abort.")
        }
        return Ok(instance);
    }
    for _ in 0..50 {
        let instance = SingleInstance::create(SINGLE_INSTANCE_NAME)?;
        if instance.is_single() {
            return Ok(instance);
        }
        thread::sleep(Duration::from_millis(200));
    }
    bail!("Failed to restart: another instance is still running.")
}

fn prepare_log_file(path: &Path) -> std::io::Result<File> {
    if path.exists() {
        OpenOptions::new().append(true).open(path)
    } else {
        File::create(path)
    }
}
