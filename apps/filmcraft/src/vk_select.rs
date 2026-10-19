//! Fork §1a.105 (b): start without NVIDIA's Vulkan driver when the app will draw on the Intel GPU.
//!
//! Every Vulkan instance loads every installed driver, and enumerating NVIDIA's woke the XPS 16's
//! RTX 4050 from runtime D3 for ~20 s at each start, even with Settings ▸ Playback ▸ Graphics device
//! on Power saving (FORK-PLAN §7 2026-10-17 07:30). The loader's `VK_LOADER_DRIVERS_SELECT` limits
//! it to the drivers named; it must be set before the first instance, and setting environment
//! variables in a running process is unsafe, so the app runs itself again (`exec`: same process id,
//! nothing opened yet) with the variable set, once. [`filmcraft_gpu::adapter_pick::igpu_only_drivers`]
//! decides; anything unexpected starts the app with every driver, as before. NVDEC / NVENC don't use
//! Vulkan, so the NVIDIA GPU still decodes and encodes when asked. Settings ▸ Performance takes
//! effect at the next start (the setting already says so).

use std::os::unix::process::CommandExt;

/// Set on the run that has the drivers selected (never restart twice).
const MARK: &str = "FILMCRAFT_VK_SELECTED";

/// Run the app again with only the Intel Vulkan driver when `graphics_device` (Settings' stored
/// value) and the machine call for it. Returns only when it doesn't (or `exec` failed).
pub fn restart_on_the_intel_driver_if_wanted(graphics_device: &str) {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    let loader_env_set = ["VK_LOADER_DRIVERS_SELECT", "VK_LOADER_DRIVERS_DISABLE", "VK_DRIVER_FILES", "VK_ICD_FILENAMES", MARK].iter().any(|k| set(k));
    let pref = filmcraft_ui_egui::graphics_device::preference(graphics_device);
    let drivers = filmcraft_gpu::adapter_pick::igpu_only_drivers(
        pref,
        filmcraft_gpu::adapter_pick::driver_override(),
        filmcraft_engine::power::has_battery(),
        &gpu_vendors(),
        &icd_manifests(),
        loader_env_set,
    );
    let Some(select) = drivers else { return };
    let Ok(exe) = std::env::current_exe() else { return };
    let err = std::process::Command::new(exe).args(std::env::args_os().skip(1)).env("VK_LOADER_DRIVERS_SELECT", select).env(MARK, "1").exec();
    log::warn!("starting with Intel's Vulkan driver only failed ({err}); starting with every driver");
}

/// The PCI vendor ids of the DRM cards (sysfs; reading it wakes no GPU).
fn gpu_vendors() -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else { return Vec::new() };
    entries
        .flatten()
        .take(64)
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with("card") && !n.contains('-')))
        .filter_map(|e| std::fs::read_to_string(e.path().join("device/vendor")).ok())
        .filter_map(|v| u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok())
        .collect()
}

/// The file names of the installed Vulkan driver manifests (the loader's standard folders).
fn icd_manifests() -> Vec<String> {
    let mut dirs = vec!["/usr/share/vulkan/icd.d".to_string(), "/usr/local/share/vulkan/icd.d".to_string(), "/etc/vulkan/icd.d".to_string()];
    if let Some(home) =
        std::env::var_os("XDG_DATA_HOME").or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".local/share").into_os_string()))
    {
        dirs.push(std::path::Path::new(&home).join("vulkan/icd.d").to_string_lossy().into_owned());
    }
    dirs.iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|e| e.flatten().take(64))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect()
}
