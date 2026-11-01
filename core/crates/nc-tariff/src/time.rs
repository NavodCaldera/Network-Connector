use crate::{TariffError, TariffProfile};
use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Slot {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub index: usize,
}

// Ambiguous starts move later and ends earlier; nonexistent endpoints skip the slot.
fn endpoint(profile: &TariffProfile, local: NaiveDateTime, start: bool) -> Option<DateTime<Utc>> {
    match profile.timezone.from_local_datetime(&local) {
        LocalResult::Single(t) => Some(t.with_timezone(&Utc)),
        LocalResult::Ambiguous(a, b) => {
            Some(if start { a.max(b) } else { a.min(b) }.with_timezone(&Utc))
        }
        LocalResult::None => None,
    }
}

pub(crate) fn slots(
    profile: &TariffProfile,
    now: DateTime<Utc>,
    horizon: DateTime<Utc>,
) -> Result<Vec<Slot>, TariffError> {
    if horizon < now || horizon.signed_duration_since(now) > Duration::days(3660) {
        return Err(TariffError::Range);
    }
    let mut date = now
        .with_timezone(&profile.timezone)
        .date_naive()
        .pred_opt()
        .ok_or(TariffError::Range)?;
    let last = horizon.with_timezone(&profile.timezone).date_naive();
    let mut result = Vec::new();
    while date <= last {
        for (index, w) in profile.windows.iter().enumerate() {
            if !w.days.contains(&date.weekday()) {
                continue;
            }
            let end_date = if w.end < w.start {
                date.succ_opt().ok_or(TariffError::Range)?
            } else {
                date
            };
            if let (Some(start), Some(end)) = (
                endpoint(profile, date.and_time(w.start), true),
                endpoint(profile, end_date.and_time(w.end), false),
            ) {
                let end = end
                    .checked_sub_signed(Duration::seconds(i64::from(
                        profile.policy.boundary_guard_seconds,
                    )))
                    .ok_or(TariffError::Range)?;
                if start < end && end > now && start <= horizon {
                    result.push(Slot { start, end, index });
                }
            }
        }
        date = date.succ_opt().ok_or(TariffError::Range)?;
    }
    result.sort();
    Ok(result)
}

fn month_boundary(
    profile: &TariffProfile,
    year: i32,
    month: u32,
) -> Result<DateTime<Utc>, TariffError> {
    let (ny, nm) = if month == 12 {
        (year.checked_add(1).ok_or(TariffError::Range)?, 1)
    } else {
        (year, month + 1)
    };
    let last = NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|d| d.pred_opt())
        .ok_or(TariffError::Range)?;
    let date = NaiveDate::from_ymd_opt(
        year,
        month,
        profile.billing.reset_rule.day_of_month.min(last.day()),
    )
    .ok_or(TariffError::Range)?;
    let local = date.and_hms_opt(0, 0, 0).ok_or(TariffError::Range)?;
    // Midnight DST gaps: use the first valid minute, at most a day later.
    for minute in 0..=1440 {
        let candidate = local
            .checked_add_signed(Duration::minutes(minute))
            .ok_or(TariffError::Range)?;
        if let Some(t) = endpoint(profile, candidate, true) {
            return Ok(t);
        }
    }
    Err(TariffError::Range)
}

pub(crate) fn period(
    profile: &TariffProfile,
    now: DateTime<Utc>,
) -> Result<(DateTime<Utc>, DateTime<Utc>), TariffError> {
    let local = now.with_timezone(&profile.timezone);
    let mut year = local.year();
    let mut month = local.month();
    let current = month_boundary(profile, year, month)?;
    if now < current {
        if month == 1 {
            year = year.checked_sub(1).ok_or(TariffError::Range)?;
            month = 12;
        } else {
            month -= 1;
        }
    }
    let start = month_boundary(profile, year, month)?;
    let (ny, nm) = if month == 12 {
        (year.checked_add(1).ok_or(TariffError::Range)?, 1)
    } else {
        (year, month + 1)
    };
    Ok((start, month_boundary(profile, ny, nm)?))
}
