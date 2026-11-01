//! Platform observations the policy and tariff engines need, behind a trait so tests can fake them.
//!
//! Unknown readings are reported as unknown (`None`); nc-policy then refuses work (fail closed). Nothing here
//! guesses a value the platform did not give us.

use chrono::{DateTime, Utc};
use nc_policy::ThermalLevel;
use nc_tariff::{InterfaceClass, NetworkObservation};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// One snapshot of device conditions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceObservation {
    /// External power present.
    pub on_ac_power: bool,
    /// Battery percent; `None` when unknown or there is no battery.
    pub battery_percent: Option<u8>,
    /// Thermal level; `None` when unknown (nc-policy then refuses work).
    pub thermal_level: Option<ThermalLevel>,
    /// Seconds since the last user input; `None` when unknown.
    pub user_idle_seconds: Option<u64>,
}

/// Source of device and network observations.
pub trait PlatformProbe: Send + Sync {
    /// Current device conditions. Must return quickly (no blocking I/O): Stop latency depends on it.
    fn device(&self) -> DeviceObservation;
    /// Current network. Must return quickly.
    fn network(&self) -> NetworkObservation;
}

/// Wall clock, behind a trait so window-boundary and clock-jump tests are deterministic.
pub trait Clock: Send + Sync {
    /// Now, UTC.
    fn now(&self) -> DateTime<Utc>;
}

/// The system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Test probe with settable readings.
#[derive(Debug)]
pub struct FakeProbe {
    state: Mutex<(DeviceObservation, NetworkObservation)>,
}

impl FakeProbe {
    /// A probe reporting `device` and `network`.
    pub fn new(device: DeviceObservation, network: NetworkObservation) -> Self {
        Self {
            state: Mutex::new((device, network)),
        }
    }
    /// Replace the device reading.
    pub fn set_device(&self, d: DeviceObservation) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).0 = d;
    }
    /// Replace the network reading (e.g. a Wi-Fi to cellular switch).
    pub fn set_network(&self, n: NetworkObservation) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).1 = n;
    }
}

impl PlatformProbe for FakeProbe {
    fn device(&self) -> DeviceObservation {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .clone()
    }
    fn network(&self) -> NetworkObservation {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .clone()
    }
}

/// Owner-declared facts the platform cannot report (see [`SystemProbe`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeConfig {
    /// The connection the owner confirmed (interface + opaque id). No platform adapter confirms the network
    /// yet, so this is the owner's statement; without it every transfer is refused.
    pub network: Option<DeclaredNetwork>,
    /// Thermal level the owner accepts as current (Windows exposes no unprivileged thermal reading).
    /// `None` keeps it unknown, which makes nc-policy refuse work.
    #[serde(default)]
    pub declared_thermal: Option<ThermalLevel>,
}

/// Owner-confirmed network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredNetwork {
    /// Interface class.
    pub interface: InterfaceClass,
    /// Opaque, owner-confirmed connection id (must match the tariff profile binding).
    pub connection_id: String,
    /// Conservative throughput estimate in bytes per second (0 = unknown, refuses metered transfers).
    pub bytes_per_second: u64,
}

#[derive(Clone, Debug)]
struct Power {
    on_ac: bool,
    battery: Option<u8>,
}

/// Real probe. Power and battery are read on Windows from `Win32_Battery` by a background refresher (every
/// 30 s, never on the session thread). User idle time is not available without `unsafe` FFI in this crate,
/// so it is reported unknown (requested from the nc-exec/nc-identity platform adapters). Thermal and network
/// come from [`ProbeConfig`].
pub struct SystemProbe {
    cfg: ProbeConfig,
    power: Arc<Mutex<Option<Power>>>,
    alive: Arc<AtomicBool>,
}

impl std::fmt::Debug for SystemProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemProbe").finish_non_exhaustive()
    }
}

