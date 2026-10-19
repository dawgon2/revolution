//! Whether this computer runs on battery (fork XPS d): read from `/sys/class/power_supply`, the
//! same kernel attributes UPower reports, without a D-Bus round trip on the UI thread. A desktop
//! (no battery) is never "on battery"; anything unreadable counts as mains power.

use std::sync::Mutex;
use std::time::Duration;
use web_time::Instant;

/// One power supply as the kernel lists it: its `type`, `online` and `status`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Supply {
    pub kind: String,
    pub online: Option<bool>,
    pub status: String,
    /// `Device` for a peripheral's own battery (a wireless mouse or headset), else empty /
    /// `System`.
    pub scope: String,
}

/// On battery: a battery of the computer itself (not a mouse's or headset's: `scope` `Device`)
/// is discharging, and no mains / USB-C supply is online.
pub fn on_battery_from(supplies: &[Supply]) -> bool {
    let own = |s: &&Supply| s.scope != "Device";
    let external = supplies.iter().filter(own).any(|s| matches!(s.kind.as_str(), "Mains" | "USB") && s.online == Some(true));
    let discharging = supplies.iter().filter(own).any(|s| s.kind == "Battery" && s.status == "Discharging");
    discharging && !external
}

/// Whether the computer has a battery of its own (a laptop), charging or not; a wireless mouse's or
/// headset's (`scope` `Device`) doesn't count (fork XPS item 17: Auto's GPU choice).
pub fn has_battery_from(supplies: &[Supply]) -> bool {
    supplies.iter().any(|s| s.kind == "Battery" && s.scope != "Device")
}

/// [`has_battery_from`] on this computer (never in the browser).
pub fn has_battery() -> bool {
    !cfg!(target_arch = "wasm32") && has_battery_from(&supplies())
}

/// The supplies under `/sys/class/power_supply` (at most 16; none elsewhere).
pub fn supplies() -> Vec<Supply> {
    let read = |p: &std::path::Path, f: &str| std::fs::read_to_string(p.join(f)).map(|v| v.trim().chars().take(32).collect::<String>()).unwrap_or_default();
    std::fs::read_dir("/sys/class/power_supply")
        .map(|d| {
            d.flatten()
                .take(16)
                .map(|e| {
                    let p = e.path();
                    let online = match read(&p, "online").as_str() {
                        "1" => Some(true),
                        "0" => Some(false),
                        _ => None,
                    };
                    Supply { kind: read(&p, "type"), online, status: read(&p, "status"), scope: read(&p, "scope") }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// [`on_battery_from`] on this computer, read at most every 10 seconds (cheap enough to ask
/// every frame).
pub fn on_battery() -> bool {
    // (no clock or sysfs in the browser)
    if cfg!(target_arch = "wasm32") {
        return false;
    }
    static LAST: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match *last {
        Some((at, v)) if at.elapsed() < Duration::from_secs(10) => v,
        _ => {
            let v = on_battery_from(&supplies());
            *last = Some((Instant::now(), v));
            v
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(kind: &str, online: Option<bool>, status: &str) -> Supply {
        Supply { kind: kind.into(), online, status: status.into(), scope: String::new() }
    }

    #[test]
    fn battery_and_mains_states() {
        // the XPS unplugged / on its charger / on a USB-C charger; a desktop; a full battery on AC
        assert!(on_battery_from(&[s("Mains", Some(false), ""), s("Battery", None, "Discharging")]));
        assert!(!on_battery_from(&[s("Mains", Some(true), ""), s("Battery", None, "Charging")]));
        assert!(!on_battery_from(&[s("USB", Some(true), ""), s("Battery", None, "Discharging")]));
        assert!(!on_battery_from(&[]));
        assert!(!on_battery_from(&[s("Mains", Some(true), ""), s("Battery", None, "Full")]));
        // unreadable values count as mains power
        assert!(!on_battery_from(&[s("Battery", None, "Unknown")]));
        assert!(!on_battery_from(&[s("", None, "")]));
        // a desktop with a wireless mouse whose own battery runs down (Logitech hidpp_battery)
        let mouse = Supply { scope: "Device".into(), ..s("Battery", Some(false), "Discharging") };
        assert!(!on_battery_from(std::slice::from_ref(&mouse)));
        assert!(on_battery_from(&[mouse, s("Mains", Some(false), ""), s("Battery", None, "Discharging")]));
    }

    /// XPS 17: a laptop has a battery whether charging, full or discharging; a desktop with a
    /// wireless mouse or headset battery does not.
    #[test]
    fn a_battery_of_its_own_makes_a_laptop() {
        assert!(has_battery_from(&[s("Mains", Some(true), ""), s("Battery", None, "Charging")]));
        assert!(has_battery_from(&[s("Battery", None, "Full")]));
        assert!(!has_battery_from(&[s("Mains", Some(true), "")]));
        assert!(!has_battery_from(&[]));
        let mouse = Supply { scope: "Device".into(), ..s("Battery", Some(false), "Discharging") };
        assert!(!has_battery_from(&[mouse, s("Mains", Some(true), "")]));
    }

    #[test]
    fn this_computer_answers_without_panicking() {
        let _ = on_battery();
        let _ = on_battery();
    }
}
