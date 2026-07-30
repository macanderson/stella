//! The smallest amount of calendar arithmetic the record engine needs.
//!
//! Records carry RFC-3339 timestamps and ISO-8601 durations as **strings** (see
//! [`super::super::ingest::record`]) so the surface stays a plain serde shape with
//! no datetime dependency. The truth sweep nonetheless has to answer one
//! question — *is this record's re-verification due?* — which is arithmetic.
//!
//! This module is that arithmetic and nothing else: RFC-3339 to epoch seconds,
//! ISO-8601 duration to seconds, and the comparison built on both. It is
//! deliberately not a date library.
//!
//! # Why an approximation is the honest choice for months and years
//!
//! `P1M` in ISO-8601 is *one calendar month*, whose length depends on which
//! month you are in. A `review_every = "P1M"` record is asking to be re-checked
//! about monthly; resolving that to 30 days can make a sweep fire a day early in
//! March. Firing a *probe* a day early costs one filesystem read and produces the
//! same verdict. Pretending to calendar precision we cannot reach without a date
//! library would cost a dependency and buy nothing, so the approximation is
//! stated here rather than hidden: `Y = 365d`, `M = 30d`.
//!
//! What must NOT be approximate is the direction of the comparison. A sweep that
//! rounded the *other* way — deciding a due record was not yet due — would let a
//! stale record keep steering, which is the failure the truth axis exists to
//! prevent. [`is_due`] therefore treats an unparseable interval as **due**.

/// Days per year, for `PnY`. See the module docs on why this is approximate.
const DAYS_PER_YEAR: i64 = 365;
/// Days per month, for `PnM` in the date half. See the module docs.
const DAYS_PER_MONTH: i64 = 30;

const SECS_PER_MINUTE: i64 = 60;
const SECS_PER_HOUR: i64 = 3_600;
const SECS_PER_DAY: i64 = 86_400;

/// Parse an RFC-3339 timestamp to seconds since the Unix epoch.
///
/// Accepts the shapes the record surface actually emits and that a human plausibly
/// hand-writes: `2026-07-20T18:30:00Z`, a fractional-second form, and a numeric
/// offset (`+02:00`). `None` for anything else — a caller that cannot read a
/// timestamp must decide what to do about it rather than silently receive a zero,
/// which would date the record to 1970 and make every sweep fire.
pub fn epoch_seconds(rfc3339: &str) -> Option<i64> {
    let text = rfc3339.trim();
    // Date: YYYY-MM-DD
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i64 = text.get(0..4)?.parse().ok()?;
    let month: i64 = text.get(5..7)?.parse().ok()?;
    let day: i64 = text.get(8..10)?.parse().ok()?;
    // The date/time separator is `T` by the spec, lowercase `t` and a space by
    // RFC 3339's own permissive note.
    if !matches!(bytes[10], b'T' | b't' | b' ') {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let hour: i64 = text.get(11..13)?.parse().ok()?;
    let minute: i64 = text.get(14..16)?.parse().ok()?;
    let second: i64 = text.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let offset = offset_seconds(text.get(19..).unwrap_or(""))?;
    let days = days_from_civil(year, month, day);
    Some(days * SECS_PER_DAY + hour * SECS_PER_HOUR + minute * SECS_PER_MINUTE + second - offset)
}

/// The timezone offset in seconds from the tail after the seconds field, which
/// may still carry a fractional-second part. `Z`/`z`/empty are zero.
fn offset_seconds(tail: &str) -> Option<i64> {
    // Skip a fractional second (`.123456`).
    let rest = match tail.strip_prefix('.') {
        Some(frac) => {
            let digits = frac.chars().take_while(char::is_ascii_digit).count();
            frac.get(digits..)?
        }
        None => tail,
    };
    if rest.is_empty() || rest == "Z" || rest == "z" {
        return Some(0);
    }
    let (sign, hhmm) = match rest.as_bytes().first()? {
        b'+' => (1, &rest[1..]),
        b'-' => (-1, &rest[1..]),
        _ => return None,
    };
    let (hh, mm) = hhmm.split_once(':')?;
    let hours: i64 = hh.parse().ok()?;
    let minutes: i64 = mm.parse().ok()?;
    Some(sign * (hours * SECS_PER_HOUR + minutes * SECS_PER_MINUTE))
}

/// Days from the Unix epoch for a proleptic-Gregorian civil date — Howard
/// Hinnant's `days_from_civil`, which is exact and needs no lookup tables.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse an ISO-8601 duration (`P90D`, `P1Y`, `PT12H`, `P1DT6H`) to seconds.
///
/// `None` when the string is not a duration this subset understands. Callers
/// treat that as "cannot schedule", and [`is_due`] resolves it to *due* — see the
/// module docs on which direction is safe.
pub fn duration_seconds(iso: &str) -> Option<i64> {
    let body = iso
        .trim()
        .strip_prefix('P')
        .or_else(|| iso.strip_prefix('p'))?;
    let (date_part, time_part) = match body.split_once(['T', 't']) {
        Some((date, time)) => (date, Some(time)),
        None => (body, None),
    };
    let mut total = 0i64;
    let mut saw_any = false;

    let mut digits = String::new();
    for ch in date_part.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        let value: i64 = digits.parse().ok()?;
        digits.clear();
        saw_any = true;
        total += match ch {
            'Y' | 'y' => value * DAYS_PER_YEAR * SECS_PER_DAY,
            'M' | 'm' => value * DAYS_PER_MONTH * SECS_PER_DAY,
            'W' | 'w' => value * 7 * SECS_PER_DAY,
            'D' | 'd' => value * SECS_PER_DAY,
            _ => return None,
        };
    }
    if !digits.is_empty() {
        // A trailing number with no unit is not a duration.
        return None;
    }

    if let Some(time) = time_part {
        for ch in time.chars() {
            if ch.is_ascii_digit() {
                digits.push(ch);
                continue;
            }
            let value: i64 = digits.parse().ok()?;
            digits.clear();
            saw_any = true;
            total += match ch {
                'H' | 'h' => value * SECS_PER_HOUR,
                'M' | 'm' => value * SECS_PER_MINUTE,
                'S' | 's' => value,
                _ => return None,
            };
        }
        if !digits.is_empty() {
            return None;
        }
    }

    saw_any.then_some(total)
}

