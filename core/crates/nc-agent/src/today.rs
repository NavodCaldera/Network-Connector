//! Today's counters for the owner (app Home screen): jobs done and time helped. In memory; they reset at
//! local midnight in the policy time zone. A summary for the owner, not an accounting record.

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use std::time::Instant;

/// Today's numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Today {
    /// Jobs committed today (each attempt counted once, even if Complete was resent).
    pub jobs_done: u64,
    /// Seconds a helper session was running today.
    pub helping_seconds: u64,
    /// Seconds spent executing jobs today.
    pub job_seconds: u64,
}

#[derive(Debug)]
pub(crate) struct Counters {
    tz: Tz,
    date: Option<NaiveDate>,
    today: Today,
    session_since: Option<Instant>,
}

impl Counters {
    pub(crate) fn new() -> Self {
        Self {
            tz: chrono_tz::UTC,
            date: None,
            today: Today::default(),
            session_since: None,
        }
    }

    fn roll(&mut self, now: DateTime<Utc>) {
        let d = now.with_timezone(&self.tz).date_naive();
        if self.date != Some(d) {
            self.date = Some(d);
            self.today = Today::default();
            if self.session_since.is_some() {
                self.session_since = Some(Instant::now());
            }
        }
    }

    pub(crate) fn session_started(&mut self, tz: Tz, now: DateTime<Utc>) {
        self.tz = tz;
        self.roll(now);
        self.session_since.get_or_insert_with(Instant::now);
    }

    pub(crate) fn session_stopped(&mut self, now: DateTime<Utc>) {
        self.roll(now);
        if let Some(t) = self.session_since.take() {
            self.today.helping_seconds = self
                .today
                .helping_seconds
                .saturating_add(t.elapsed().as_secs());
        }
    }

    pub(crate) fn job_committed(&mut self, now: DateTime<Utc>) {
        self.roll(now);
        self.today.jobs_done += 1;
    }

    pub(crate) fn job_ran(&mut self, now: DateTime<Utc>, seconds: u64) {
        self.roll(now);
        self.today.job_seconds = self.today.job_seconds.saturating_add(seconds);
    }

    pub(crate) fn snapshot(&mut self, now: DateTime<Utc>) -> Today {
        self.roll(now);
        let mut t = self.today;
        if let Some(s) = self.session_since {
            t.helping_seconds = t.helping_seconds.saturating_add(s.elapsed().as_secs());
        }
        t
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn resets_at_local_midnight() {
        let mut c = Counters::new();
        let tz: Tz = "Asia/Colombo".parse().unwrap();
        // 23:00 Colombo = 17:30 UTC.
        let late = Utc.with_ymd_and_hms(2026, 11, 2, 17, 30, 0).unwrap();
        c.session_started(tz, late);
        c.job_committed(late);
        assert_eq!(c.snapshot(late).jobs_done, 1);
        // 00:30 Colombo next day = 19:00 UTC.
        let next = Utc.with_ymd_and_hms(2026, 11, 2, 19, 0, 0).unwrap();
        assert_eq!(c.snapshot(next).jobs_done, 0);
    }
}
