use std::collections::HashMap;

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// The stretch of time one limit of a subscription covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Span {
    FiveHours,
    Week,
    Month,
}

/// How much of one limit is used, and when it starts over.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UsageWindow {
    pub span: Span,
    pub percent: u32,
    /// Epoch seconds; `None` when the provider reports no reset for this window.
    pub resets_at: Option<i64>,
}

/// What a subscription reports of its limits, shortest window first.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub windows: Vec<UsageWindow>,
    /// What an OpenRouter key reports of its own spending; `None` for a subscription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<KeyMoney>,
}

/// What an OpenRouter key reports of its own spending. Money is in micro-dollars
/// (10⁻⁶ USD), the finest unit the API reports, kept integral.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeyMoney {
    /// Total credit spent on the key, whatever the window of its limit.
    pub spent: u64,
    /// The spending limit the key enforces, `None` for a key without one.
    pub limit: Option<u64>,
    /// What is left of the limit. Never shown: it only colors the limit by how much
    /// of it is spent.
    pub remaining: Option<u64>,
    /// `daily`, `weekly` or `monthly`, `None` for a limit that never resets. A key
    /// whose limit was removed keeps reporting one, so this never stands alone.
    pub resets: Option<String>,
    /// What is left of the account's credit; shown instead of the limit when the key
    /// has none, which is the only case that fetches it.
    pub credit: Option<Credit>,
}

/// What is left of the account's purchased credit: `left` of `total`, the purchases
/// that paid for it. Micro-dollars like [`KeyMoney`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Credit {
    pub left: u64,
    pub total: u64,
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
        let time = crate::util::time::local_hhmm(resets_at, offset_of(resets_at));
        let reset = match self.span {
            Span::FiveHours => time,
            Span::Week => {
                // Day 0 of the epoch is a Thursday.
                let weekday = WEEKDAYS[(days + 4).rem_euclid(7) as usize];
                format!("{weekday} {time}")
            }
            Span::Month => {
                let (_, _, day) = crate::util::time::civil_from_days(days);
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
    /// The status-bar entry for the spending behind `provider`'s endpoint named
    /// `label`; empty when there is nothing to state yet. A key states money and its
    /// limit, a subscription the percents and reset times of its windows, and `None` —
    /// the `claude` CLI, which reports its own windows — states those windows.
    pub fn entry(
        &self,
        provider: Option<crate::endpoint::Provider>,
        label: &str,
        now: i64,
        offset_of: impl Fn(i64) -> i32,
    ) -> Vec<(String, Option<u32>)> {
        use crate::endpoint::Provider;
        match provider {
            Some(Provider::OpenRouter) => self.money_pieces(label).unwrap_or_default(),
            _ => self.pieces(label, now, offset_of),
        }
    }

    /// The status-bar entry of the subscription named `label`, as the pieces of
    /// `label 22% 17:00 · 18% Thu 01:00`. A piece that states a percent carries it,
    /// for its color. `offset_of` gives the local offset at an instant.
    fn pieces(
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

    /// The status-bar entry of the OpenRouter key named `label`, as the pieces of
    /// `label $0.50 · $1.00 daily`: the credit the key has spent, then its limit and
    /// the window that limit counts over. The limit carries the percent of it spent,
    /// for its color; a key without a limit states what is left of the account's
    /// credit instead, and one without a credit states what it has spent alone. `None`
    /// for a usage that states no key, which is every subscription, and for a key that
    /// has reported no money yet.
    fn money_pieces(&self, label: &str) -> Option<Vec<(String, Option<u32>)>> {
        let key = self.key.as_ref()?;
        let mut pieces = Vec::with_capacity(5);
        pieces.push((format!("│ {label}"), None));
        pieces.push((format!(" {}", money(key.spent)), None));
        if let Some(limit) = key.limit {
            let percent = key
                .remaining
                .map(|remaining| percent_spent(remaining, limit));
            pieces.push((format!(" · {}", money(limit)), percent));
            if let Some(resets) = &key.resets {
                pieces.push((format!(" {resets}"), None));
            }
        } else if let Some(credit) = key.credit {
            pieces.push((
                format!(" · {}", money(credit.left)),
                Some(percent_spent(credit.left, credit.total)),
            ));
            pieces.push((" left".to_string(), None));
        }
        pieces.push((" ".to_string(), None));
        Some(pieces)
    }
}

/// `$1.23`. The status bar states money to the cent: a cheap turn is a fraction of
/// one, and the exact figure is one request away.
fn money(micros: u64) -> String {
    format!("${}.{:02}", micros / 1_000_000, micros % 1_000_000 / 10_000)
}

/// The percent of `limit` spent, given what is left of it. A limit of nothing counts
/// as nothing spent.
fn percent_spent(remaining: u64, limit: u64) -> u32 {
    let spent = limit.saturating_sub(remaining.min(limit));
    (spent.saturating_mul(100) / limit.max(1)).min(100) as u32
}

/// USD as the micro-dollars the counters keep, rounded and never negative.
fn micros(usd: f64) -> u64 {
    if !usd.is_finite() || usd <= 0.0 {
        return 0;
    }
    (usd * 1_000_000.0).round() as u64
}

#[derive(serde::Deserialize)]
struct GoReply {
    usage: GoUsage,
}

#[derive(serde::Deserialize)]
struct GoUsage {
    rolling: Option<GoWindow>,
    weekly: Option<GoWindow>,
    monthly: Option<GoWindow>,
}

#[derive(serde::Deserialize)]
struct GoWindow {
    percent: u32,
    #[serde(rename = "resetsAt")]
    resets_at: String,
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
        let resets_at = crate::util::time::parse_utc_stamp(&window.resets_at)
            .ok_or_else(|| format!("unreadable reset time '{}'", window.resets_at))?;
        windows.push(UsageWindow {
            span,
            percent: window.percent,
            resets_at: Some(resets_at),
        });
    }
    Ok(Usage { windows, key: None })
}

#[derive(serde::Deserialize)]
struct KimiReply {
    usages: KimiUsages,
}

#[derive(serde::Deserialize, Default)]
struct KimiUsages {
    limit_5h: Option<KimiWindow>,
    limit_7d: Option<KimiWindow>,
    limit_month_total: Option<KimiWindow>,
}

#[derive(serde::Deserialize)]
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
        let resets_at = crate::util::time::parse_utc_stamp(&window.reset_time)
            .ok_or_else(|| format!("unreadable reset time '{}'", window.reset_time))?;
        windows.push(UsageWindow {
            span,
            percent: (window.used_ratio * 100.0).round().clamp(0.0, 100.0) as u32,
            resets_at: Some(resets_at),
        });
    }
    Ok(Usage { windows, key: None })
}

