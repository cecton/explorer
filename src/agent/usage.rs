use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// The stretch of time one limit of a subscription covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Span {
    FiveHours,
    Week,
    Month,
}

/// How much of one limit is used, and when it starts over.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageWindow {
    pub span: Span,
    pub percent: u32,
    /// Epoch seconds; `None` when the provider reports no reset for this window.
    pub resets_at: Option<i64>,
}

/// What a subscription reports of its limits, shortest window first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub windows: Vec<UsageWindow>,
}

impl UsageWindow {
    /// The percent used at `now` and the local time of the reset, `offset_of` giving
    /// the local offset at that reset. A window whose reset has passed started over:
    /// nothing of it is used, and its next reset is not known. A window with no known
    /// reset states its percent alone.
    fn shown(&self, now: i64, offset_of: &impl Fn(i64) -> i32) -> (u32, Option<String>) {
        let Some(resets_at) = self.resets_at else {
            return (self.percent, None);
        };
        if resets_at <= now {
            return (0, None);
        }
        let local = resets_at + i64::from(offset_of(resets_at));
        let days = local.div_euclid(86_400);
        let secs = local.rem_euclid(86_400);
        let time = format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60);
        let reset = match self.span {
            Span::FiveHours => time,
            Span::Week => {
                // Day 0 of the epoch is a Thursday.
                let weekday = WEEKDAYS[(days + 4).rem_euclid(7) as usize];
                format!("{weekday} {time}")
            }
            Span::Month => {
                let (_, _, day) = crate::git::civil_from_days(days);
                format!("{day}{} {time}", ordinal_suffix(day))
            }
        };
        (self.percent, Some(reset))
    }
}

fn ordinal_suffix(day: u32) -> &'static str {
    match (day % 10, day % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    }
}

impl Usage {
    /// The status-bar entry of the subscription named `label`, as the pieces of
    /// `label 22% 17:00 · 18% Thu 01:00`. A piece that states a percent carries it,
    /// for its color. `offset_of` gives the local offset at an instant.
    pub fn pieces(
        &self,
        label: &str,
        now: i64,
        offset_of: impl Fn(i64) -> i32,
    ) -> Vec<(String, Option<u32>)> {
        let mut pieces = Vec::with_capacity(self.windows.len() * 3 + 2);
        pieces.push((format!("│ {label}"), None));
        for (index, window) in self.windows.iter().enumerate() {
            let (percent, reset) = window.shown(now, &offset_of);
            let separator = if index == 0 { " " } else { " · " };
            pieces.push((separator.to_string(), None));
            pieces.push((format!("{percent}%"), Some(percent)));
            if let Some(reset) = reset {
                pieces.push((format!(" {reset}"), None));
            }
        }
        pieces.push((" ".to_string(), None));
        pieces
    }
}

#[derive(Deserialize)]
struct GoReply {
    usage: GoUsage,
}

#[derive(Deserialize)]
struct GoUsage {
    rolling: Option<GoWindow>,
    weekly: Option<GoWindow>,
    monthly: Option<GoWindow>,
}

#[derive(Deserialize)]
struct GoWindow {
    percent: u32,
    #[serde(rename = "resetsAt")]
    resets_at: String,
}

/// Epoch seconds of a UTC timestamp such as `2026-10-12T15:11:32.000Z`.
fn utc_secs(stamp: &str) -> Option<i64> {
    let bytes = stamp.as_bytes();
    if !stamp.ends_with('Z')
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }
    let year: i64 = stamp.get(0..4)?.parse().ok()?;
    let month: u32 = stamp.get(5..7)?.parse().ok()?;
    let day: u32 = stamp.get(8..10)?.parse().ok()?;
    let hour: i64 = stamp.get(11..13)?.parse().ok()?;
    let minute: i64 = stamp.get(14..16)?.parse().ok()?;
    let second: i64 = stamp.get(17..19)?.parse().ok()?;
    let days = crate::git::days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// The usage in the answer of OpenCode Go's `usage` request.
