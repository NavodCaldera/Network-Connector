//! Owner settings: conversion to nc-policy types, validation and the one settings file.
//!
//! File: `<app support dir>/settings.json`, versioned (`format`), written atomically (temp file, flush,
//! rename). A file that cannot be read, parsed or validated is moved to `settings.json.bad` and safe
//! defaults are used. The file holds no secrets: limits and night-data windows only.

use crate::api::{AppSettings, LoadedSettings, NightDataSettings, PolicySummary, TimeWindow};
use crate::night;
use chrono::{NaiveTime, Timelike, Weekday};
use chrono_tz::Tz;
use nc_policy::{OwnerPolicy, WorkingWindow};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Current file format. Bump and add a migration when the layout changes.
pub(crate) const FORMAT: u32 = 1;
/// Settings files are small; anything larger is not ours.
const MAX_FILE_BYTES: u64 = 256 * 1024;
pub(crate) const FILE_NAME: &str = "settings.json";
pub(crate) const BAD_NAME: &str = "settings.json.bad";
const TMP_NAME: &str = "settings.json.tmp";
/// Sri Lanka first (PLAN.md §6). The owner can change it.
pub(crate) const DEFAULT_TZ: &str = "Asia/Colombo";

const APP_DIR: &str = "NetworkConnector";

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Windows `%APPDATA%\NetworkConnector`, macOS `~/Library/Application Support/NetworkConnector`,
/// Linux `$XDG_DATA_HOME/NetworkConnector` or `~/.local/share/NetworkConnector`.
pub(crate) fn app_support_dir() -> Result<PathBuf, String> {
    let base = if cfg!(windows) {
        env_dir("APPDATA")
    } else if cfg!(target_os = "macos") {
        env_dir("HOME").map(|h| h.join("Library").join("Application Support"))
    } else if cfg!(target_os = "linux") {
        env_dir("XDG_DATA_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".local").join("share")))
    } else {
        None
    };
    base.map(|b| b.join(APP_DIR))
        .ok_or_else(|| "No folder to save settings on this device yet.".to_string())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsFile {
    format: u32,
    policy: OwnerPolicy,
    night_data: NightDataFile,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NightDataFile {
    preset_id: String,
    days: Vec<u8>,
    start_minute: u16,
    end_minute: u16,
    nightly_budget_mb: u64,
    never_paid_data: bool,
    timezone: String,
}

pub(crate) fn default_settings() -> AppSettings {
    let policy = OwnerPolicy {
        timezone: chrono_tz::Asia::Colombo,
        ..OwnerPolicy::default()
    };
    AppSettings {
        policy: from_policy(&policy),
        night_data: night::default_night_data(),
    }
}

pub(crate) fn parse_tz(name: &str) -> Result<Tz, String> {
    name.parse::<Tz>()
        .map_err(|_| "Unknown time zone. Use a name like Asia/Colombo.".to_string())
}

pub(crate) fn weekday(d: u8) -> Result<Weekday, String> {
    Weekday::try_from(d).map_err(|_| "Days must be Monday to Sunday.".to_string())
}

pub(crate) fn time(minute: u16) -> Result<NaiveTime, String> {
    let bad = || "Times must be between 00:00 and 23:59.".to_string();
    if minute >= 24 * 60 {
        return Err(bad());
    }
    NaiveTime::from_hms_opt(u32::from(minute / 60), u32::from(minute % 60), 0).ok_or_else(bad)
}

pub(crate) fn minute_of(t: NaiveTime) -> u16 {
    // < 1440, always fits.
    u16::try_from(t.num_seconds_from_midnight() / 60).unwrap_or(0)
}

pub(crate) fn day_number(d: Weekday) -> u8 {
    // 0..=6, always fits.
    u8::try_from(d.num_days_from_monday()).unwrap_or(0)
}

/// Builds and validates the nc-policy `OwnerPolicy`. Errors are nc-policy's own sentences.
pub(crate) fn to_policy(s: &PolicySummary) -> Result<OwnerPolicy, String> {
    let mut windows = Vec::with_capacity(s.hours.len());
    for w in &s.hours {
        windows.push(WorkingWindow {
            days: w
                .days
                .iter()
                .map(|d| weekday(*d))
                .collect::<Result<_, _>>()?,
            start: time(w.start_minute)?,
            end: time(w.end_minute)?,
        });
    }
    let policy = OwnerPolicy {
        accept_jobs: s.accept_jobs,
        cpu_budget_percent: s.cpu_budget_percent,
        max_memory_mb: s.max_memory_mb,
        max_disk_mb: s.max_disk_mb,
        charging_only: s.charging_only,
        min_battery_percent: s.min_battery_percent,
        max_task_duration: Duration::from_secs(s.max_task_minutes.saturating_mul(60)),
        pause_on_user_activity: s.pause_on_user_activity,
        idle_minutes: s.idle_minutes,
        timezone: parse_tz(&s.timezone)?,
        time_windows: windows,
        ..OwnerPolicy::default()
    };
    policy.validate().map_err(|e| e.to_string())?;
    Ok(policy)
}

pub(crate) fn from_policy(p: &OwnerPolicy) -> PolicySummary {
    PolicySummary {
        accept_jobs: p.accept_jobs,
        cpu_budget_percent: p.cpu_budget_percent,
        max_memory_mb: p.max_memory_mb,
        max_disk_mb: p.max_disk_mb,
        charging_only: p.charging_only,
        min_battery_percent: p.min_battery_percent,
        max_task_minutes: p.max_task_duration.as_secs() / 60,
        pause_on_user_activity: p.pause_on_user_activity,
        idle_minutes: p.idle_minutes,
        timezone: p.timezone.name().to_string(),
        hours: p
            .time_windows
            .iter()
            .map(|w| TimeWindow {
                days: w.days.iter().map(|d| day_number(*d)).collect(),
                start_minute: minute_of(w.start),
                end_minute: minute_of(w.end),
            })
            .collect(),
    }
}

fn to_file(s: &AppSettings) -> Result<SettingsFile, String> {
    night::to_profile(&s.night_data)?;
    let n = &s.night_data;
    Ok(SettingsFile {
        format: FORMAT,
        policy: to_policy(&s.policy)?,
        night_data: NightDataFile {
            preset_id: n.preset_id.clone(),
            days: n.window.days.clone(),
            start_minute: n.window.start_minute,
            end_minute: n.window.end_minute,
            nightly_budget_mb: n.nightly_budget_mb,
            never_paid_data: n.never_paid_data,
            timezone: n.timezone.clone(),
        },
    })
}

fn from_file(f: SettingsFile) -> Result<AppSettings, String> {
    if f.format != FORMAT {
        return Err(format!("unsupported format {}", f.format));
    }
    f.policy.validate().map_err(|e| e.to_string())?;
    let n = f.night_data;
    let night_data = NightDataSettings {
        preset_id: n.preset_id,
        window: TimeWindow {
            days: n.days,
            start_minute: n.start_minute,
            end_minute: n.end_minute,
        },
        nightly_budget_mb: n.nightly_budget_mb,
        never_paid_data: n.never_paid_data,
        timezone: n.timezone,
    };
    night::to_profile(&night_data)?;
    Ok(AppSettings {
        policy: from_policy(&f.policy),
        night_data,
    })
}

fn read(path: &Path) -> Result<Option<AppSettings>, String> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.kind().to_string()),
    };
    if meta.len() > MAX_FILE_BYTES {
        return Err("file too large".into());
    }
    let bytes = fs::read(path).map_err(|e| e.kind().to_string())?;
    let file: SettingsFile = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    from_file(file).map(Some)
}

