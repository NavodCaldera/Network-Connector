//! Today's helping summary, kept in nc-bridge until nc-agent reports it (see the hand-back Requests).
//!
//! Counts wall-clock time between a successful start and Stop. Resets at local midnight in the policy time
//! zone. In memory only: it is a summary for the owner, not an accounting record.

use crate::api::TodaySummary;
use chrono::{NaiveDate, Utc};
use chrono_tz::Tz;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

struct Today {
    tz: Tz,
    date: Option<NaiveDate>,
    seconds: u64,
    running_since: Option<Instant>,
}

static TODAY: Mutex<Today> = Mutex::new(Today {
    tz: chrono_tz::Asia::Colombo,
    date: None,
    seconds: 0,
    running_since: None,
});

impl Today {
    fn roll(&mut self, today: NaiveDate) {
        if self.date != Some(today) {
            self.date = Some(today);
            self.seconds = 0;
            if self.running_since.is_some() {
                self.running_since = Some(Instant::now());
            }
        }
    }
    fn local_date(&self) -> NaiveDate {
        Utc::now().with_timezone(&self.tz).date_naive()
    }
    fn bank(&mut self) {
        if let Some(t) = self.running_since.take() {
            self.seconds = self.seconds.saturating_add(t.elapsed().as_secs());
        }
    }
}

fn with<R>(f: impl FnOnce(&mut Today) -> R) -> R {
    let mut t = TODAY.lock().unwrap_or_else(PoisonError::into_inner);
    let d = t.local_date();
    t.roll(d);
    f(&mut t)
}

pub(crate) fn session_started(tz: &str) {
    with(|t| {
        if let Ok(tz) = tz.parse() {
            t.tz = tz;
        }
        t.running_since.get_or_insert_with(Instant::now);
    });
}

pub(crate) fn session_stopped() {
    with(Today::bank);
}

pub(crate) fn summary() -> TodaySummary {
    with(|t| TodaySummary {
        helping_seconds: t
            .seconds
            .saturating_add(t.running_since.map_or(0, |s| s.elapsed().as_secs())),
        jobs_done: 0,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn date_change_resets_the_count() {
        let mut t = Today {
            tz: chrono_tz::UTC,
            date: NaiveDate::from_ymd_opt(2026, 1, 5),
            seconds: 100,
            running_since: None,
        };
        t.roll(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap());
        assert_eq!(t.seconds, 100);
        t.roll(NaiveDate::from_ymd_opt(2026, 1, 6).unwrap());
        assert_eq!(t.seconds, 0);
    }

    #[test]
    fn running_time_is_banked_on_stop() {
        let mut t = Today {
            tz: chrono_tz::UTC,
            date: None,
            seconds: 7,
            running_since: Some(Instant::now()),
        };
        t.bank();
        assert!(t.running_since.is_none());
        assert!(t.seconds >= 7);
    }
}
