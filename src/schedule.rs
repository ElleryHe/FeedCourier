//! Route delivery time windows (`HH:MM`–`HH:MM` in the system IANA timezone) and the global
//! outbox queue pause state.

use chrono::{Datelike, Duration, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::channels::{DeliveryTimeWindow, RouteOptions};
use crate::error::{AppError, AppResult};

/// Default system timezone when neither `TIMEZONE`/`TZ` nor a persisted setting exists.
pub const DEFAULT_TIMEZONE: &str = "America/Los_Angeles";

/// Persisted global outbox queue state (`system_settings.outbox_queue`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxQueueState {
    #[serde(default)]
    pub paused: bool,
    /// Unix timestamp (seconds) at which a paused queue resumes automatically.
    #[serde(default)]
    pub resume_at: Option<i64>,
}

impl OutboxQueueState {
    /// Whether delivery is blocked at `now`.
    pub fn is_blocking(&self, now: i64) -> bool {
        self.paused && self.resume_at.is_none_or(|at| at > now)
    }
}

/// Parse an IANA timezone name.
pub fn parse_timezone(raw: &str) -> AppResult<Tz> {
    raw.trim()
        .parse::<Tz>()
        .map_err(|_| AppError::Validation(format!("unknown IANA timezone '{}'", raw.trim())))
}

/// Resolve the effective timezone for a route: `options.timezone` if valid, otherwise `system_tz`.
pub fn resolve_route_timezone(options: &RouteOptions, system_tz: Tz) -> Tz {
    options
        .timezone
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<Tz>().ok())
        .unwrap_or(system_tz)
}

/// Format a UTC Unix timestamp in `tz` with explicit UTC offset, e.g. `2026-11-08 11:00 (UTC+08:00)`.
pub fn format_local_timestamp(ts: i64, tz: Tz) -> Option<String> {
    let utc = Utc.timestamp_opt(ts, 0).single()?;
    let local = utc.with_timezone(&tz);
    Some(format!(
        "{} (UTC{})",
        local.format("%Y-%m-%d %H:%M"),
        local.format("%:z")
    ))
}

/// Format a UTC Unix timestamp in `tz` in compact form without explicit UTC offset,
/// e.g. `2026-11-08 11:00`, suited for narrow mobile Telegram card footers.
pub fn format_local_timestamp_compact(ts: i64, tz: Tz) -> Option<String> {
    let utc = Utc.timestamp_opt(ts, 0).single()?;
    let local = utc.with_timezone(&tz);
    Some(local.format("%Y-%m-%d %H:%M").to_string())
}

/// Parse `HH:MM` (also `24:00` as end-of-day) into minutes since midnight.
pub fn parse_hhmm(raw: &str) -> Option<u32> {
    let (h, m) = raw.trim().split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    if m > 59 || h > 24 || (h == 24 && m != 0) {
        return None;
    }
    Some(h * 60 + m)
}

/// Validate and normalize timezone, windows, and weekdays of [`RouteOptions`].
pub fn validate_schedule(options: &mut RouteOptions) -> AppResult<()> {
    if let Some(raw_tz) = options.timezone.take() {
        let trimmed = raw_tz.trim();
        if !trimmed.is_empty() {
            let parsed = parse_timezone(trimmed)?;
            options.timezone = Some(parsed.name().to_string());
        }
    }
    if options.active_windows.len() > 12 {
        return Err(AppError::Validation(
            "at most 12 active_windows are allowed".to_string(),
        ));
    }
    for w in &mut options.active_windows {
        let (Some(s), Some(e)) = (parse_hhmm(&w.start), parse_hhmm(&w.end)) else {
            return Err(AppError::Validation(format!(
                "invalid active window '{}'–'{}' (expected HH:MM)",
                w.start, w.end
            )));
        };
        if s == e && s != 0 {
            return Err(AppError::Validation(format!(
                "invalid active window '{}'–'{}' (start and end may only be equal as 00:00–00:00 for all-day)",
                w.start, w.end
            )));
        }
        w.start = format!("{:02}:{:02}", s / 60, s % 60);
        w.end = format!("{:02}:{:02}", e / 60, e % 60);
    }
    options.active_weekdays.sort_unstable();
    options.active_weekdays.dedup();
    if options.active_weekdays.iter().any(|d| !(1..=7).contains(d)) {
        return Err(AppError::Validation(
            "active_weekdays must contain integers 1 (Mon) to 7 (Sun)".to_string(),
        ));
    }
    if options.active_weekdays.len() == 7 {
        options.active_weekdays.clear();
    }
    Ok(())
}