fn parse_go(body: &str) -> Result<Usage, String> {
    let reply: GoReply = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let mut windows = Vec::with_capacity(3);
    for (span, window) in [
        (Span::FiveHours, reply.usage.rolling),
        (Span::Week, reply.usage.weekly),
        (Span::Month, reply.usage.monthly),
    ] {
        let Some(window) = window else {
            continue;
        };
        let resets_at = utc_secs(&window.resets_at)
            .ok_or_else(|| format!("unreadable reset time '{}'", window.resets_at))?;
        windows.push(UsageWindow {
            span,
            percent: window.percent,
            resets_at: Some(resets_at),
        });
    }
    Ok(Usage { windows })
}

/// How much of an OpenCode Go subscription is used, from `GET <url>/usage`.
pub async fn fetch(url: &str, token: &str) -> Result<Usage, String> {
    let url = format!("{}/usage", url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("{url}: HTTP {}", response.status()));
    }
    let body = response.text().await.map_err(|e| e.to_string())?;
    parse_go(&body)
}

#[derive(Deserialize)]
struct KimiReply {
    usages: KimiUsages,
}

#[derive(Deserialize, Default)]
struct KimiUsages {
    limit_5h: Option<KimiWindow>,
    limit_7d: Option<KimiWindow>,
    limit_month_total: Option<KimiWindow>,
}

#[derive(Deserialize)]
struct KimiWindow {
    used_ratio: f64,
    reset_time: String,
}

impl KimiUsages {
    fn is_empty(&self) -> bool {
        self.limit_5h.is_none() && self.limit_7d.is_none() && self.limit_month_total.is_none()
    }
}

fn parse_kimi(body: &str) -> Result<Usage, String> {
    let reply: KimiReply = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if reply.usages.is_empty() {
        return Err("no usage window in answer".to_string());
    }
    let mut windows = Vec::with_capacity(3);
    for (span, window) in [
        (Span::FiveHours, reply.usages.limit_5h),
        (Span::Week, reply.usages.limit_7d),
        (Span::Month, reply.usages.limit_month_total),
    ] {
        let Some(window) = window else {
            continue;
        };
        let resets_at = utc_secs(&window.reset_time)
            .ok_or_else(|| format!("unreadable reset time '{}'", window.reset_time))?;
        windows.push(UsageWindow {
            span,
            percent: (window.used_ratio * 100.0).round().clamp(0.0, 100.0) as u32,
            resets_at: Some(resets_at),
        });
    }
    Ok(Usage { windows })
}

/// How much of a Kimi Code membership is used, from `GET <url>/usages`.
pub async fn fetch_kimi(url: &str, token: &str) -> Result<Usage, String> {
    let url = format!("{}/usages", url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("{url}: HTTP {}", response.status()));
    }
    let body = response.text().await.map_err(|e| e.to_string())?;
    parse_kimi(&body)
}

/// `<data_dir>/usage.json`: a subscription is the same in every repository, so its
/// last known usage is not part of a repository's session.
fn store_path() -> Option<Utf8PathBuf> {
    directories::ProjectDirs::from("", "", "explorer")
        .and_then(|dirs| Utf8PathBuf::from_path_buf(dirs.data_dir().join("usage.json")).ok())
}

/// The last known usage of each endpoint, by endpoint name. A missing or unreadable
/// file yields none.
pub fn load() -> HashMap<String, Usage> {
    let Some(path) = store_path() else {
        return HashMap::new();
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
            tracing::warn!("ignoring {path}: {e}");
            HashMap::new()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => {
            tracing::warn!("cannot read {path}: {e}");
            HashMap::new()
        }
    }
}

