use axum::extract::{Query, State};
use axum::Json;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use crate::db::queries;
use crate::error::{AppError, Result};
use crate::models::show::CalendarEpisode;
use crate::state::{today_in, AppState};

const MAX_RANGE_DAYS: i64 = 92;

#[derive(Debug, Deserialize)]
pub struct CalendarQuery {
    pub start: String,
    pub end: String,
}

#[derive(Debug, Serialize)]
pub struct CalendarResponse {
    /// The server's current date in the configured `TIMEZONE`, so the page
    /// highlights the same day the aired rules use rather than the browser's.
    pub today: String,
    pub episodes: Vec<CalendarEpisode>,
}

// @spec AIRING-CAL-001, AIRING-CAL-002, AIRING-CAL-003, AIRING-CAL-004, AIRING-CAL-005, AIRING-CAL-013
pub async fn get_calendar(
    State(state): State<AppState>,
    Query(q): Query<CalendarQuery>,
) -> Result<Json<CalendarResponse>> {
    let start = Date::strptime("%Y-%m-%d", &q.start)
        .map_err(|_| AppError::InvalidData(format!("invalid start date: {}", q.start)))?;
    let end = Date::strptime("%Y-%m-%d", &q.end)
        .map_err(|_| AppError::InvalidData(format!("invalid end date: {}", q.end)))?;

    if end < start {
        return Err(AppError::InvalidData("end must be >= start".into()));
    }
    let span = i64::from((end - start).get_days());
    if span > MAX_RANGE_DAYS {
        return Err(AppError::InvalidData(format!(
            "range {} days exceeds max of {}",
            span, MAX_RANGE_DAYS
        )));
    }

    // Bind the *parsed* dates, not the raw query strings. `air_date` is a bare
    // TEXT column compared bytewise, so the value that reaches SQL has to be the
    // same canonical zero-padded form the span check above ran against —
    // otherwise `2026-1-5` or a leading space (both of which parse) would be
    // compared against stored dates as different text.
    let episodes = queries::list_calendar_episodes(
        &state.pool,
        &start.strftime("%Y-%m-%d").to_string(),
        &end.strftime("%Y-%m-%d").to_string(),
    )
    .await?;
    Ok(Json(CalendarResponse {
        today: today_in(&state.tz),
        episodes,
    }))
}
