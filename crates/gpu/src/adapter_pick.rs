//! Which GPU FilmCraft draws and exports on (fork Phase XPS a, LINUX-AUDIT L2.6): one rule for the
//! window, the DMA-BUF interop device, the headless export devices and the export renderer, so a
//! laptop with an integrated and a discrete GPU (the Dell XPS 16: Intel Arc iGPU + RTX 4050) never
//! draws on one GPU and exports on the other by accident.
//!
//! Settings ▸ Playback ▸ Graphics device (`playback.graphicsDevice`) stores one of: `""` (Auto),
//! `"power-saving"`, `"performance"` or a GPU's PCI ids (`"10de:28a1"`, matched by ids so a driver
//! update that renames the GPU doesn't lose it). Auto keeps the drivers' own overrides: with
//! `DRI_PRIME` or `MESA_VK_DEVICE_SELECT` set, the GPU the driver lists first wins (Mesa's
//! device-select layer puts the chosen GPU first); with `__NV_PRIME_RENDER_OFFLOAD=1` (what
//! `prime-run` sets) the NVIDIA GPU, because NVIDIA's offload layer does not reorder Vulkan's list
//! (XPS item 2: the Arc iGPU stays first on the XPS 16); otherwise a discrete GPU, as FilmCraft has
//! always done on desktops.
//!
//! Provisional (XPS, before the laptop): Auto on a hybrid laptop is the discrete GPU, as on a
//! desktop; the laptop runs (docs/fork/XPS-SETUP.md) measure whether Auto should prefer the
//! integrated GPU on battery.

use std::sync::RwLock;

/// What a preference asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GpuPref {
    /// The drivers' override when one is set, else a discrete GPU.
    #[default]
    Auto,
    /// An integrated GPU first (longer battery life, less heat).
    PowerSaving,
    /// A discrete GPU first.
    Performance,
    /// This GPU (PCI vendor, device); Auto when it isn't there.
    Pci(u32, u32),
}

/// The stored value for [`GpuPref::PowerSaving`].
pub const POWER_SAVING: &str = "power-saving";
/// The stored value for [`GpuPref::Performance`].
pub const PERFORMANCE: &str = "performance";

impl GpuPref {
    /// The preference a stored value names; anything else (empty, hostile, a number) is Auto.
    pub fn parse(s: &str) -> GpuPref {
        let s = s.trim();
        match s {
            POWER_SAVING => GpuPref::PowerSaving,
            PERFORMANCE => GpuPref::Performance,
            _ => parse_pci(s).map_or(GpuPref::Auto, |(v, d)| GpuPref::Pci(v, d)),
        }
    }

    /// The value Settings stores for this preference.
    pub fn stored(self) -> String {
        match self {
            GpuPref::Auto => String::new(),
            GpuPref::PowerSaving => POWER_SAVING.to_string(),
            GpuPref::Performance => PERFORMANCE.to_string(),
            GpuPref::Pci(v, d) => format!("{v:04x}:{d:04x}"),
        }
    }
}

/// The `(vendor, device)` PCI ids a value names (`1002:73df`); `None` unless it is exactly two
/// hex numbers of at most eight digits.
pub fn parse_pci(s: &str) -> Option<(u32, u32)> {
    let (v, d) = s.trim().split_once(':')?;
    let hex = |s: &str| (!s.is_empty() && s.len() <= 8 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| u32::from_str_radix(s, 16).ok()).flatten();
    Some((hex(v)?, hex(d)?))
}

/// An adapter as the rule sees it (from `wgpu::AdapterInfo`, or a fake list in tests).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdapterDesc {
    pub vendor: u32,
    pub device: u32,
    pub kind: wgpu::DeviceType,
}

impl From<&wgpu::AdapterInfo> for AdapterDesc {
    fn from(i: &wgpu::AdapterInfo) -> Self {
        AdapterDesc { vendor: i.vendor, device: i.device, kind: i.device_type }
    }
}

