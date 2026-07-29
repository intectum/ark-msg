use time::macros::format_description;
use time::{Duration, OffsetDateTime, UtcOffset, Weekday};

/// Format a timestamp relative to now, in local time.
///
/// - <45s: "now"
/// - <60m: "N min ago"
/// - <24h same local day: "N hr ago"
/// - previous local day: "yesterday HH:MM"
/// - <7 days: weekday name + local HH:MM
/// - >=7 days: YYYY-MM-DD
pub fn relative(ts_utc: OffsetDateTime) -> String {
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let ts = ts_utc.to_offset(offset);
    let now = OffsetDateTime::now_utc().to_offset(offset);
    let delta = now - ts;
    format_delta(delta, ts, now)
}

fn format_delta(delta: Duration, ts: OffsetDateTime, now: OffsetDateTime) -> String {
    if delta < Duration::seconds(-30) {
        return ts.date().to_string();
    }
    let secs = delta.whole_seconds().max(0);
    if secs < 45 { return "now".into(); }
    let mins = delta.whole_minutes();
    if mins < 60 { return format!("{} min ago", mins); }
    let hrs = delta.whole_hours();
    if hrs < 24 && ts.date() == now.date() {
        return format!("{} hr ago", hrs);
    }
    let days = (now.date() - ts.date()).whole_days();
    let fmt = format_description!("[hour]:[minute]");
    let time_str = ts.format(&fmt).unwrap_or_default();
    if days == 1 {
        return format!("yesterday {}", time_str);
    }
    if days < 7 {
        return format!("{} {}", weekday_short(ts.weekday()), time_str);
    }
    ts.date().to_string()
}

fn weekday_short(w: Weekday) -> &'static str {
    match w {
        Weekday::Monday => "Mon",
        Weekday::Tuesday => "Tue",
        Weekday::Wednesday => "Wed",
        Weekday::Thursday => "Thu",
        Weekday::Friday => "Fri",
        Weekday::Saturday => "Sat",
        Weekday::Sunday => "Sun",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn fmt_at(now: OffsetDateTime, ts: OffsetDateTime) -> String {
        format_delta(now - ts, ts, now)
    }

    #[test]
    fn now_within_45s() {
        let now = datetime!(2026-07-26 12:00:00 UTC);
        let ts = datetime!(2026-07-26 11:59:20 UTC);
        assert_eq!(fmt_at(now, ts), "now");
    }

    #[test]
    fn minutes_ago() {
        let now = datetime!(2026-07-26 12:05:00 UTC);
        let ts = datetime!(2026-07-26 12:00:00 UTC);
        assert_eq!(fmt_at(now, ts), "5 min ago");
    }

    #[test]
    fn hours_ago_same_day() {
        let now = datetime!(2026-07-26 15:00:00 UTC);
        let ts = datetime!(2026-07-26 12:00:00 UTC);
        assert_eq!(fmt_at(now, ts), "3 hr ago");
    }

    #[test]
    fn yesterday_bucket() {
        let now = datetime!(2026-07-26 12:00:00 UTC);
        let ts = datetime!(2026-07-25 22:30:00 UTC);
        assert_eq!(fmt_at(now, ts), "yesterday 22:30");
    }

    #[test]
    fn weekday_within_week() {
        let now = datetime!(2026-07-26 12:00:00 UTC);
        let ts = datetime!(2026-07-22 09:15:00 UTC);
        assert_eq!(fmt_at(now, ts), "Wed 09:15");
    }

    #[test]
    fn date_after_week() {
        let now = datetime!(2026-07-26 12:00:00 UTC);
        let ts = datetime!(2026-07-10 09:15:00 UTC);
        assert_eq!(fmt_at(now, ts), "2026-07-10");
    }

    #[test]
    fn future_stamp_falls_back_to_date() {
        let now = datetime!(2026-07-26 12:00:00 UTC);
        let ts = datetime!(2027-01-01 00:00:00 UTC);
        assert_eq!(fmt_at(now, ts), "2027-01-01");
    }
}
