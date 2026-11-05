pub fn format_log_time(time_secs: i64, time_offset_secs: i32) -> String {
    let local_secs = time_secs + time_offset_secs as i64;
    let days = local_secs.div_euclid(86_400);
    let secs_of_day = local_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// Days since the Unix epoch for a proleptic Gregorian date; inverse of `civil_from_days`.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

pub(crate) fn utc_date(secs: i64) -> String {
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix time.
pub(crate) fn utc_stamp(secs: i64) -> String {
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let of_day = secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3600,
        (of_day % 3600) / 60,
        of_day % 60
    )
}

/// Seconds since the Unix epoch of a UTC stamp: a node id's `2026-10-05T14-32-07Z`, a
/// `created` metadata value, or an API's `2026-10-12T15:11:32.000Z` whose fraction is
/// dropped.
pub(crate) fn parse_utc_stamp(stamp: &str) -> Option<i64> {
    let head = stamp.get(..20)?;
    let bytes = head.as_bytes();
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || [bytes[13], bytes[16]]
            .iter()
            .any(|b| *b != b'-' && *b != b':')
    {
        return None;
    }
    // A node id may carry a `-2` suffix after its `Z`.
    let tail = stamp.get(19..)?;
    let tail_ok =
        tail.starts_with('Z') || (tail.starts_with('.') && tail.len() > 2 && tail.ends_with('Z'));
    if !tail_ok {
        return None;
    }
    let year: i64 = head.get(0..4)?.parse().ok()?;
    let month: u32 = head.get(5..7)?.parse().ok()?;
    let day: u32 = head.get(8..10)?.parse().ok()?;
    let hour: i64 = head.get(11..13)?.parse().ok()?;
    let minute: i64 = head.get(14..16)?.parse().ok()?;
    let second: i64 = head.get(17..19)?.parse().ok()?;
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

pub(crate) fn local_hhmm(secs: i64, offset_secs: i32) -> String {
    let minutes = (secs + i64::from(offset_secs)).rem_euclid(86_400) / 60;
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

pub(crate) fn local_offset_secs(time_secs: i64) -> i32 {
    let time = time_secs as libc::time_t;
    // SAFETY: `tm` is plain data that `localtime_r` fully initializes on success, and
    // both pointers outlive the call.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() {
            return 0;
        }
        tm
    };
    tm.tm_gmtoff as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_log_time_renders_local_datetime() {
        assert_eq!(format_log_time(0, 0), "1970-01-01 00:00");
        assert_eq!(format_log_time(1_700_000_000, 0), "2023-11-14 22:13");
        assert_eq!(format_log_time(-1, 0), "1969-12-31 23:59");
        assert_eq!(format_log_time(0, 7_200), "1970-01-01 02:00");
        assert_eq!(format_log_time(0, -3_600), "1969-12-31 23:00");
    }

    #[test]
    fn utc_date_formats_the_day_of_a_unix_time() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(86_400 * 365 + 3600), "1971-01-01");
    }

    #[test]
    fn parse_utc_stamp_reads_both_stamp_shapes() {
        assert_eq!(parse_utc_stamp("2026-10-21T03:51:21Z"), Some(1_792_554_681));
        assert_eq!(parse_utc_stamp("1970-01-02T00:00:01Z"), Some(86_401));
        assert_eq!(
            parse_utc_stamp("2026-10-12T15:11:32.000Z"),
            Some(1_791_817_892)
        );
        assert_eq!(
            parse_utc_stamp("2026-10-05T14-32-07Z"),
            Some(days_from_civil(2026, 10, 5) * 86_400 + 14 * 3600 + 32 * 60 + 7)
        );
    }

    #[test]
    fn parse_utc_stamp_refuses_other_shapes() {
        assert_eq!(parse_utc_stamp("1970-01-02 00:00:01Z"), None);
        assert_eq!(parse_utc_stamp("too short"), None);
        assert_eq!(parse_utc_stamp("2026-10-12T15:11:32"), None);
        assert_eq!(parse_utc_stamp("2026-10-12T15:11:32.000X"), None);
    }

    #[test]
    fn local_hhmm_is_local_24h_rolling_over_midnight() {
        assert_eq!(local_hhmm(14 * 3600 + 32 * 60, 0), "14:32");
        assert_eq!(local_hhmm(86_400 - 60, 0), "23:59");
        assert_eq!(local_hhmm(86_400 + 60, 0), "00:01");
        assert_eq!(local_hhmm(0, -3_600), "23:00");
    }
}