/// `<data_dir>/usage.json`: a subscription is the same in every repository, so its
/// last known usage is not part of a repository's session.
fn store_path() -> Option<camino::Utf8PathBuf> {
    Some(crate::util::fs::data_dir()?.join("usage.json"))
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
            crate::util::fs::write_atomic(&path, &content)
        });
    if let Err(e) = written {
        tracing::warn!("failed to write {path}: {e}");
    }
}

#[derive(serde::Deserialize)]
struct ClineReply {
    data: ClineData,
}

#[derive(serde::Deserialize)]
struct ClineData {
    limits: Vec<ClineLimit>,
}

#[derive(serde::Deserialize)]
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
        serde_json::Value::String(stamp) => crate::util::time::parse_utc_stamp(stamp),
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
    Ok(Usage { windows, key: None })
}

fn cline_span(kind: &str) -> Option<Span> {
    match kind {
        "five_hour" => Some(Span::FiveHours),
        "weekly" => Some(Span::Week),
        "monthly" => Some(Span::Month),
        _ => None,
    }
}

/// What a subscription of `provider` reports of its spending, or why the request it
/// answers failed; `Ok(None)` for a provider that reports none, which is not a failure.
pub async fn fetch(
    provider: crate::endpoint::Provider,
    url: &str,
    token: &str,
) -> Result<Option<Usage>, String> {
    use crate::endpoint::Provider;
    let path = match provider {
        Provider::OpenCodeGo => "usage",
        Provider::KimiCode => "usages",
        Provider::Cline => "users/me/plan/usage-limits",
        Provider::OpenRouter => return fetch_openrouter(url, token).await.map(Some),
        Provider::OpenAi => return Ok(None),
    };
    let url = format!("{}/{}", crate::util::http::base_url(url), path);
    let body = crate::util::http::get(&url, token).await?;
    let usage = match provider {
        Provider::KimiCode => parse_kimi(&body),
        Provider::Cline => parse_cline(&body),
        _ => parse_go(&body),
    };
    usage.map(Some)
}

/// Whether the spending behind `provider` has to be polled rather than read when a turn
/// ends: an OpenRouter key's counter settles seconds after the requests that spent it.
pub fn polls_the_key(provider: crate::endpoint::Provider) -> bool {
    use crate::endpoint::Provider;
    provider == Provider::OpenRouter
}

#[derive(serde::Deserialize)]
struct OpenRouterReply {
    data: OpenRouterKey,
}

#[derive(serde::Deserialize)]
struct OpenRouterKey {
    usage: f64,
    limit: Option<f64>,
    #[serde(rename = "limit_remaining")]
    limit_remaining: Option<f64>,
    #[serde(rename = "limit_reset")]
    limit_reset: Option<String>,
}

