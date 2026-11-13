use super::Error;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Report {
    pub primary: String,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    pub id: String,
    pub name: String,
    pub plan: String,
    pub fetched_at: Option<i64>,
    pub stale: bool,
    pub failed: bool,
    pub metrics: Vec<Metric>,
    pub available: Option<f64>,
    pub credits: Vec<Credit>,
    pub blocks: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Metric {
    pub label: String,
    pub percent: Option<f64>,
    pub value: String,
    pub reset_at: Option<i64>,
    pub window_secs: Option<f64>,
    pub severity: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Credit {
    pub title: String,
    pub expires_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Resets {
    pub latest: Option<ResetEvent>,
    pub scheduled: Option<ResetEvent>,
    pub watch: Option<Forecast>,
    pub generated_at: Option<i64>,
    pub total: Option<f64>,
    pub average_days: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResetEvent {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub announced_at: Option<i64>,
    pub scheduled_for: Option<i64>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Forecast {
    pub level: String,
    pub chance: Option<f64>,
    pub window: String,
    pub text: String,
    pub expires_at: Option<i64>,
    pub observed_at: Option<i64>,
    pub url: Option<String>,
}

impl Report {
    /// A pinned account never silently substitutes another account.
    pub(crate) fn select(&self, wanted: &str) -> Option<&Entry> {
        let find = |id: &str| self.entries.iter().find(|entry| entry.id == id);
        if !wanted.is_empty() && wanted != "auto" {
            return find(wanted);
        }
        let primary = if self.primary.is_empty() {
            None
        } else {
            find(&self.primary)
        };
        primary
            .or_else(|| find("openai"))
            .or_else(|| self.entries.iter().find(|entry| !entry.failed))
            .or_else(|| self.entries.first())
    }
}

impl Entry {
    pub(crate) fn headline(&self) -> Option<&Metric> {
        if self.failed {
            return None;
        }
        // An unknown reset is potentially later than every known reset.
        self.metrics
            .iter()
            .filter(|metric| metric.percent.is_some_and(|used| used >= 100.0))
            .max_by_key(|metric| metric.reset_at.unwrap_or(i64::MAX))
            .or_else(|| self.metrics.iter().find(|metric| metric.percent.is_some()))
    }
}

impl Metric {
    pub(crate) fn remaining(&self) -> Option<f64> {
        self.percent
            .filter(|used| used.is_finite())
            .map(|used| (100.0 - used).clamp(0.0, 100.0))
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|n| n.is_finite())
}

fn time(value: &Value) -> Option<i64> {
    timestamp(value.as_str()?)
}

fn items(value: &Value) -> impl Iterator<Item = &Value> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item.is_object())
}

pub(crate) fn parse_report(bytes: &[u8]) -> Result<Report, Error> {
    let raw: Value = serde_json::from_slice(bytes).map_err(|_| Error::Format)?;
    if raw["schema_version"].as_u64() != Some(1) || !raw["entries"].is_array() {
        return Err(Error::Format);
    }
    let entries = items(&raw["entries"])
        .filter_map(|entry| {
            let id = entry["id"].as_str()?.to_owned();
            let metrics = items(&entry["metrics"])
                .map(|metric| Metric {
                    label: text(&metric["label"]),
                    percent: number(&metric["percent"]),
                    value: text(&metric["value"]),
                    reset_at: time(&metric["reset_at"]),
                    window_secs: number(&metric["window_secs"]),
                    severity: text(&metric["severity"]),
                })
                .collect();
            let credits = &entry["reset_credits"];
            // Only the typed balance section is retained. Diagnostics can contain
            // credentials and must never cross into reactive state or logs.
            let blocks = items(&entry["sections"])
                .filter(|section| section["type"] == "block" && section["label"] == "Credits")
                .map(|section| {
                    section["body"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .collect();
            Some(Entry {
                name: entry["display_name"].as_str().unwrap_or(&id).to_owned(),
                id,
                plan: text(&entry["plan"]),
                fetched_at: time(&entry["fetched_at"]),
                stale: entry["stale"].as_bool() == Some(true),
                failed: entry["status"] == "error",
                metrics,
                available: number(&credits["available"]),
                credits: items(&credits["credits"])
                    .map(|credit| Credit {
                        title: text(&credit["title"]),
                        expires_at: time(&credit["expires_at"]),
                    })
                    .collect(),
                blocks,
            })
        })
        .collect();
    Ok(Report {
        primary: text(&raw["primary"]),
        entries,
    })
}

fn source_url(raw: &Value) -> Option<String> {
    let source = raw["source"]["url"].as_str()?;
    if source.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let url = url::Url::parse(source).ok()?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || source
            .split_once("://")?
            .1
            .split(['/', '?', '#'])
            .next()?
            .contains('@')
    {
        return None;
    }
    Some(url.into())
}

fn event(raw: &Value) -> Option<ResetEvent> {
    Some(ResetEvent {
        id: raw["id"].as_str()?.to_owned(),
        kind: text(&raw["reset_type"]),
        text: text(&raw["text"]),
        announced_at: time(&raw["announced_at"]),
        scheduled_for: time(&raw["scheduled_for"]),
        url: source_url(raw),
    })
}

pub(crate) fn parse_resets(bytes: &[u8]) -> Result<Resets, Error> {
    let raw: Value = serde_json::from_slice(bytes).map_err(|_| Error::Format)?;
    let data = &raw["data"];
    if raw["meta"]["api_version"] != "v1" || !data.is_object() || !data["stats"].is_object() {
        return Err(Error::Format);
    }
    let watch = &data["active_watch"];
    Ok(Resets {
        latest: event(&data["latest_reset"]),
        scheduled: event(&data["scheduled_reset"]),
        watch: watch.is_object().then(|| Forecast {
            level: text(&watch["level"]),
            chance: number(&watch["reset_chance_percent"]),
            window: text(&watch["forecast_window"]),
            text: text(&watch["text"]),
            expires_at: time(&watch["expires_at"]),
            observed_at: time(&watch["observed_at"]),
            url: source_url(watch),
        }),
        generated_at: time(&raw["meta"]["generated_at"]),
        total: number(&data["stats"]["total"]),
        average_days: number(&data["stats"]["avg_interval_days"]),
    })
}

/// Strict Gregorian RFC3339 adaptation, independent of the machine timezone.
pub(crate) fn timestamp(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let digits = |start: usize, end: usize| -> Option<i64> {
        bytes.get(start..end)?.iter().try_fold(0i64, |n, b| {
            b.is_ascii_digit().then(|| n * 10 + i64::from(b - b'0'))
        })
    };
    let (mut y, m, d) = (digits(0, 4)?, digits(5, 7)?, digits(8, 10)?);
    let (h, minute, second) = (digits(11, 13)?, digits(14, 16)?, digits(17, 19)?);
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&m)
        || d < 1
        || d > days[(m - 1) as usize]
        || h > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let mut end = 19;
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        let start = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if start == end {
            return None;
        }
    }
    let offset = match bytes.get(end..)? {
        b"Z" => 0,
        tail if tail.len() == 6 && matches!(tail[0], b'+' | b'-') && tail[3] == b':' => {
            let hours = digits(end + 1, end + 3)?;
            let mins = digits(end + 4, end + 6)?;
            if hours > 23 || mins > 59 {
                return None;
            }
            (hours * 3600 + mins * 60) * if tail[0] == b'-' { -1 } else { 1 }
        }
        _ => return None,
    };
    if m <= 2 {
        y -= 1;
    }
    let era = y.div_euclid(400);
    let year = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = year * 365 + year / 4 - year / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400 + h * 3600 + minute * 60 + second - offset)
}

pub(crate) fn now() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(time) => time.as_secs().min(i64::MAX as u64) as i64,
        Err(time) => -(time.duration().as_secs().min(i64::MAX as u64) as i64),
    }
}

pub(crate) fn date(seconds: i64) -> String {
    let days = seconds.div_euclid(86400) + 719468;
    let era = days.div_euclid(146097);
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    let clock = seconds.rem_euclid(86400);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        clock / 3600,
        clock / 60 % 60
    )
}