/// Render epoch seconds back as a UTC RFC-3339 timestamp.
///
/// The inverse of [`epoch_seconds`] for the `Z` form, which is the only form
/// anything here writes: a ledger that recorded local offsets would sort wrong the
/// moment somebody crossed a timezone.
pub fn to_rfc3339(epoch: i64) -> String {
    let days = epoch.div_euclid(SECS_PER_DAY);
    let secs = epoch.rem_euclid(SECS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        secs / SECS_PER_HOUR,
        (secs % SECS_PER_HOUR) / SECS_PER_MINUTE,
        secs % SECS_PER_MINUTE,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// `days_from_civil` inverted — Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `timestamp + duration`, as an RFC-3339 string. `None` when either input is
/// unreadable — a cooldown nobody can compute must not silently become "now".
pub fn plus_duration(timestamp: &str, duration: &str) -> Option<String> {
    let start = epoch_seconds(timestamp)?;
    let step = duration_seconds(duration)?;
    Some(to_rfc3339(start.saturating_add(step)))
}

/// Whether `deadline` is still in the future as of `now`. An unreadable deadline is
/// treated as **still in force**: a cooldown that cannot be read must not be the
/// reason a declined claim comes back.
pub fn is_before(now: &str, deadline: &str) -> bool {
    match (epoch_seconds(now), epoch_seconds(deadline)) {
        (Some(now), Some(deadline)) => now < deadline,
        _ => true,
    }
}

/// Whether `interval` has elapsed since `last` as of `now`.
///
/// **Unreadable inputs mean due.** A record whose interval or last-checked stamp
/// cannot be parsed is one nobody can prove is still fresh, and the cost of the
/// two answers is asymmetric: re-running a declarative probe is a filesystem
/// read, while skipping one lets a refuted claim keep steering every turn.
pub fn is_due(last: Option<&str>, interval: Option<&str>, now: &str) -> bool {
    let Some(interval) = interval else {
        // No cadence declared: nothing schedules this record, so it is not due.
        // (`Truth::probe` with no `review_every`/`ttl` is checked on demand by
        // `stella context validate`, never on a timer.)
        return false;
    };
    let (Some(last), Some(step), Some(now)) = (
        last.and_then(epoch_seconds),
        duration_seconds(interval),
        epoch_seconds(now),
    ) else {
        return true;
    };
    now.saturating_sub(last) >= step
}

/// Whether `valid_from + ttl` is in the past as of `now` — the record's claim has
/// outlived its declared shelf life. Same unreadable-means-expired asymmetry as
/// [`is_due`].
pub fn is_expired(valid_from: Option<&str>, ttl: Option<&str>, now: &str) -> bool {
    let Some(ttl) = ttl else {
        return false;
    };
    let (Some(from), Some(step), Some(now)) = (
        valid_from.and_then(epoch_seconds),
        duration_seconds(ttl),
        epoch_seconds(now),
    ) else {
        return true;
    };
    now >= from.saturating_add(step)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_seconds_reads_the_surface_timestamp_shape() {
        assert_eq!(epoch_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_seconds("2026-07-20T18:30:00Z"), Some(1_784_572_200));
        assert_eq!(
            epoch_seconds("2026-07-20T18:30:00.123456Z"),
            Some(1_784_572_200),
            "a fractional second is dropped, not rejected"
        );
    }

    #[test]
    fn a_numeric_offset_is_normalized_to_utc() {
        let utc = epoch_seconds("2026-07-20T18:30:00Z").unwrap();
        assert_eq!(
            epoch_seconds("2026-07-20T20:30:00+02:00"),
            Some(utc),
            "the same instant written in another zone must compare equal"
        );
        assert_eq!(epoch_seconds("2026-07-20T16:30:00-02:00"), Some(utc));
    }

    #[test]
    fn a_malformed_timestamp_is_none_rather_than_epoch_zero() {
        for bad in [
            "",
            "2026-07-20",
            "not-a-date",
            "2026-13-01T00:00:00Z",
            "2026-07-20T25:00:00Z",
            "2026/07/20T00:00:00Z",
        ] {
            assert_eq!(epoch_seconds(bad), None, "{bad} must not parse");
        }
    }

    #[test]
    fn duration_seconds_covers_the_shapes_the_surface_emits() {
        assert_eq!(duration_seconds("P90D"), Some(90 * SECS_PER_DAY));
        assert_eq!(duration_seconds("P1Y"), Some(365 * SECS_PER_DAY));
        assert_eq!(duration_seconds("P1M"), Some(30 * SECS_PER_DAY));
        assert_eq!(duration_seconds("P2W"), Some(14 * SECS_PER_DAY));
        assert_eq!(duration_seconds("PT12H"), Some(12 * SECS_PER_HOUR));
        assert_eq!(
            duration_seconds("P1DT6H30M"),
            Some(SECS_PER_DAY + 6 * SECS_PER_HOUR + 30 * SECS_PER_MINUTE)
        );
    }

    #[test]
    fn minutes_and_months_are_disambiguated_by_position_not_case() {
        assert_eq!(
            duration_seconds("PT1M"),
            Some(SECS_PER_MINUTE),
            "M after T is minutes"
        );
        assert_eq!(
            duration_seconds("P1M"),
            Some(30 * SECS_PER_DAY),
            "M before T is months"
        );
    }

    #[test]
    fn a_non_duration_is_none() {
        for bad in ["", "90D", "P", "P90", "PT", "P1X", "hello"] {
            assert_eq!(duration_seconds(bad), None, "{bad} must not parse");
        }
    }

    #[test]
    fn is_due_fires_once_the_interval_has_elapsed() {
        let now = "2026-07-20T00:00:00Z";
        assert!(!is_due(Some("2026-07-19T00:00:00Z"), Some("P90D"), now));
        assert!(is_due(Some("2026-01-01T00:00:00Z"), Some("P90D"), now));
        assert!(
            is_due(Some("2026-04-21T00:00:00Z"), Some("P90D"), now),
            "exactly at the interval counts as due"
        );
    }

    #[test]
    fn no_declared_cadence_never_becomes_due_on_a_timer() {
        assert!(!is_due(
            Some("1999-01-01T00:00:00Z"),
            None,
            "2026-07-20T00:00:00Z"
        ));
    }

    #[test]
    fn unreadable_scheduling_inputs_resolve_to_due() {
        let now = "2026-07-20T00:00:00Z";
        assert!(
            is_due(None, Some("P90D"), now),
            "never checked, with a cadence declared, is due"
        );
        assert!(
            is_due(Some("yesterday"), Some("P90D"), now),
            "an unparseable last-checked must not read as fresh"
        );
        assert!(
            is_due(Some("2026-07-19T00:00:00Z"), Some("every quarter"), now),
            "an unparseable interval must not read as fresh"
        );
    }

    #[test]
    fn to_rfc3339_round_trips_epoch_seconds() {
        for stamp in [
            "1970-01-01T00:00:00Z",
            "2026-07-20T18:30:00Z",
            "2000-02-29T23:59:59Z",
            "2100-03-01T00:00:00Z",
        ] {
            let epoch = epoch_seconds(stamp).expect("parses");
            assert_eq!(to_rfc3339(epoch), stamp, "round trip failed for {stamp}");
        }
    }

    #[test]
    fn plus_duration_advances_a_timestamp() {
        assert_eq!(
            plus_duration("2026-07-20T00:00:00Z", "P90D").as_deref(),
            Some("2026-10-18T00:00:00Z")
        );
        assert_eq!(plus_duration("nonsense", "P90D"), None);
        assert_eq!(plus_duration("2026-07-20T00:00:00Z", "soon"), None);
    }

    #[test]
    fn an_unreadable_deadline_stays_in_force() {
        assert!(is_before("2026-07-20T00:00:00Z", "2026-10-18T00:00:00Z"));
        assert!(!is_before("2026-10-19T00:00:00Z", "2026-10-18T00:00:00Z"));
        assert!(
            is_before("2026-07-20T00:00:00Z", "whenever"),
            "a cooldown nobody can read must not be the reason a decline is forgotten"
        );
    }

    #[test]
    fn is_expired_compares_valid_from_plus_ttl() {
        let now = "2026-07-20T00:00:00Z";
        assert!(!is_expired(Some("2026-07-01T00:00:00Z"), Some("P90D"), now));
        assert!(is_expired(Some("2026-01-01T00:00:00Z"), Some("P90D"), now));
        assert!(
            !is_expired(Some("2026-01-01T00:00:00Z"), None, now),
            "no ttl means no expiry"
        );
        assert!(
            is_expired(None, Some("P90D"), now),
            "a ttl with no anchor cannot be proven fresh"
        );
    }
}