/// Loads settings; never fails. See the module docs.
pub(crate) fn load(dir: &Path) -> LoadedSettings {
    let path = dir.join(FILE_NAME);
    match read(&path) {
        Ok(Some(settings)) => LoadedSettings {
            settings,
            reset_notice: None,
        },
        Ok(None) => LoadedSettings {
            settings: default_settings(),
            reset_notice: None,
        },
        Err(_why) => {
            // The reason is not logged: parser messages can echo file contents (AGENT.md §1 rule 10).
            let kept = fs::rename(&path, dir.join(BAD_NAME)).is_ok();
            LoadedSettings {
                settings: default_settings(),
                reset_notice: Some(if kept {
                    "Your saved settings could not be read, so safe defaults are back on. The old file was kept as settings.json.bad.".into()
                } else {
                    "Your saved settings could not be read, so safe defaults are back on.".into()
                }),
            }
        }
    }
}

/// Validates, then writes atomically. Errors are plain English.
pub(crate) fn save(dir: &Path, s: &AppSettings) -> Result<(), String> {
    let file = to_file(s)?;
    let json =
        serde_json::to_vec_pretty(&file).map_err(|_| "Could not save settings.".to_string())?;
    write_atomic(dir, &json).map_err(|_| "Could not save settings.".to_string())
}