/// A GPU choice the environment makes for Auto.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DriverOverride {
    /// None set: Auto picks for itself.
    #[default]
    None,
    /// `DRI_PRIME` or `MESA_VK_DEVICE_SELECT`: the GPU the driver lists first.
    DriverFirst,
    /// `__NV_PRIME_RENDER_OFFLOAD=1`: the NVIDIA GPU.
    Nvidia,
}

impl DriverOverride {
    /// Whether the environment makes a choice at all.
    pub fn is_set(self) -> bool {
        self != DriverOverride::None
    }
}

/// NVIDIA's PCI vendor id.
const NVIDIA: u32 = 0x10de;
const INTEL: u32 = 0x8086;

/// The choice the environment makes for Auto: `__NV_PRIME_RENDER_OFFLOAD=1` names NVIDIA (any
/// other value is "off", as NVIDIA's driver reads it); `DRI_PRIME` or `MESA_VK_DEVICE_SELECT` set
/// to anything but empty hands it to the driver (`WGPU_ADAPTER_NAME` is wgpu's own and is read by
/// wgpu where it applies).
pub fn driver_override_from(get: impl Fn(&str) -> Option<String>) -> DriverOverride {
    if get("__NV_PRIME_RENDER_OFFLOAD").is_some_and(|v| v.trim() == "1") {
        return DriverOverride::Nvidia;
    }
    if ["DRI_PRIME", "MESA_VK_DEVICE_SELECT"].iter().any(|k| get(k).is_some_and(|v| !v.trim().is_empty())) {
        return DriverOverride::DriverFirst;
    }
    DriverOverride::None
}

/// Fork §1a.105 (b): what to set `VK_LOADER_DRIVERS_SELECT` to before the app starts, so the
/// Vulkan loader never loads NVIDIA's driver when the window and exports will be on the Intel GPU
/// anyway (enumerating NVIDIA's driver woke the XPS 16's RTX 4050 for ~20 s at every start).
/// `Some` only when: `pref` picks the Intel GPU (Power saving; Auto on a battery-powered computer,
/// as [`pick_adapter_for`]; or the Intel GPU by its ids), no driver override is set, the computer
/// has an Intel and an NVIDIA GPU (`gpu_vendors`, PCI vendor ids from sysfs), nothing set the
/// loader's driver variables already (`loader_env_set`), and an Intel Vulkan driver manifest is
/// installed (`icd_manifests`: file names; without one the app must start with every driver).
pub fn igpu_only_drivers(
    pref: GpuPref,
    driver_override: DriverOverride,
    battery_powered: bool,
    gpu_vendors: &[u32],
    icd_manifests: &[String],
    loader_env_set: bool,
) -> Option<&'static str> {
    let wants_intel = match pref {
        GpuPref::PowerSaving => true,
        GpuPref::Auto => battery_powered,
        GpuPref::Pci(vendor, _) => vendor == INTEL,
        GpuPref::Performance => false,
    };
    let hybrid = gpu_vendors.contains(&INTEL) && gpu_vendors.contains(&NVIDIA);
    let intel_driver = icd_manifests.iter().any(|m| m.to_ascii_lowercase().contains("intel"));
    (wants_intel && hybrid && intel_driver && !loader_env_set && driver_override == DriverOverride::None).then_some("*intel*")
}

/// [`driver_override_from`] on this process's environment.
pub fn driver_override() -> DriverOverride {
    driver_override_from(|k| std::env::var(k).ok())
}

/// Which of `adapters` (in the order the driver lists them) `pref` picks. `driver_override`:
/// [`driver_override`]. A named GPU that isn't there, or Auto: what the environment names (the
/// driver's first GPU, or NVIDIA's), else as [`GpuPref::Performance`]. Software renderers come
/// last in every case. `None` only for an empty list.
pub fn pick_adapter(pref: GpuPref, adapters: &[AdapterDesc], driver_override: DriverOverride) -> Option<usize> {
    pick_adapter_for(pref, adapters, driver_override, battery_powered())
}

