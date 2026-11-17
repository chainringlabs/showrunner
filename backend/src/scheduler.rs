use std::future::Future;
use std::time::Duration;

use croner::parser::{CronParser, Seconds};
use croner::Cron;
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use tokio::task::JoinHandle;

use crate::config::ScheduleConfig;
use crate::error::{AppError, Result};
use crate::logic::resync;
use crate::state::AppState;

/// Longest single sleep before the wall clock is read again, so a host suspend
/// or a clock step moves a fire by at most this much.
const MAX_SLEEP: Duration = Duration::from_secs(60);

/// Start the cron-driven TMDB resync. The returned task runs for the life of
/// the process.
// @spec RESYNC-TRIGGER-001, RESYNC-TRIGGER-002, APP-CONFIG-002
pub async fn start(state: AppState, schedule: ScheduleConfig) -> Result<JoinHandle<()>> {
    let cron = parse_cron(&schedule.resync_cron)?;
    let tz = state.tz.clone();

    tracing::info!(
        cron = %schedule.resync_cron,
        timezone = tz.iana_name().unwrap_or("unnamed"),
        "scheduler started"
    );

    Ok(tokio::spawn(run_loop(cron, tz, move || {
        let state = state.clone();
        async move {
            match resync::resync_all(&state.pool, &state.tmdb).await {
                Ok(report) => tracing::info!(
                    synced = report.shows_synced,
                    failed = report.errors.len(),
                    "scheduled resync done"
                ),
                Err(e) => tracing::error!(error = %e, "scheduled resync errored"),
            }
        }
    })))
}

/// Parse `RESYNC_CRON`: seconds are required, and a day-of-month and a
/// day-of-week given together must both match.
// @spec RESYNC-TRIGGER-001, RESYNC-TRIGGER-002
fn parse_cron(expr: &str) -> Result<Cron> {
    CronParser::builder()
        .seconds(Seconds::Required)
        .dom_and_dow(true)
        .build()
        .parse(expr)
        .map_err(|e| AppError::Config(format!("Invalid RESYNC_CRON: {}", e)))
}

/// The first fire time strictly after `after`, in `after`'s time zone. `None`
/// when the expression has no further occurrence (a year field in the past).
// @spec RESYNC-TRIGGER-001, RESYNC-TRIGGER-009
fn next_fire(cron: &Cron, after: &Zoned) -> Option<Zoned> {
    cron.find_next_occurrence(after, false).ok()
}

/// Fire `job` at each cron time in `tz`, one run at a time. The next fire time
/// is computed after a run ends, so times that pass during a run are skipped.
/// Each run is its own task, so a panic is logged and the schedule continues.
// @spec RESYNC-TRIGGER-007, RESYNC-TRIGGER-008
async fn run_loop<F, Fut>(cron: Cron, tz: TimeZone, job: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    loop {
        let now = Timestamp::now().to_zoned(tz.clone());
        let Some(next) = next_fire(&cron, &now) else {
            tracing::warn!("resync schedule has no further fire times; scheduler stopped");
            return;
        };
        sleep_until(next.timestamp()).await;
        if let Err(e) = tokio::spawn(job()).await {
            tracing::error!(error = %e, "scheduled resync panicked");
        }
    }
}

/// Sleep until the wall clock reaches `target`, re-reading it at least every
/// `MAX_SLEEP`.
async fn sleep_until(target: Timestamp) {
    loop {
        let remaining = target.duration_since(Timestamp::now());
        let Ok(remaining) = Duration::try_from(remaining) else {
            return; // negative: the target has passed
        };
        if remaining.is_zero() {
            return;
        }
        tokio::time::sleep(remaining.min(MAX_SLEEP)).await;
    }
}

#[cfg(test)]
mod tests;