fn write_atomic(dir: &Path, bytes: &[u8]) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let tmp: PathBuf = dir.join(TMP_NAME);
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    // On Windows std::fs::rename replaces the target (MoveFileExW + MOVEFILE_REPLACE_EXISTING).
    let result = fs::rename(&tmp, dir.join(FILE_NAME));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn missing_file_gives_safe_defaults() {
        let d = dir();
        let l = load(d.path());
        assert_eq!(l.settings, default_settings());
        assert!(l.reset_notice.is_none());
        assert!(!l.settings.policy.accept_jobs);
        assert!(l.settings.policy.hours.is_empty());
        assert!(l.settings.night_data.never_paid_data);
    }

    #[test]
    fn save_then_load_round_trips() {
        let d = dir();
        let mut s = default_settings();
        s.policy.cpu_budget_percent = 60;
        s.policy.accept_jobs = true;
        s.policy.hours = vec![TimeWindow {
            days: vec![0, 1, 2, 3, 4],
            start_minute: 22 * 60,
            end_minute: 6 * 60,
        }];
        s.night_data.nightly_budget_mb = 1234;
        save(d.path(), &s).unwrap();
        assert!(!d.path().join(TMP_NAME).exists(), "temp file left behind");
        let l = load(d.path());
        assert_eq!(l.settings, s);
        assert!(l.reset_notice.is_none());
    }

    #[test]
    fn save_replaces_existing_file_atomically() {
        let d = dir();
        let mut s = default_settings();
        save(d.path(), &s).unwrap();
        s.policy.cpu_budget_percent = 40;
        // A stale temp file from a crash must not block the next save.
        fs::write(d.path().join(TMP_NAME), b"half written").unwrap();
        save(d.path(), &s).unwrap();
        assert_eq!(load(d.path()).settings.policy.cpu_budget_percent, 40);
        assert!(!d.path().join(TMP_NAME).exists());
    }

    #[test]
    fn invalid_settings_are_not_saved() {
        let d = dir();
        let mut s = default_settings();
        s.policy.max_memory_mb = 0;
        assert_eq!(
            save(d.path(), &s).unwrap_err(),
            "Memory limit must be greater than zero."
        );
        assert!(!d.path().join(FILE_NAME).exists());
    }

    #[test]
    fn corrupt_file_gives_defaults_and_is_kept_as_bad() {
        let d = dir();
        fs::write(d.path().join(FILE_NAME), b"{ not json").unwrap();
        let l = load(d.path());
        assert_eq!(l.settings, default_settings());
        assert!(l.reset_notice.unwrap().contains("settings.json.bad"));
        assert_eq!(
            fs::read(d.path().join(BAD_NAME)).unwrap(),
            b"{ not json".to_vec()
        );
        assert!(!d.path().join(FILE_NAME).exists());
        // Next start is clean.
        assert!(load(d.path()).reset_notice.is_none());
    }

    #[test]
    fn parseable_but_invalid_file_is_treated_as_corrupt() {
        let d = dir();
        save(d.path(), &default_settings()).unwrap();
        let text = fs::read_to_string(d.path().join(FILE_NAME)).unwrap();
        let broken = text.replace("\"cpu_budget_percent\": 25", "\"cpu_budget_percent\": 0");
        assert_ne!(text, broken);
        fs::write(d.path().join(FILE_NAME), broken).unwrap();
        let l = load(d.path());
        assert!(l.reset_notice.is_some());
        assert_eq!(l.settings, default_settings());
        assert!(d.path().join(BAD_NAME).exists());
    }

    #[test]
    fn unknown_format_and_fields_are_refused() {
        let d = dir();
        save(d.path(), &default_settings()).unwrap();
        let text = fs::read_to_string(d.path().join(FILE_NAME)).unwrap();
        fs::write(
            d.path().join(FILE_NAME),
            text.replace("\"format\": 1", "\"format\": 99"),
        )
        .unwrap();
        assert!(load(d.path()).reset_notice.is_some());

        fs::write(
            d.path().join(FILE_NAME),
            text.replace("\"format\": 1", "\"format\": 1, \"token\": \"x\""),
        )
        .unwrap();
        assert!(load(d.path()).reset_notice.is_some());
    }

    #[test]
    fn oversized_file_is_refused() {
        let d = dir();
        fs::write(
            d.path().join(FILE_NAME),
            vec![b' '; (MAX_FILE_BYTES + 1) as usize],
        )
        .unwrap();
        assert!(load(d.path()).reset_notice.is_some());
    }

    #[test]
    fn file_holds_only_limits_and_windows() {
        let d = dir();
        save(d.path(), &default_settings()).unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&fs::read(d.path().join(FILE_NAME)).unwrap()).unwrap();
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["format", "night_data", "policy"]);
    }

    #[test]
    fn validation_messages_come_from_nc_policy() {
        let mut s = default_settings().policy;
        s.cpu_budget_percent = 101;
        assert_eq!(
            to_policy(&s).unwrap_err(),
            "CPU budget must be between 1 and 100 percent."
        );
        let mut s = default_settings().policy;
        s.idle_minutes = 0;
        assert_eq!(
            to_policy(&s).unwrap_err(),
            "Idle minutes must be greater than zero when pausing for user activity."
        );
        let mut s = default_settings().policy;
        s.hours = vec![TimeWindow {
            days: vec![0],
            start_minute: 600,
            end_minute: 600,
        }];
        assert!(to_policy(&s)
            .unwrap_err()
            .starts_with("Working hours are invalid"));
        let mut s = default_settings().policy;
        s.hours = vec![TimeWindow {
            days: vec![9],
            start_minute: 0,
            end_minute: 60,
        }];
        assert_eq!(to_policy(&s).unwrap_err(), "Days must be Monday to Sunday.");
        let mut s = default_settings().policy;
        s.timezone = "Mars/Base".into();
        assert!(to_policy(&s).unwrap_err().starts_with("Unknown time zone"));
    }
}