/// [`pick_adapter`] on a computer that has a battery of its own (`battery_powered`) or not. Auto
/// on a battery-powered computer with an Intel integrated GPU and an NVIDIA GPU (a hybrid laptop
/// such as the XPS 16) picks the integrated one, on mains and on battery (fork XPS item 17,
/// measured on the XPS 16, synthetic X-M5 6.2K: playback 0.3 % dropped on the Arc vs 86–93 % on
/// the RTX 4050, 4K export 47–51 s vs 424–449 s, ~⅕ of the energy on battery). Intel decodes the
/// camera's HEVC and NVIDIA's driver can't import its tiled pictures, so on the NVIDIA GPU every
/// picture is read back through the CPU; until frames stay on the GPU (8.6c), the integrated GPU
/// wins. Performance still picks the NVIDIA GPU; a desktop (no battery) keeps the discrete GPU.
pub fn pick_adapter_for(pref: GpuPref, adapters: &[AdapterDesc], driver_override: DriverOverride, battery_powered: bool) -> Option<usize> {
    use wgpu::DeviceType as T;
    let first = |kind: T| adapters.iter().position(|a| a.kind == kind);
    let hardware = || adapters.iter().position(|a| a.kind != T::Cpu);
    let any = || (!adapters.is_empty()).then_some(0);
    let performance = || first(T::DiscreteGpu).or_else(|| first(T::IntegratedGpu)).or_else(hardware).or_else(any);
    match pref {
        GpuPref::Pci(v, d) => adapters.iter().position(|a| a.vendor == v && a.device == d).or_else(|| pick_adapter(GpuPref::Auto, adapters, driver_override)),
        GpuPref::PowerSaving => first(T::IntegratedGpu).or_else(|| first(T::DiscreteGpu)).or_else(hardware).or_else(any),
        GpuPref::Performance => performance(),
        GpuPref::Auto => match driver_override {
            DriverOverride::DriverFirst => hardware().or_else(any),
            DriverOverride::Nvidia => adapters.iter().position(|a| a.vendor == NVIDIA && a.kind != T::Cpu).or_else(performance),
            DriverOverride::None if battery_powered && hybrid_intel_nvidia(adapters) => first(T::IntegratedGpu).or_else(performance),
            DriverOverride::None => performance(),
        },
    }
}

/// An Intel integrated GPU next to an NVIDIA one.
fn hybrid_intel_nvidia(adapters: &[AdapterDesc]) -> bool {
    adapters.iter().any(|a| a.vendor == INTEL && a.kind == wgpu::DeviceType::IntegratedGpu)
        && adapters.iter().any(|a| a.vendor == NVIDIA && a.kind != wgpu::DeviceType::Cpu)
}