pub fn save(usage: &HashMap<String, Usage>) {
    let Some(path) = store_path() else {
        return;
    };
    let written = serde_json::to_string_pretty(usage)
        .map_err(std::io::Error::other)
        .and_then(|content| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            crate::conversation::write_atomic(&path, &content)
        });
    if let Err(e) = written {
        tracing::warn!("failed to write {path}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::EndpointKind;

    fn text(pieces: &[(String, Option<u32>)]) -> String {
        pieces.iter().map(|(text, _)| text.as_str()).collect()
    }

    #[test]
    fn go_answer_gives_the_three_windows_shortest_first() {
        let body = r#"{"usage":{"rolling":{"status":"ok","percent":1,"resetsAt":"2026-10-12T15:11:32.000Z"},"weekly":{"status":"ok","percent":39,"resetsAt":"2026-10-19T00:00:00.000Z"},"monthly":{"status":"ok","percent":19,"resetsAt":"2026-11-10T09:46:37.000Z"}}}"#;
        let usage = parse_go(body).expect("valid answer");
        assert_eq!(
            usage.windows,
            [
                UsageWindow {
                    span: Span::FiveHours,
                    percent: 1,
                    resets_at: Some(1_791_817_892)
                },
                UsageWindow {
                    span: Span::Week,
                    percent: 39,
                    resets_at: Some(1_792_368_000)
                },
                UsageWindow {
                    span: Span::Month,
                    percent: 19,
                    resets_at: Some(1_794_303_997)
                },
            ]
        );
    }

    #[test]
    fn go_answer_of_another_shape_is_an_error() {
        assert!(parse_go(r#"{"code":500,"success":false}"#).is_err());
        assert!(parse_go(r#"{"usage":{"rolling":{"percent":1,"resetsAt":"tomorrow"}}}"#).is_err());
    }

    #[test]
    fn kimi_answer_gives_the_quota_windows_shortest_first() {
        let body = r#"{
            "limits": [
                {
                    "window": {"duration": 300, "timeUnit": "TIME_UNIT_MINUTE"},
                    "detail": {
                        "limit": "100",
                        "used": "16",
                        "remaining": "84",
                        "resetTime": "2026-11-01T12:32:35.416426Z"
                    }
                }
            ],
            "usages": {
                "limit_5h": {"used_ratio": 0.1561, "reset_time": "2026-11-01T12:32:35Z"},
                "limit_7d": {"used_ratio": 0.2, "reset_time": "2026-11-07T00:00:00Z"},
                "limit_month_total": {"used_ratio": 0.0192, "reset_time": "2026-12-02T00:00:00Z"},
                "limit_month_code": {"used_ratio": 0.0192, "reset_time": "2026-12-02T00:00:00Z"}
            }
        }"#;
        let usage = parse_kimi(body).expect("valid answer");
        assert_eq!(
            usage.windows,
            [
                UsageWindow {
                    span: Span::FiveHours,
                    percent: 16,
                    resets_at: Some(1_793_536_355)
                },
                UsageWindow {
                    span: Span::Week,
                    percent: 20,
                    resets_at: Some(1_794_009_600)
                },
                UsageWindow {
                    span: Span::Month,
                    percent: 2,
                    resets_at: Some(1_796_169_600)
                },
            ]
        );
    }

    #[test]
    fn kimi_answer_with_a_single_window_keeps_it() {
        let body = r#"{
            "usages": {
                "limit_5h": {"used_ratio": 0.5, "reset_time": "2026-09-11T18:00:00Z"}
            }
        }"#;
        let usage = parse_kimi(body).expect("valid answer");
        assert_eq!(
            usage.windows,
            [UsageWindow {
                span: Span::FiveHours,
                percent: 50,
                resets_at: Some(1_789_149_600)
            }]
        );
    }

    #[test]
    fn kimi_answer_of_another_shape_is_an_error() {
        assert!(
            parse_kimi(r#"{"usages":{"limit_5h":{"used_ratio":1,"reset_time":"tomorrow"}}}"#)
                .is_err()
        );
        assert!(parse_kimi(
            r#"{"usages":{"limit_5h":{"used_ratio":"half","reset_time":"2026-09-11T18:00:00Z"}}}"#
        )
        .is_err());
    }

    #[test]
    fn kimi_usage_empty_payload_is_an_error() {
        assert!(parse_kimi("{}").is_err());
        assert!(parse_kimi(r#"{"usages":null}"#).is_err());
        assert!(parse_kimi(r#"{"usages":{}}"#).is_err());
        assert!(parse_kimi(r#"{"usages":{"limit_5h":null}}"#).is_err());
    }

    #[test]
    fn kimi_endpoint_names_its_subscription() {
        assert!(matches!(
            EndpointKind::KimiCode {
                url: String::new(),
                token: String::new(),
            }
            .usage_label(),
            Some("Kimi")
        ));
    }

    #[test]
    fn entry_shows_each_reset_with_what_its_window_needs() {
        let usage = Usage {
            windows: vec![
                UsageWindow {
                    span: Span::FiveHours,
                    percent: 22,
                    resets_at: Some(1_791_817_200),
                },
                UsageWindow {
                    span: Span::Week,
                    percent: 18,
                    resets_at: Some(1_792_018_800),
                },
                UsageWindow {
                    span: Span::Month,
                    percent: 91,
                    resets_at: Some(1_794_303_997),
                },
            ],
        };
        // Summer time until the monthly reset, which falls after the change.
        let offset_of = |at: i64| if at < 1_793_000_000 { 7200 } else { 3600 };
        let pieces = usage.pieces("OpenCode", 1_791_800_000, offset_of);
        assert_eq!(
            text(&pieces),
            "│ OpenCode 22% 17:00 · 18% Thu 01:00 · 91% 10th 10:46 "
        );
        let percents: Vec<u32> = pieces.iter().filter_map(|(_, percent)| *percent).collect();
        assert_eq!(percents, [22, 18, 91]);
    }

    #[test]
    fn cline_answer_gives_the_three_windows_shortest_first() {
        let body = r#"{"data":{"limits":[
            {"type":"monthly","percentUsed":12.4,"resetsAt":"2026-12-01T12:43:48Z"},
            {"type":"five_hour","percentUsed":3,"resetsAt":"2026-11-01T17:52:00Z"},
            {"type":"weekly","percentUsed":47.5,"resetsAt":"2026-11-07T00:00:00Z"}
        ]},"success":true}"#;
        let usage = parse_cline(body).expect("valid answer");
        assert_eq!(
            usage.windows,
            [
                UsageWindow {
                    span: Span::FiveHours,
                    percent: 3,
                    resets_at: Some(1_793_555_520)
                },
                UsageWindow {
                    span: Span::Week,
                    percent: 48,
                    resets_at: Some(1_794_009_600)
                },
                UsageWindow {
                    span: Span::Month,
                    percent: 12,
                    resets_at: Some(1_796_129_028)
                },
            ]
        );
    }

    #[test]
    fn cline_window_without_a_reset_time_keeps_its_percent() {
        let body = r#"{"data":{"limits":[
            {"type":"five_hour","percentUsed":0},
            {"type":"weekly","percentUsed":0},
            {"type":"monthly","percentUsed":0}
        ]},"success":true}"#;
        let usage = parse_cline(body).expect("valid answer");
        assert_eq!(usage.windows.len(), 3);
        assert!(
            usage
                .windows
                .iter()
                .all(|window| window.resets_at.is_none())
        );
        let pieces = usage.pieces("Cline", 1_791_800_000, |_| 3600);
        assert_eq!(text(&pieces), "│ Cline 0% · 0% · 0% ");
    }

    #[test]
    fn cline_reset_reads_a_timestamp_or_an_epoch_in_seconds_or_milliseconds() {
        assert_eq!(
            cline_reset(&serde_json::json!("2026-11-01T17:52:00Z")),
            Some(1_793_555_520)
        );
        assert_eq!(
            cline_reset(&serde_json::json!(1_793_555_520)),
            Some(1_793_555_520)
        );
        assert_eq!(
            cline_reset(&serde_json::json!(1_793_555_520_000_i64)),
            Some(1_793_555_520)
        );
        assert_eq!(cline_reset(&serde_json::json!("soon")), None);
        assert_eq!(cline_reset(&serde_json::json!(null)), None);
    }

    #[test]
    fn cline_answer_of_another_shape_is_an_error() {
        assert!(parse_cline(r#"{"code":500,"success":false}"#).is_err());
        assert!(parse_cline(r#"{"data":{"limits":[]}}"#).is_err());
        assert!(parse_cline(r#"{"data":{"limits":[{"type":"hourly","percentUsed":2}]}}"#).is_err());
        assert!(
            parse_cline(
                r#"{"data":{"limits":[{"type":"weekly","percentUsed":2,"resetsAt":"soon"}]}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn a_usage_file_written_before_optional_resets_still_loads() {
        let stored =
            r#"{"cline":{"windows":[{"span":"week","percent":7,"resets_at":1794009600}]}}"#;
        let usage: HashMap<String, Usage> = serde_json::from_str(stored).expect("valid store");
        assert_eq!(usage["cline"].windows[0].resets_at, Some(1_794_009_600));
    }

    #[test]
    fn cline_endpoint_names_its_subscription() {
        assert!(matches!(
            EndpointKind::Cline {
                url: String::new(),
                token: String::new(),
            }
            .usage_label(),
            Some("Cline")
        ));
    }

    #[test]
    fn window_past_its_reset_has_used_nothing() {
        let usage = Usage {
            windows: vec![
                UsageWindow {
                    span: Span::FiveHours,
                    percent: 22,
                    resets_at: Some(1_791_817_200),
                },
                UsageWindow {
                    span: Span::Week,
                    percent: 18,
                    resets_at: Some(1_792_018_800),
                },
            ],
        };
        let pieces = usage.pieces("Claude", 1_791_817_200, |_| 7200);
        assert_eq!(text(&pieces), "│ Claude 0% · 18% Thu 01:00 ");
    }

    #[test]
    fn day_of_the_month_reads_as_an_ordinal() {
        let suffixes: Vec<&str> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 30, 31]
            .into_iter()
            .map(ordinal_suffix)
            .collect();
        assert_eq!(
            suffixes,
            [
                "st", "nd", "rd", "th", "th", "th", "th", "st", "nd", "rd", "th", "st"
            ]
        );
    }
}

#[derive(Deserialize)]
struct ClineReply {
    data: ClineData,
}

#[derive(Deserialize)]
struct ClineData {
    limits: Vec<ClineLimit>,
}

#[derive(Deserialize)]
struct ClineLimit {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "percentUsed")]
    percent_used: f64,
    /// Absent until the window has been spent once; Cline's dashboard reads it as an
    /// ISO timestamp, but the browser's `Date` takes epoch numbers too.
    #[serde(rename = "resetsAt")]
    resets_at: Option<serde_json::Value>,
}

/// Epoch seconds of a Cline reset, which is an ISO timestamp or an epoch number in
/// seconds or milliseconds.
fn cline_reset(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::String(stamp) => utc_secs(stamp),
        serde_json::Value::Number(number) => number.as_i64().map(|epoch| {
            if epoch > 100_000_000_000 {
                epoch / 1000
            } else {
                epoch
            }
        }),
        _ => None,
    }
}

/// The usage in the answer of Cline's `users/me/plan/usage-limits` request. Cline
/// reports a percent per window and no reset time while the window is untouched, so
/// such a window states its percent only.
fn parse_cline(body: &str) -> Result<Usage, String> {
    let reply: ClineReply = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if reply.data.limits.is_empty() {
        return Err("no usage window in answer".to_string());
    }
    let mut windows = Vec::with_capacity(reply.data.limits.len());
    for limit in reply.data.limits {
        let Some(span) = cline_span(&limit.kind) else {
            continue;
        };
        let resets_at = match &limit.resets_at {
            Some(stamp) => {
                Some(cline_reset(stamp).ok_or_else(|| format!("unreadable reset time {stamp}"))?)
            }
            None => None,
        };
        windows.push(UsageWindow {
            span,
            percent: limit.percent_used.round().clamp(0.0, 100.0) as u32,
            resets_at,
        });
    }
    if windows.is_empty() {
        return Err("no known usage window in answer".to_string());
    }
    windows.sort_by_key(|window| match window.span {
        Span::FiveHours => 0,
        Span::Week => 1,
        Span::Month => 2,
    });
    Ok(Usage { windows })
}

fn cline_span(kind: &str) -> Option<Span> {
    match kind {
        "five_hour" => Some(Span::FiveHours),
        "weekly" => Some(Span::Week),
        "monthly" => Some(Span::Month),
        _ => None,
    }
}

/// How much of a Cline Pass is used, from `GET <url>/users/me/plan/usage-limits`.
pub async fn fetch_cline(url: &str, token: &str) -> Result<Usage, String> {
    let url = format!("{}/users/me/plan/usage-limits", url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("{url}: HTTP {}", response.status()));
    }
    let body = response.text().await.map_err(|e| e.to_string())?;
    parse_cline(&body)
}