#[derive(serde::Deserialize)]
struct CreditsReply {
    data: Credits,
}

#[derive(serde::Deserialize)]
struct Credits {
    total_credits: f64,
    total_usage: f64,
}

/// What an OpenRouter key reports of its own spending, from the answer of its `key`
/// request: what it has cost, and the limit it enforces.
fn parse_openrouter(body: &str) -> Result<Usage, String> {
    let reply: OpenRouterReply = serde_json::from_str(body).map_err(|e| e.to_string())?;
    Ok(Usage {
        windows: Vec::new(),
        key: Some(KeyMoney {
            spent: micros(reply.data.usage),
            limit: reply.data.limit.map(micros),
            remaining: reply.data.limit_remaining.map(micros),
            resets: reply.data.limit_reset,
            credit: None,
        }),
    })
}

/// What is left of the account's purchased credit, from the answer of its `credits`
/// request.
fn parse_credit(body: &str) -> Result<Credit, String> {
    let reply: CreditsReply = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let total = micros(reply.data.total_credits);
    Ok(Credit {
        left: total.saturating_sub(micros(reply.data.total_usage)),
        total,
    })
}

/// What an OpenRouter key has cost and where its limit stands, from `GET <url>/key`.
/// The plain inference key answers this; no management key is needed. A key without a
/// limit also gets what is left of the account's credit, from `GET <url>/credits`; a
/// failure of that second request is only logged.
async fn fetch_openrouter(url: &str, token: &str) -> Result<Usage, String> {
    let base = crate::util::http::base_url(url);
    let mut usage =
        parse_openrouter(&crate::util::http::get(&format!("{base}/key"), token).await?)?;
    if usage.key.as_ref().is_some_and(|key| key.limit.is_none()) {
        let credit = crate::util::http::get(&format!("{base}/credits"), token)
            .await
            .and_then(|body| parse_credit(&body))
            .map_err(|e| tracing::warn!("credits of '{base}' failed: {e}"));
        if let (Ok(credit), Some(key)) = (credit, usage.key.as_mut()) {
            key.credit = Some(credit);
        }
    }
    Ok(usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::Provider;

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
            key: None,
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
            key: None,
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

    #[test]
    fn openrouter_answer_gives_the_key_money() {
        let body = r#"{"data":{"label":"sk-or-v1-483...f01","is_management_key":false,"is_free_tier":false,"usage":0.00072304,"usage_daily":0.00072304,"limit":1,"limit_remaining":0.99927696,"limit_reset":"daily","expires_at":null}}"#;
        let usage = parse_openrouter(body).expect("valid answer");
        assert_eq!(
            usage.key,
            Some(KeyMoney {
                spent: 723,
                limit: Some(1_000_000),
                remaining: Some(999_277),
                resets: Some("daily".to_string()),
                credit: None,
            })
        );
        assert!(usage.windows.is_empty());
    }

    #[test]
    fn openrouter_answer_of_a_key_without_a_limit_keeps_its_window_word() {
        // A key whose limit was removed: `limit_reset` stays, so the entry is keyed on
        // the limit alone and shows the spend without the account credit it never got.
        let body = r#"{"data":{"label":"sk-or-v1-483...f01","limit":null,"limit_reset":"daily","limit_remaining":null,"usage":0.00072304,"budgets":[],"effective_budget":null}}"#;
        let usage = parse_openrouter(body).expect("valid answer");
        let key = usage.key.as_ref().expect("key money");
        assert_eq!(key.limit, None);
        assert_eq!(key.resets.as_deref(), Some("daily"));
        assert_eq!(
            text(&usage.money_pieces("OpenRouter").expect("a key")),
            "│ OpenRouter $0.00 "
        );
    }

    #[test]
    fn openrouter_credits_state_what_is_left_of_the_account() {
        let body = r#"{"data":{"total_credits":170,"total_usage":150.686801757}}"#;
        let credit = parse_credit(body).expect("valid answer");
        assert_eq!(
            credit,
            Credit {
                left: 19_313_198,
                total: 170_000_000,
            }
        );
    }

    #[test]
    fn openrouter_credits_of_another_shape_is_an_error() {
        assert!(parse_credit(r#"{"data":{"total_credits":170}}"#).is_err());
        assert!(parse_credit(r#"{"code":500}"#).is_err());
    }

    #[test]
    fn openrouter_answer_of_another_shape_is_an_error() {
        assert!(parse_openrouter(r#"{"error":{"message":"User not found.","code":401}}"#).is_err());
        assert!(parse_openrouter(r#"{"data":{"limit":1}}"#).is_err());
    }

    #[test]
    fn openrouter_entry_shows_the_spend_and_the_limit() {
        let usage = Usage {
            windows: Vec::new(),
            key: Some(KeyMoney {
                spent: 500_000,
                limit: Some(1_000_000),
                remaining: Some(500_000),
                resets: Some("daily".to_string()),
                credit: None,
            }),
        };
        let pieces = usage.money_pieces("OpenRouter").expect("a key");
        assert_eq!(text(&pieces), "│ OpenRouter $0.50 · $1.00 daily ");
        // Only the limit carries a percent, for its color.
        let percents: Vec<Option<u32>> = pieces.iter().map(|(_, percent)| *percent).collect();
        assert_eq!(percents, [None, None, Some(50), None, None]);
    }

    #[test]
    fn a_key_without_a_limit_states_what_is_left_of_the_account_credit() {
        let usage = Usage {
            windows: Vec::new(),
            key: Some(KeyMoney {
                spent: 723,
                limit: None,
                remaining: None,
                resets: Some("daily".to_string()),
                credit: Some(Credit {
                    left: 19_313_198,
                    total: 170_000_000,
                }),
            }),
        };
        let pieces = usage.money_pieces("OpenRouter").expect("a key");
        assert_eq!(text(&pieces), "│ OpenRouter $0.00 · $19.31 left ");
        // The credit carries the percent of the purchased credit spent, for its color.
        assert_eq!(pieces[2].1, Some(88));
    }

    #[test]
    fn a_limit_wins_over_the_account_credit() {
        let usage = Usage {
            windows: Vec::new(),
            key: Some(KeyMoney {
                spent: 500_000,
                limit: Some(1_000_000),
                remaining: Some(500_000),
                resets: Some("daily".to_string()),
                credit: Some(Credit {
                    left: 19_313_198,
                    total: 170_000_000,
                }),
            }),
        };
        assert_eq!(
            text(&usage.money_pieces("OpenRouter").expect("a key")),
            "│ OpenRouter $0.50 · $1.00 daily "
        );
    }

    #[test]
    fn openrouter_entry_without_a_limit_shows_the_spend_alone() {
        let usage = Usage {
            windows: Vec::new(),
            key: Some(KeyMoney {
                spent: 12_345_678,
                limit: None,
                remaining: None,
                resets: None,
                credit: None,
            }),
        };
        assert_eq!(
            text(&usage.money_pieces("OpenRouter").expect("a key")),
            "│ OpenRouter $12.34 "
        );
    }

    #[test]
    fn openrouter_limit_that_never_resets_stays_unnamed() {
        let usage = Usage {
            windows: Vec::new(),
            key: Some(KeyMoney {
                spent: 500_000,
                limit: Some(1_000_000),
                remaining: Some(250_000),
                resets: None,
                credit: None,
            }),
        };
        let pieces = usage.money_pieces("OpenRouter").expect("a key");
        assert_eq!(text(&pieces), "│ OpenRouter $0.50 · $1.00 ");
        assert_eq!(pieces[2].1, Some(75));
    }

    #[test]
    fn the_percent_of_a_limit_spent_comes_from_what_is_left_of_it() {
        assert_eq!(percent_spent(250_000, 1_000_000), 75);
        assert_eq!(percent_spent(1_000_000, 1_000_000), 0);
        assert_eq!(percent_spent(0, 1_000_000), 100);
        assert_eq!(percent_spent(0, 0), 0);
    }

    #[test]
    fn a_usage_file_written_before_the_key_money_still_loads() {
        let stored = r#"{"openrouter":{"windows":[]}}"#;
        let usage: HashMap<String, Usage> = serde_json::from_str(stored).expect("valid store");
        assert_eq!(usage["openrouter"].key, None);
        // Nothing to state: the entry names no money and no window, so it stays empty
        // rather than leaving a bare label in the status bar.
        assert!(
            usage["openrouter"]
                .entry(Some(Provider::OpenRouter), "OpenRouter", 0, |_| 0)
                .is_empty()
        );
    }

    #[test]
    fn a_key_states_money_where_a_subscription_states_its_windows() {
        let usage = Usage {
            windows: vec![UsageWindow {
                span: Span::FiveHours,
                percent: 22,
                resets_at: None,
            }],
            key: Some(KeyMoney {
                spent: 500_000,
                limit: Some(1_000_000),
                remaining: Some(250_000),
                resets: Some("daily".to_string()),
                credit: None,
            }),
        };
        assert_eq!(
            text(&usage.entry(None, "Claude", 0, |_| 0)),
            "│ Claude 22% "
        );
        assert_eq!(
            text(&usage.entry(Some(Provider::OpenAi), "Server", 0, |_| 0)),
            "│ Server 22% "
        );
        assert_eq!(
            text(&usage.entry(Some(Provider::OpenRouter), "OpenRouter", 0, |_| 0)),
            "│ OpenRouter $0.50 · $1.00 daily "
        );
    }
}