static PREF: RwLock<GpuPref> = RwLock::new(GpuPref::Auto);
static BATTERY_POWERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether this computer has a battery of its own (set once at start by the app, from the
/// kernel's power supplies; a desktop: false, the default).
pub fn set_battery_powered(on: bool) {
    BATTERY_POWERED.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// [`set_battery_powered`]'s value.
pub fn battery_powered() -> bool {
    BATTERY_POWERED.load(std::sync::atomic::Ordering::Relaxed)
}

/// The process's preference, set at start from Settings (like the window's GPU, a change in
/// Settings applies at the next start: the export renderer keeps the device it made).
pub fn set_preference(pref: GpuPref) {
    *PREF.write().unwrap_or_else(std::sync::PoisonError::into_inner) = pref;
}

/// The preference [`set_preference`] last set (Auto before that).
pub fn preference() -> GpuPref {
    *PREF.read().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The adapter of `instance` the process's preference picks, among `backends`.
#[cfg(not(target_arch = "wasm32"))]
pub fn choose(instance: &wgpu::Instance, backends: wgpu::Backends) -> Option<wgpu::Adapter> {
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    let descs: Vec<AdapterDesc> = adapters.iter().map(|a| AdapterDesc::from(&a.get_info())).collect();
    let i = pick_adapter(preference(), &descs, driver_override())?;
    adapters.into_iter().nth(i)
}

/// The PCI vendor of the GPU [`choose`] picks on all backends (what exports render on), listed
/// once per run; `None` without a GPU. Encoders use it to prefer the same GPU's video engine.
#[cfg(not(target_arch = "wasm32"))]
pub fn chosen_vendor() -> Option<u32> {
    static VENDOR: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *VENDOR.get_or_init(|| {
        std::panic::catch_unwind(|| {
            if wgpu::Instance::enabled_backend_features().is_empty() {
                return None;
            }
            let instance = wgpu::Instance::default();
            choose(&instance, wgpu::Backends::PRIMARY).map(|a| a.get_info().vendor)
        })
        .unwrap_or(None)
    })
}

/// On the web the browser picks the GPU: `None` (the caller asks wgpu for its default).
#[cfg(target_arch = "wasm32")]
pub fn choose(_instance: &wgpu::Instance, _backends: wgpu::Backends) -> Option<wgpu::Adapter> {
    None
}

#[cfg(test)]
mod tests {
    use super::DriverOverride::{DriverFirst, None as NoOverride, Nvidia};
    use super::*;
    use wgpu::DeviceType as T;

    const INTEL_ARC: AdapterDesc = AdapterDesc { vendor: 0x8086, device: 0x7d55, kind: T::IntegratedGpu };
    const RTX_4050: AdapterDesc = AdapterDesc { vendor: 0x10de, device: 0x28a1, kind: T::DiscreteGpu };
    const LLVMPIPE: AdapterDesc = AdapterDesc { vendor: 0x10005, device: 0, kind: T::Cpu };

    /// XPS item 17: Auto on a battery-powered Intel + NVIDIA computer picks the Arc, in either
    /// listing order; a desktop with the same GPUs keeps the discrete one; Performance, a named GPU
    /// and the drivers' overrides are as before; an AMD + NVIDIA or NVIDIA-only laptop is unchanged.
    #[test]
    fn auto_on_a_hybrid_laptop_picks_the_integrated_gpu() {
        for list in [[INTEL_ARC, RTX_4050, LLVMPIPE], [RTX_4050, INTEL_ARC, LLVMPIPE]] {
            let arc = list.iter().position(|a| *a == INTEL_ARC);
            let rtx = list.iter().position(|a| *a == RTX_4050);
            assert_eq!(pick_adapter_for(GpuPref::Auto, &list, NoOverride, true), arc, "laptop: the Arc");
            assert_eq!(pick_adapter_for(GpuPref::Auto, &list, NoOverride, false), rtx, "desktop: the discrete GPU");
            assert_eq!(pick_adapter_for(GpuPref::Performance, &list, NoOverride, true), rtx, "Performance");
            assert_eq!(pick_adapter_for(GpuPref::Pci(0x10de, 0x28a1), &list, NoOverride, true), rtx, "by name");
            assert_eq!(pick_adapter_for(GpuPref::Auto, &list, Nvidia, true), rtx, "prime-run");
            assert_eq!(pick_adapter_for(GpuPref::Auto, &list, DriverFirst, true), Some(0), "DRI_PRIME / MESA_VK_DEVICE_SELECT");
        }
        let amd_igpu = AdapterDesc { vendor: 0x1002, device: 0x15bf, kind: T::IntegratedGpu };
        assert_eq!(pick_adapter_for(GpuPref::Auto, &[amd_igpu, RTX_4050], NoOverride, true), Some(1), "AMD + NVIDIA: not measured, unchanged");
        assert_eq!(pick_adapter_for(GpuPref::Auto, &[RTX_4050, LLVMPIPE], NoOverride, true), Some(0));
        assert_eq!(pick_adapter_for(GpuPref::Auto, &[], NoOverride, true), None);
    }

    #[test]
    fn stored_values_round_trip_and_hostile_ones_are_auto() {
        for p in [GpuPref::Auto, GpuPref::PowerSaving, GpuPref::Performance, GpuPref::Pci(0x10de, 0x28a1)] {
            assert_eq!(GpuPref::parse(&p.stored()), p);
        }
        assert_eq!(GpuPref::parse(" 8086:7d55 "), GpuPref::Pci(0x8086, 0x7d55));
        for bad in ["", "auto", "Power-Saving", "1002", ":73df", "1002:", "zz:73df", "1002:73df:1", "123456789:1", "-1:2", "+1:2", "１002:73df"] {
            assert_eq!(GpuPref::parse(bad), GpuPref::Auto, "{bad}");
        }
    }

    /// The XPS 16 as Vulkan lists it in both orders (NVIDIA's ICD may come first or second), with
    /// Mesa's software renderer as well.
    #[test]
    fn xps16_layout_every_preference() {
        let layouts: [&[AdapterDesc]; 2] = [&[INTEL_ARC, RTX_4050, LLVMPIPE], &[LLVMPIPE, RTX_4050, INTEL_ARC]];
        for l in layouts {
            let at = |want: AdapterDesc| l.iter().position(|a| *a == want);
            assert_eq!(pick_adapter(GpuPref::Auto, l, NoOverride), at(RTX_4050));
            assert_eq!(pick_adapter(GpuPref::Performance, l, NoOverride), at(RTX_4050));
            assert_eq!(pick_adapter(GpuPref::PowerSaving, l, NoOverride), at(INTEL_ARC));
            assert_eq!(pick_adapter(GpuPref::Pci(0x8086, 0x7d55), l, NoOverride), at(INTEL_ARC));
            // a GPU from another computer's preferences: Auto
            assert_eq!(pick_adapter(GpuPref::Pci(0x1002, 0x73df), l, NoOverride), at(RTX_4050));
            // explicit choices beat the driver override; Auto follows the driver's first GPU
            assert_eq!(pick_adapter(GpuPref::PowerSaving, l, DriverFirst), at(INTEL_ARC));
            assert_eq!(pick_adapter(GpuPref::Auto, l, DriverFirst), l.iter().position(|a| a.kind != T::Cpu));
            // prime-run (XPS item 2): NVIDIA whatever the order; explicit choices still win
            assert_eq!(pick_adapter(GpuPref::Auto, l, Nvidia), at(RTX_4050));
            assert_eq!(pick_adapter(GpuPref::PowerSaving, l, Nvidia), at(INTEL_ARC));
        }
    }

    #[test]
    fn single_and_odd_layouts() {
        // this desktop: RX 6700 XT + UHD 770
        let amd = AdapterDesc { vendor: 0x1002, device: 0x73df, kind: T::DiscreteGpu };
        let uhd = AdapterDesc { vendor: 0x8086, device: 0x4680, kind: T::IntegratedGpu };
        assert_eq!(pick_adapter(GpuPref::Auto, &[uhd, amd], NoOverride), Some(1));
        assert_eq!(pick_adapter(GpuPref::PowerSaving, &[uhd, amd], NoOverride), Some(0));
        // only one GPU: every preference takes it
        for p in [GpuPref::Auto, GpuPref::PowerSaving, GpuPref::Performance, GpuPref::Pci(1, 2)] {
            assert_eq!(pick_adapter(p, &[INTEL_ARC], NoOverride), Some(0));
            assert_eq!(pick_adapter(p, &[LLVMPIPE, RTX_4050], DriverFirst), Some(1));
            assert_eq!(pick_adapter(p, &[LLVMPIPE, INTEL_ARC], Nvidia), Some(1), "prime-run without NVIDIA");
            assert_eq!(pick_adapter(p, &[LLVMPIPE], NoOverride), Some(0));
            assert_eq!(pick_adapter(p, &[], NoOverride), None);
        }
        let other = AdapterDesc { vendor: 7, device: 7, kind: T::Other };
        assert_eq!(pick_adapter(GpuPref::PowerSaving, &[LLVMPIPE, other], NoOverride), Some(1));
        // two identical cards: the first
        assert_eq!(pick_adapter(GpuPref::Pci(0x10de, 0x28a1), &[RTX_4050, RTX_4050], NoOverride), Some(0));
    }

    #[test]
    fn driver_overrides_are_read_from_the_environment() {
        let env = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
        assert_eq!(driver_override_from(env(&[])), NoOverride);
        assert_eq!(driver_override_from(env(&[("DRI_PRIME", ""), ("MESA_VK_DEVICE_SELECT", "  ")])), NoOverride);
        assert_eq!(driver_override_from(env(&[("DRI_PRIME", "1")])), DriverFirst);
        assert_eq!(driver_override_from(env(&[("__NV_PRIME_RENDER_OFFLOAD", "1")])), Nvidia);
        assert_eq!(driver_override_from(env(&[("__NV_PRIME_RENDER_OFFLOAD", "0")])), NoOverride);
        assert_eq!(driver_override_from(env(&[("__NV_PRIME_RENDER_OFFLOAD", "1"), ("DRI_PRIME", "0")])), Nvidia);
        assert_eq!(driver_override_from(env(&[("MESA_VK_DEVICE_SELECT", "8086:7d55")])), DriverFirst);
    }

    #[test]
    fn the_process_preference_is_shared() {
        set_preference(GpuPref::PowerSaving);
        assert_eq!(preference(), GpuPref::PowerSaving);
        set_preference(GpuPref::Auto);
        assert_eq!(preference(), GpuPref::Auto);
    }

    /// Fork §1a.105 (b): the loader is limited to Intel's driver only where the app would draw on
    /// the Intel GPU anyway, never with an override, a missing Intel driver, or on other machines.
    #[test]
    fn the_loader_skips_nvidia_only_when_the_intel_gpu_is_used() {
        let xps = [INTEL, NVIDIA];
        let icds = vec!["intel_icd.x86_64.json".to_string(), "nvidia_icd.json".to_string()];
        let pick = |pref, ov, bat, vendors: &[u32], icds: &[String], set| igpu_only_drivers(pref, ov, bat, vendors, icds, set);
        assert_eq!(pick(GpuPref::PowerSaving, DriverOverride::None, true, &xps, &icds, false), Some("*intel*"));
        assert_eq!(pick(GpuPref::Auto, DriverOverride::None, true, &xps, &icds, false), Some("*intel*"), "Auto on the laptop");
        assert_eq!(pick(GpuPref::Pci(INTEL, 0x7d55), DriverOverride::None, true, &xps, &icds, false), Some("*intel*"));
        assert_eq!(pick(GpuPref::Performance, DriverOverride::None, true, &xps, &icds, false), None);
        assert_eq!(pick(GpuPref::Pci(NVIDIA, 0x28a1), DriverOverride::None, true, &xps, &icds, false), None);
        assert_eq!(pick(GpuPref::Auto, DriverOverride::None, false, &xps, &icds, false), None, "Auto on a desktop: the discrete GPU");
        assert_eq!(pick(GpuPref::PowerSaving, DriverOverride::Nvidia, true, &xps, &icds, false), None, "prime-run");
        assert_eq!(pick(GpuPref::PowerSaving, DriverOverride::DriverFirst, true, &xps, &icds, false), None, "DRI_PRIME");
        assert_eq!(pick(GpuPref::PowerSaving, DriverOverride::None, true, &xps, &icds, true), None, "already set (no loop)");
        assert_eq!(pick(GpuPref::PowerSaving, DriverOverride::None, true, &xps, &["nvidia_icd.json".to_string()], false), None, "no Intel driver");
        assert_eq!(pick(GpuPref::PowerSaving, DriverOverride::None, true, &[INTEL], &icds, false), None, "no NVIDIA GPU");
        assert_eq!(pick(GpuPref::PowerSaving, DriverOverride::None, true, &[0x1002, NVIDIA], &icds, false), None, "AMD + NVIDIA");
    }
}
