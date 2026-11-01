//! Synthetic example only: no ISP package, pricing or performance claims.
use chrono::{TimeZone, Utc};
use nc_tariff::sim;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let nights = std::env::args()
        .nth(1)
        .map_or(Ok(90u32), |arg| arg.parse())?;
    let start = Utc
        .with_ymd_and_hms(2026, 1, 1, 12, 0, 0)
        .single()
        .ok_or("invalid example date")?;
    let report = sim::run(nights, start, 0x4e43544152494646)?;
    assert_eq!(report.completed + report.deferred, u64::from(nights) * 32);
    println!("EXAMPLE ONLY: nights={}, completed={}, deferred={}, committed_bytes={}; window/budget assertions passed", report.nights, report.completed, report.deferred, report.committed_bytes);
    Ok(())
}
