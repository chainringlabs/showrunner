use jiff::tz::TimeZone;
use jiff::Timestamp;
use sqlx::SqlitePool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::datasources::tmdb::TmdbClient;

/// Minimum wall-clock gap between two manual `POST /api/v1/sync` runs.
pub const MANUAL_SYNC_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// Enforces a minimum interval between runs of an expensive operation.
///
/// `resync_all` caps how much work a single run can do; this caps how often a
/// run can be started, so the ceiling can't just be reapplied in a tight loop by
/// an unauthenticated caller. State is per-`AppState` rather than a process
/// global so tests and any future multi-instance setup stay independent.
#[derive(Debug, Default)]
pub struct RateGate {
    last_run: Mutex<Option<Instant>>,
}

impl RateGate {
    /// Record a run and return true, or return false if one happened less than
    /// `min_interval` ago. Checking and recording are one critical section, so
    /// concurrent callers can't both pass.
    // @spec RESYNC-TRIGGER-004, RESYNC-TRIGGER-005
    pub fn try_acquire(&self, min_interval: Duration) -> bool {
        let mut last = self.last_run.lock().expect("sync gate mutex poisoned");
        let now = Instant::now();
        match *last {
            Some(prev) if now.duration_since(prev) < min_interval => false,
            _ => {
                *last = Some(now);
                true
            }
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub tmdb: Arc<TmdbClient>,
    pub tz: TimeZone,
    pub sync_gate: Arc<RateGate>,
}

impl AppState {
    // @spec RESYNC-TRIGGER-005
    pub fn new(pool: SqlitePool, tmdb: TmdbClient, tz: TimeZone) -> Self {
        Self {
            pool,
            tmdb: Arc::new(tmdb),
            tz,
            sync_gate: Arc::new(RateGate::default()),
        }
    }
}

/// Today's date in the configured timezone, formatted as YYYY-MM-DD.
// @spec SHOWS-PROGRESS-001
pub fn today_in(tz: &TimeZone) -> String {
    Timestamp::now().to_zoned(tz.clone()).date().to_string()
}

/// The current instant as the one timestamp form the database stores: UTC,
/// nine fractional digits, `+00:00` offset. Lists order these columns as text,
/// so the offset must stay `+00:00` (never `Z`) to sort with existing rows.
// @spec APP-TIME-001
pub fn now_utc_rfc3339() -> String {
    format_utc_rfc3339(Timestamp::now())
}

/// `now_utc_rfc3339`'s format applied to any instant.
// @spec APP-TIME-001
pub fn format_utc_rfc3339(ts: Timestamp) -> String {
    ts.strftime("%Y-%m-%dT%H:%M:%S%.9f+00:00").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // @spec SHOWS-PROGRESS-001
    #[test]
    fn today_in_returns_iso_date() {
        let tz = TimeZone::get("America/New_York").unwrap();
        let s = today_in(&tz);
        // YYYY-MM-DD shape
        assert_eq!(s.len(), 10);
        assert_eq!(s.as_bytes()[4], b'-');
        assert_eq!(s.as_bytes()[7], b'-');
        // round-trip parses
        s.parse::<jiff::civil::Date>().unwrap();
    }

    // @spec SHOWS-PROGRESS-001
    #[test]
    fn today_in_can_differ_across_timezones() {
        // Pacific/Kiritimati (UTC+14) and Pacific/Pago_Pago (UTC-11) span 25h,
        // so for some instants their dates differ. Just verify both parse.
        let east = TimeZone::get("Pacific/Kiritimati").unwrap();
        let west = TimeZone::get("Pacific/Pago_Pago").unwrap();
        let east_day: jiff::civil::Date = today_in(&east).parse().unwrap();
        let west_day: jiff::civil::Date = today_in(&west).parse().unwrap();
        // 25 hours apart: the eastern date is never behind the western one.
        assert!(east_day >= west_day);
    }

    // @spec APP-TIME-001
    #[test]
    fn stored_timestamps_are_utc_with_nine_digits_and_a_plus_offset() {
        let ts: Timestamp = "2026-11-17T11:34:42.267791Z".parse().unwrap();
        assert_eq!(
            format_utc_rfc3339(ts),
            "2026-11-17T11:34:42.267791000+00:00"
        );
        let whole: Timestamp = "2026-11-17T11:34:42Z".parse().unwrap();
        assert_eq!(
            format_utc_rfc3339(whole),
            "2026-11-17T11:34:42.000000000+00:00"
        );

        let now = now_utc_rfc3339();
        assert!(now.ends_with("+00:00"), "{now}");
        assert_eq!(now.len(), "2026-11-17T11:34:42.267791000+00:00".len());
        // Still a valid RFC3339 instant, and close to the real clock.
        let parsed: Timestamp = now.parse().unwrap();
        assert!((Timestamp::now().as_second() - parsed.as_second()).abs() < 5);
    }

    // @spec APP-TIME-001
    #[test]
    fn the_plus_offset_form_sorts_as_text_across_fractional_precisions() {
        // Rows already stored carry 0, 3, 6, or 9 fractional digits.
        let mut rows = [
            "2026-11-17T11:34:42.5+00:00",
            "2026-11-17T11:34:42+00:00",
            "2026-11-17T11:34:42.267791000+00:00",
            "2026-11-17T11:34:42.267+00:00",
            "2026-11-17T11:34:41.999999999+00:00",
        ];
        rows.sort_unstable();
        let instants: Vec<Timestamp> = rows.iter().map(|r| r.parse().unwrap()).collect();
        assert!(instants.windows(2).all(|w| w[0] <= w[1]), "{rows:?}");
    }
}