/// Returns `true` when `options` define a delivery schedule restriction.
pub fn has_schedule(options: &RouteOptions) -> bool {
    !options.active_windows.is_empty() || !options.active_weekdays.is_empty()
}

fn normalize_date_and_time(date: NaiveDate, minutes: u32) -> Option<(NaiveDate, NaiveTime)> {
    let (date, minutes) = if minutes >= 24 * 60 {
        (date.succ_opt()?, minutes - 24 * 60)
    } else {
        (date, minutes)
    };
    let time = NaiveTime::from_hms_opt(minutes / 60, minutes % 60, 0)?;
    Some((date, time))
}

/// Advance a nonexistent spring-forward local wall-clock instant forward minute-by-minute
/// to the first valid instant after the DST transition gap.
fn clamp_forward_dst_gap(tz: Tz, naive: chrono::NaiveDateTime) -> Option<i64> {
    for step_mins in 1..=180_i64 {
        let candidate = naive + Duration::minutes(step_mins);
        match tz.from_local_datetime(&candidate) {
            LocalResult::Single(dt) => return Some(dt.timestamp()),
            LocalResult::Ambiguous(earliest, _) => return Some(earliest.timestamp()),
            LocalResult::None => continue,
        }
    }
    None
}

/// Convert a window start wall-clock time to UTC seconds:
/// - Spring-forward gap (`None`): clamp forward to the first valid instant after the jump.
/// - Fall-back overlap (`Ambiguous`): use the earliest occurrence so the window opens on time.
fn local_start_to_utc(tz: Tz, date: NaiveDate, minutes: u32) -> Option<i64> {
    let (date, time) = normalize_date_and_time(date, minutes)?;
    let naive = date.and_time(time);
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Some(dt.timestamp()),
        LocalResult::Ambiguous(earliest, _) => Some(earliest.timestamp()),
        LocalResult::None => clamp_forward_dst_gap(tz, naive),
    }
}

/// Convert a window end wall-clock time to UTC seconds:
/// - Spring-forward gap (`None`): clamp forward to the first valid instant after the jump.
/// - Fall-back overlap (`Ambiguous`): use the latest occurrence so the window is not closed 1 hour early.
fn local_end_to_utc(tz: Tz, date: NaiveDate, minutes: u32) -> Option<i64> {
    let (date, time) = normalize_date_and_time(date, minutes)?;
    let naive = date.and_time(time);
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Some(dt.timestamp()),
        LocalResult::Ambiguous(_, latest) => Some(latest.timestamp()),
        LocalResult::None => clamp_forward_dst_gap(tz, naive),
    }
}

fn effective_windows(windows: &[DeliveryTimeWindow]) -> Vec<(u32, u32)> {
    if windows.is_empty() {
        return vec![(0, 24 * 60)];
    }
    windows
        .iter()
        .filter_map(|w| {
            let s = parse_hhmm(&w.start)?;
            let e = parse_hhmm(&w.end)?;
            if s == e {
                (s == 0).then_some((0, 24 * 60))
            } else {
                Some((s, e))
            }
        })
        .collect()
}

/// Earliest instant `>= now` (unix seconds) at which the route may deliver. Returns `now` when
/// delivery is currently allowed, no schedule is configured, or no slot exists within 15 days.
pub fn next_allowed_at(options: &RouteOptions, now: i64, tz: Tz) -> i64 {
    if !has_schedule(options) {
        return now;
    }
    let windows = effective_windows(&options.active_windows);
    if windows.is_empty() {
        return now;
    }
    let Some(now_dt) = Utc.timestamp_opt(now, 0).single() else {
        return now;
    };
    let today = now_dt.with_timezone(&tz).date_naive();

    let mut best: Option<i64> = None;
    for offset in -1i64..=15 {
        let Some(date) = today.checked_add_signed(Duration::days(offset)) else {
            continue;
        };
        let weekday = u8::try_from(date.weekday().number_from_monday()).unwrap_or(1);
        if !options.active_weekdays.is_empty() && !options.active_weekdays.contains(&weekday) {
            continue;
        }
        for &(start_min, end_min) in &windows {
            let end_is_next_day = end_min <= start_min;
            let Some(start_ts) = local_start_to_utc(tz, date, start_min) else {
                continue;
            };
            let end_minutes = if end_is_next_day {
                end_min + 24 * 60
            } else {
                end_min
            };
            let Some(end_ts) = local_end_to_utc(tz, date, end_minutes) else {
                continue;
            };
            if start_ts >= end_ts {
                continue;
            }
            if now >= start_ts && now < end_ts {
                return now;
            }
            if start_ts > now {
                best = Some(best.map_or(start_ts, |b| b.min(start_ts)));
            }
        }
    }
    best.unwrap_or(now)
}

