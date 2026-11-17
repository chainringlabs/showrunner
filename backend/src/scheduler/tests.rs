//! Tests for the resync scheduler (`scheduler.rs`).

use super::*;
use crate::datasources::tmdb::TmdbClient;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

async fn fresh_state() -> AppState {
    let opts = SqliteConnectOptions::from_str("sqlite::memory:").unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();
    sqlx::migrate!("./src/db/migrations")
        .run(&pool)
        .await
        .unwrap();
    let tmdb = TmdbClient::new("k".into());
    AppState::new(pool, tmdb, TimeZone::UTC)
}

fn ny(datetime: &str) -> Zoned {
    format!("{datetime}[America/New_York]").parse().unwrap()
}

fn fires(expr: &str, after: &str, count: usize) -> Vec<String> {
    let cron = parse_cron(expr).unwrap();
    let mut at = ny(after);
    (0..count)
        .map(|_| {
            at = next_fire(&cron, &at).unwrap();
            at.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()
        })
        .collect()
}

// @spec RESYNC-TRIGGER-001
#[tokio::test]
async fn start_succeeds_with_valid_cron_and_returns_the_running_task() {
    let state = fresh_state().await;
    let schedule = ScheduleConfig {
        resync_cron: "0 0 12 * * *".into(),
    };
    let task = start(state, schedule).await.unwrap();
    assert!(!task.is_finished());
    task.abort();
}

// @spec RESYNC-TRIGGER-002, APP-CONFIG-002
#[tokio::test]
async fn start_returns_config_error_for_invalid_cron() {
    let state = fresh_state().await;
    let schedule = ScheduleConfig {
        resync_cron: "this is not a cron".into(),
    };
    let result = start(state, schedule).await;
    match result {
        Err(AppError::Config(msg)) => {
            assert!(msg.contains("RESYNC_CRON"));
        }
        Err(other) => panic!("expected Config error, got {:?}", other),
        Ok(_) => panic!("expected Config error, got Ok"),
    }
}

// @spec RESYNC-TRIGGER-001
#[test]
fn the_default_and_every_documented_schedule_parse() {
    // The default, the README's "Changing the schedule" table, and the
    // config test's override.
    for expr in [
        "0 0 6 * * *",
        "0 0 */12 * * *",
        "0 0 * * * *",
        "0 0 4 * * 1",
        "0 30 5 * * *",
    ] {
        assert!(parse_cron(expr).is_ok(), "{expr} should parse");
    }
}

// @spec RESYNC-TRIGGER-002
#[test]
fn a_five_field_or_empty_expression_is_a_config_error_naming_the_variable() {
    for expr in ["0 6 * * *", "", "0 0 25 * * *"] {
        match parse_cron(expr) {
            Err(AppError::Config(msg)) => assert!(msg.contains("RESYNC_CRON"), "{msg}"),
            other => panic!("{expr:?}: expected Config error, got {other:?}"),
        }
    }
}

// @spec RESYNC-TRIGGER-001
#[test]
fn fire_times_are_evaluated_in_the_configured_timezone() {
    // 12:00 UTC is 07:00 in New York, so today's 06:00 has passed there.
    let after: Zoned = "2026-11-17T12:00:00Z[UTC]".parse().unwrap();
    let after = after.with_time_zone(TimeZone::get("America/New_York").unwrap());
    let next = next_fire(&parse_cron("0 0 6 * * *").unwrap(), &after).unwrap();
    assert_eq!(
        next.timestamp(),
        "2026-11-18T11:00:00Z".parse::<Timestamp>().unwrap()
    );
    // Mondays at 04:00: 2026-11-17 is a Tuesday.
    assert_eq!(
        fires("0 0 4 * * 1", "2026-11-17T12:00:00-05:00", 1),
        ["2026-11-23T04:00:00-05:00"]
    );
}

// @spec RESYNC-TRIGGER-009
#[test]
fn a_time_in_the_skipped_hour_fires_once_after_the_gap() {
    // New York springs forward on 2027-03-14: 02:00 jumps to 03:00.
    assert_eq!(
        fires("0 30 2 * * *", "2027-03-13T12:00:00-05:00", 3),
        [
            "2027-03-14T03:00:00-04:00",
            "2027-03-15T02:30:00-04:00",
            "2027-03-16T02:30:00-04:00",
        ]
    );
}

// @spec RESYNC-TRIGGER-009
#[test]
fn a_time_in_the_repeated_hour_fires_once_at_its_first_occurrence() {
    // New York falls back on 2026-11-01: 01:00-02:00 happens twice.
    assert_eq!(
        fires("0 30 1 * * *", "2026-10-31T12:00:00-04:00", 3),
        [
            "2026-11-01T01:30:00-04:00",
            "2026-11-02T01:30:00-05:00",
            "2026-11-03T01:30:00-05:00",
        ]
    );
}

// @spec RESYNC-TRIGGER-007
#[tokio::test]
async fn a_fire_time_that_passes_during_a_run_is_skipped() {
    let (started, running, overlapped) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    let (s, r, o) = (started.clone(), running.clone(), overlapped.clone());
    // Fires every second; each run takes 1.5 s, spanning a fire time.
    let cron = parse_cron("* * * * * *").unwrap();
    let task = tokio::spawn(run_loop(cron, TimeZone::UTC, move || {
        let (s, r, o) = (s.clone(), r.clone(), o.clone());
        async move {
            s.fetch_add(1, Ordering::SeqCst);
            if r.fetch_add(1, Ordering::SeqCst) > 0 {
                o.fetch_add(1, Ordering::SeqCst);
            }
            tokio::time::sleep(Duration::from_millis(1500)).await;
            r.fetch_sub(1, Ordering::SeqCst);
        }
    }));
    tokio::time::sleep(Duration::from_millis(4600)).await;
    task.abort();

    assert_eq!(overlapped.load(Ordering::SeqCst), 0, "two runs overlapped");
    // Four or five fire times pass in 4.6 s; skipping leaves room for at
    // most three runs, and at least two must have started.
    let runs = started.load(Ordering::SeqCst);
    assert!((2..=3).contains(&runs), "expected 2 or 3 runs, got {runs}");
}

// @spec RESYNC-TRIGGER-008
#[tokio::test]
async fn a_panicking_run_does_not_stop_the_schedule() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let cron = parse_cron("* * * * * *").unwrap();
    let task = tokio::spawn(run_loop(cron, TimeZone::UTC, move || {
        let c = c.clone();
        async move {
            if c.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("first scheduled run fails");
            }
        }
    }));
    tokio::time::sleep(Duration::from_millis(2600)).await;
    assert!(!task.is_finished(), "the loop ended after a panic");
    task.abort();
    assert!(calls.load(Ordering::SeqCst) >= 2, "no run after the panic");
}

#[tokio::test]
async fn sleep_until_returns_at_once_for_a_past_target() {
    let past = Timestamp::now() - jiff::SignedDuration::from_secs(5);
    tokio::time::timeout(Duration::from_millis(200), sleep_until(past))
        .await
        .expect("did not return for a past target");
}

#[tokio::test]
async fn the_loop_ends_when_the_expression_has_no_future_fire_time() {
    // A seven-field expression pinned to a past year.
    let cron = parse_cron("0 0 0 1 1 * 2001").unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        run_loop(cron, TimeZone::UTC, || async {}),
    )
    .await
    .expect("loop did not end");
}