impl SystemProbe {
    /// Start the probe (and its refresher thread on Windows).
    pub fn new(cfg: ProbeConfig) -> Self {
        let power = Arc::new(Mutex::new(None));
        let alive = Arc::new(AtomicBool::new(true));
        {
            let power = Arc::clone(&power);
            let alive = Arc::clone(&alive);
            let _ = std::thread::Builder::new()
                .name("nc-probe".into())
                .spawn(move || {
                    while alive.load(Ordering::SeqCst) {
                        let p = read_power();
                        *power.lock().unwrap_or_else(PoisonError::into_inner) = p;
                        for _ in 0..300 {
                            if !alive.load(Ordering::SeqCst) {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                });
        }
        Self { cfg, power, alive }
    }
}

impl Drop for SystemProbe {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
    }
}

impl PlatformProbe for SystemProbe {
    fn device(&self) -> DeviceObservation {
        let p = self
            .power
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        DeviceObservation {
            // Unknown power is reported as "not on AC" so charging-only policies refuse.
            on_ac_power: p.as_ref().is_some_and(|p| p.on_ac),
            battery_percent: p.and_then(|p| p.battery),
            thermal_level: self.cfg.declared_thermal,
            user_idle_seconds: None,
        }
    }
    fn network(&self) -> NetworkObservation {
        match &self.cfg.network {
            Some(n) => NetworkObservation {
                interface: n.interface,
                connection_id: Some(n.connection_id.clone()),
                bytes_per_second: n.bytes_per_second,
            },
            None => NetworkObservation {
                interface: InterfaceClass::Cellular,
                connection_id: None,
                bytes_per_second: 0,
            },
        }
    }
}

/// Parse `Get-CimInstance Win32_Battery | ConvertTo-Json` output. Empty output means no battery (a desktop on
/// mains power). BatteryStatus 1, 4 and 5 are the discharging states.
fn parse_power(text: &str) -> Option<Power> {
    let text = text.trim();
    if text.is_empty() {
        return Some(Power {
            on_ac: true,
            battery: None,
        });
    }
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let v = if let Some(a) = v.as_array() {
        a.first()?.clone()
    } else {
        v
    };
    let status = v.get("BatteryStatus")?.as_u64()?;
    let pct = v
        .get("EstimatedChargeRemaining")
        .and_then(serde_json::Value::as_u64)
        .and_then(|p| u8::try_from(p).ok())
        .filter(|p| *p <= 100);
    Some(Power {
        on_ac: !matches!(status, 1 | 4 | 5),
        battery: pct,
    })
}

#[cfg(windows)]
fn read_power() -> Option<Power> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance -ClassName Win32_Battery | Select-Object BatteryStatus,EstimatedChargeRemaining | ConvertTo-Json -Compress",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_power(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(windows))]
fn read_power() -> Option<Power> {
    // Linux: /sys/class/power_supply. No battery directory means mains power.
    let dir = std::fs::read_dir("/sys/class/power_supply").ok()?;
    let mut battery = None;
    let mut on_ac = None;
    for e in dir.flatten() {
        let p = e.path();
        let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
        match kind.trim() {
            "Mains" => {
                let online = std::fs::read_to_string(p.join("online")).unwrap_or_default();
                on_ac = Some(online.trim() == "1");
            }
            "Battery" => {
                battery = std::fs::read_to_string(p.join("capacity"))
                    .ok()
                    .and_then(|c| c.trim().parse::<u8>().ok())
                    .filter(|c| *c <= 100);
            }
            _ => {}
        }
    }
    Some(Power {
        on_ac: on_ac.unwrap_or(battery.is_none()),
        battery,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_battery_states() {
        let p = parse_power("").unwrap();
        assert!(p.on_ac && p.battery.is_none());
        let p = parse_power(r#"{"BatteryStatus":1,"EstimatedChargeRemaining":55}"#).unwrap();
        assert!(!p.on_ac);
        assert_eq!(p.battery, Some(55));
        let p = parse_power(r#"[{"BatteryStatus":2,"EstimatedChargeRemaining":101}]"#).unwrap();
        assert!(p.on_ac);
        assert_eq!(p.battery, None, "out-of-range percent is unknown");
        assert!(parse_power("garbage").is_none());
    }
}