/// Whether delivery is allowed right now under `options`.
pub fn is_allowed_now(options: &RouteOptions, now: i64, tz: Tz) -> bool {
    next_allowed_at(options, now, tz) <= now
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(windows: &[(&str, &str)], weekdays: &[u8]) -> RouteOptions {
        RouteOptions {
            active_windows: windows
                .iter()
                .map(|(s, e)| DeliveryTimeWindow {
                    start: (*s).to_string(),
                    end: (*e).to_string(),
                })
                .collect(),
            active_weekdays: weekdays.to_vec(),
            ..RouteOptions::default()
        }
    }

    fn ts(tz: Tz, y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        tz.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .earliest()
            .unwrap()
            .timestamp()
    }

    #[test]
    fn no_schedule_is_always_allowed() {
        let tz: Tz = "Asia/Shanghai".parse().unwrap();
        assert_eq!(next_allowed_at(&RouteOptions::default(), 123, tz), 123);
    }

    #[test]
    fn inside_and_outside_daily_window() {
        let tz: Tz = "Asia/Shanghai".parse().unwrap();
        let o = opts(&[("08:00", "22:00")], &[]);
        let inside = ts(tz, 2026, 11, 18, 12, 0);
        assert_eq!(next_allowed_at(&o, inside, tz), inside);
        let late = ts(tz, 2026, 11, 18, 23, 0);
        assert_eq!(next_allowed_at(&o, late, tz), ts(tz, 2026, 11, 19, 8, 0));
        let early = ts(tz, 2026, 11, 18, 6, 0);
        assert_eq!(next_allowed_at(&o, early, tz), ts(tz, 2026, 11, 18, 8, 0));
    }

    #[test]
    fn midnight_wrapping_window() {
        let tz: Tz = "Asia/Shanghai".parse().unwrap();
        let o = opts(&[("22:00", "06:00")], &[]);
        let after_midnight = ts(tz, 2026, 11, 19, 1, 0);
        assert_eq!(next_allowed_at(&o, after_midnight, tz), after_midnight);
        let noon = ts(tz, 2026, 11, 19, 12, 0);
        assert_eq!(next_allowed_at(&o, noon, tz), ts(tz, 2026, 11, 19, 22, 0));
    }

    #[test]
    fn multiple_windows_pick_next_start() {
        let tz: Tz = "America/Los_Angeles".parse().unwrap();
        let o = opts(&[("07:00", "09:00"), ("19:00", "21:00")], &[]);
        let noon = ts(tz, 2026, 7, 1, 12, 0);
        assert_eq!(next_allowed_at(&o, noon, tz), ts(tz, 2026, 7, 1, 19, 0));
    }

    #[test]
    fn weekday_restriction() {
        let tz: Tz = "Asia/Shanghai".parse().unwrap();
        // 2026-11-18 is a Wednesday (3); allow Saturdays (6) only.
        let o = opts(&[("09:00", "10:00")], &[6]);
        let now = ts(tz, 2026, 11, 18, 9, 30);
        assert_eq!(next_allowed_at(&o, now, tz), ts(tz, 2026, 11, 21, 9, 0));
    }

    #[test]
    fn validation_normalizes_and_rejects() {
        let mut ok = opts(&[("8:00", "22:00")], &[]);
        assert!(validate_schedule(&mut ok).is_err());
        let mut ok = opts(&[("08:00", "22:00")], &[7, 1, 1]);
        validate_schedule(&mut ok).unwrap();
        assert_eq!(ok.active_weekdays, vec![1, 7]);
        let mut bad = opts(&[], &[8]);
        assert!(validate_schedule(&mut bad).is_err());

        let mut tz_ok = RouteOptions {
            timezone: Some("  Asia/Shanghai  ".to_string()),
            ..RouteOptions::default()
        };
        validate_schedule(&mut tz_ok).unwrap();
        assert_eq!(tz_ok.timezone.as_deref(), Some("Asia/Shanghai"));

        let mut tz_blank = RouteOptions {
            timezone: Some("   ".to_string()),
            ..RouteOptions::default()
        };
        validate_schedule(&mut tz_blank).unwrap();
        assert_eq!(tz_blank.timezone, None);

        let mut tz_bad = RouteOptions {
            timezone: Some("Mars/Olympus".to_string()),
            ..RouteOptions::default()
        };
        assert!(validate_schedule(&mut tz_bad).is_err());

        // `00:00`–`00:00` is the sole valid all-day equal-endpoint window; non-zero equal windows
        // (e.g. `09:00`–`09:00`) must be rejected.
        let mut all_day = opts(&[("00:00", "00:00")], &[]);
        assert!(validate_schedule(&mut all_day).is_ok());

        let mut bad_equal = opts(&[("09:00", "09:00")], &[]);
        assert!(validate_schedule(&mut bad_equal).is_err());
    }

    #[test]
    fn dst_spring_forward_and_fall_back_windows() {
        let la: Tz = "America/Los_Angeles".parse().unwrap();

        // 1. Spring forward on 2026-03-08 (02:00 PST jumps directly to 03:00 PDT):
        //    Daytime window `02:30–22:00` must clamp its start forward to `03:00 PDT`, NOT drop the day.
        let daytime = opts(&[("02:30", "22:00")], &[]);
        let before_gap = la
            .with_ymd_and_hms(2026, 3, 8, 1, 30, 0)
            .single()
            .unwrap()
            .timestamp();
        let three_am_pdt = la
            .with_ymd_and_hms(2026, 3, 8, 3, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(next_allowed_at(&daytime, before_gap, la), three_am_pdt);
        assert_eq!(next_allowed_at(&daytime, three_am_pdt, la), three_am_pdt);

        // 2. Midnight-wrapping Saturday `22:00–02:30` across the 2026-03-08 spring-forward gap:
        //    end `02:30` clamps forward to `03:00 PDT`, so `01:45 PST` is inside and `03:00 PDT` is outside.
        let overnight = opts(&[("22:00", "02:30")], &[6]);
        let sun_0145_pst = la
            .with_ymd_and_hms(2026, 3, 8, 1, 45, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(next_allowed_at(&overnight, sun_0145_pst, la), sun_0145_pst);
        let next_sat_2200 = la
            .with_ymd_and_hms(2026, 3, 14, 22, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(next_allowed_at(&overnight, three_am_pdt, la), next_sat_2200);

        // 3. Fall back on 2026-11-01 (01:00–01:59 occurs twice: PDT -07:00 then PST -08:00):
        //    Window `00:30–01:30` uses latest `01:30 PST` as its end, so `01:15 PST` (second occurrence)
        //    is still inside the window!
        let fallback_win = opts(&[("00:30", "01:30")], &[]);
        let second_0115_pst = la
            .with_ymd_and_hms(2026, 11, 1, 1, 15, 0)
            .latest()
            .unwrap()
            .timestamp();
        assert_eq!(
            next_allowed_at(&fallback_win, second_0115_pst, la),
            second_0115_pst
        );
        let second_0130_pst = la
            .with_ymd_and_hms(2026, 11, 1, 1, 30, 0)
            .latest()
            .unwrap()
            .timestamp();
        let next_day_0030 = la
            .with_ymd_and_hms(2026, 11, 2, 0, 30, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(
            next_allowed_at(&fallback_win, second_0130_pst, la),
            next_day_0030
        );

        // 4. Single-weekday route (`Sunday [7]`) with a window entirely inside the 2026-03-08
        //    spring-forward DST gap (`02:10–02:40`): on Sunday 2026-03-01 at 03:00 PST, the
        //    2026-03-08 occurrence collapses to an empty interval, so `next_allowed_at` must scan
        //    14 days ahead to Sunday 2026-03-15 02:10 PDT rather than falling back to `now`.
        let sun_gap_only = opts(&[("02:10", "02:40")], &[7]);
        let sun_mar1_0300 = la
            .with_ymd_and_hms(2026, 3, 1, 3, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        let sun_mar15_0210 = la
            .with_ymd_and_hms(2026, 3, 15, 2, 10, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(
            next_allowed_at(&sun_gap_only, sun_mar1_0300, la),
            sun_mar15_0210
        );
    }

    #[test]
    fn format_local_timestamp_uses_timezone_offset() {
        let shanghai: Tz = "Asia/Shanghai".parse().unwrap();
        let utc_ts = chrono::Utc
            .with_ymd_and_hms(2026, 11, 8, 3, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(
            format_local_timestamp(utc_ts, shanghai).as_deref(),
            Some("2026-11-08 11:00 (UTC+08:00)")
        );
        let la: Tz = "America/Los_Angeles".parse().unwrap();
        assert_eq!(
            format_local_timestamp(utc_ts, la).as_deref(),
            Some("2026-11-07 19:00 (UTC-08:00)")
        );
    }

    #[test]
    fn queue_state_blocking() {
        let s = OutboxQueueState {
            paused: true,
            resume_at: Some(100),
        };
        assert!(s.is_blocking(99));
        assert!(!s.is_blocking(100));
        assert!(
            OutboxQueueState {
                paused: true,
                resume_at: None
            }
            .is_blocking(1_000_000)
        );
    }
}
