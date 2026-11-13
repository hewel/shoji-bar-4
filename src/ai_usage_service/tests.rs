use super::model::timestamp;
use super::*;
use serde_json::{Value, json};

fn report_value() -> Value {
    json!({
        "schema_version": 1, "primary": "openai@work", "entries": [
            {"id":"openai", "display_name":"Personal", "status":"ready", "metrics":[
                {"label":"Session", "percent":24, "reset_at":"2030-01-01T02:00:00Z"},
                {"label":"Weekly", "percent":100, "reset_at":"2030-01-06T00:00:00Z"}
            ]},
            {"id":"openai@work", "display_name":"Work", "status":"ready", "metrics":[
                {"label":"Session", "percent":0}
            ], "reset_credits":{"available":2,"credits":[
                {"title":"Full reset","expires_at":"2030-02-01T00:00:00Z"}
            ]}}
        ]
    })
}

fn parse(value: &Value) -> Report {
    parse_report(&serde_json::to_vec(value).unwrap()).unwrap()
}

#[test]
fn pinned_accounts_never_substitute_and_auto_uses_documented_priority() {
    let mut report = parse(&report_value());
    assert_eq!(report.select("openai@missing"), None);
    assert_eq!(report.select("auto").unwrap().id, "openai@work");
    report.primary = "missing".into();
    report.entries.reverse();
    assert_eq!(report.select("auto").unwrap().id, "openai");
    report.entries.retain(|entry| entry.id != "openai");
    assert_eq!(report.select("").unwrap().id, "openai@work");
    report.entries[0].failed = true;
    assert!(report.select("auto").unwrap().failed);
    assert_eq!(report.select("auto").unwrap().headline(), None);
}

#[test]
fn exhaustion_overrides_short_windows_and_unknown_resets_are_conservative() {
    let mut report = parse(&report_value());
    let account = &mut report.entries[0];
    assert_eq!(account.headline().unwrap().label, "Weekly");
    assert_eq!(account.headline().unwrap().remaining(), Some(0.0));
    account.metrics[1].percent = Some(96.0);
    account.metrics[1].severity = "critical".into();
    assert_eq!(account.headline().unwrap().label, "Session");
    account.metrics[0].percent = Some(100.0);
    account.metrics[1].percent = Some(100.0);
    assert_eq!(account.headline().unwrap().label, "Weekly");
    account.metrics[0].reset_at = None;
    assert_eq!(account.headline().unwrap().label, "Session");
    account.failed = true;
    assert_eq!(account.headline(), None);
}

#[test]
fn unknown_and_nonfinite_usage_never_fabricate_available_quota() {
    let mut value = report_value();
    value["entries"][0]["metrics"] = json!([
        {"label":"Missing"}, {"label":"Malformed", "percent":"not-a-number"},
        {"label":"Nonfinite", "percent":"NaN"}, {"label":"Infinite", "percent":"inf"},
        {"label":"Overrun", "percent":"120"}, {"label":"Negative", "percent":-5}
    ]);
    let report = parse(&value);
    let metrics = &report.entries[0].metrics;
    for metric in &metrics[..4] {
        assert_eq!(metric.remaining(), None);
    }
    assert_eq!(metrics[4].remaining(), Some(0.0));
    assert_eq!(metrics[5].remaining(), Some(100.0));
    assert_eq!(
        report.entries[1].headline().unwrap().remaining(),
        Some(100.0)
    );
}

#[test]
fn diagnostic_sections_are_dropped_but_typed_credits_and_balances_survive() {
    let mut value = report_value();
    value["entries"][0]["error"] = json!("private-diagnostic");
    value["entries"][0]["sections"] = json!([
        {"type":"text", "label":"Warning", "value":"private-diagnostic"},
        {"type":"block", "label":"Diagnostics", "body":["private-diagnostic"]},
        {"type":"block", "label":"Credits", "body":["Balance: 4", "Expires soon", 123]}
    ]);
    let report = parse(&value);
    assert!(!format!("{report:?}").contains("private-diagnostic"));
    assert_eq!(report.entries[0].blocks, vec!["Balance: 4\nExpires soon"]);
    assert_eq!(report.entries[1].available, Some(2.0));
    assert_eq!(
        report.entries[1].credits[0].expires_at,
        timestamp("2030-02-01T00:00:00Z")
    );
}

#[test]
fn unsupported_envelopes_fail_instead_of_replacing_a_snapshot_with_empty_data() {
    for value in [
        json!(null),
        json!([]),
        json!({"schema_version":2,"entries":[]}),
        json!({"schema_version":1,"entries":{}}),
    ] {
        assert_eq!(
            parse_report(&serde_json::to_vec(&value).unwrap()),
            Err(Error::Format)
        );
    }
    assert_eq!(parse_report(b"{"), Err(Error::Format));
    for value in [
        json!({}),
        json!({"meta":{"api_version":"v2"},"data":{"stats":{}}}),
        json!({"meta":{"api_version":"v1"},"data":{"stats":[]}}),
    ] {
        assert_eq!(
            parse_resets(&serde_json::to_vec(&value).unwrap()),
            Err(Error::Format)
        );
    }
    assert!(
        parse_report(br#"{"schema_version":1,"entries":[]}"#)
            .unwrap()
            .entries
            .is_empty()
    );
}

#[test]
fn rfc3339_offsets_fractional_seconds_and_calendar_boundaries_are_strict() {
    assert_eq!(timestamp("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(timestamp("1969-12-31T23:59:59Z"), Some(-1));
    let expected = timestamp("2030-01-01T00:00:00Z");
    assert_eq!(timestamp("2030-01-01T08:30:00.123456789+08:30"), expected);
    assert_eq!(timestamp("2029-12-31T19:00:00-05:00"), expected);
    assert_eq!(timestamp("2024-02-29T00:00:00Z"), Some(1709164800));
    for invalid in [
        "2025-02-29T00:00:00Z",
        "1900-02-29T00:00:00Z",
        "2030-04-31T00:00:00Z",
        "2030-00-01T00:00:00Z",
        "2030-01-00T00:00:00Z",
        "2030-01-01T25:00:00Z",
        "2030-01-01T00:60:00Z",
        "2030-01-01T00:00:60Z",
        "2030-01-01T00:00:00",
        "2030-01-01T00:00:00+24:00",
        "2030-01-01T00:00:00+01:60",
        "2030-01-01T00:00:00.Z",
        "2030-01-01T00:00:00Zjunk",
        "2030-01-01T00:00:00.1",
        "２０３０-01-01T00:00:00Z",
    ] {
        assert_eq!(timestamp(invalid), None, "{invalid}");
    }
    assert_eq!(date(-1), "1969-12-31 23:59 UTC");
    for civil in [
        "0000-02-29T00:00:00Z",
        "2000-02-29T00:00:00Z",
        "2030-12-31T23:59:00Z",
    ] {
        let formatted = date(timestamp(civil).unwrap());
        assert_eq!(&formatted[..16], &civil[..16].replace('T', " "));
    }
}

fn feed(source: &str) -> Value {
    json!({"meta":{"api_version":"v1","generated_at":"2030-01-01T00:00:00Z"},
        "data":{"stats":{"total":12,"avg_interval_days":7.5},
        "latest_reset":{"id":"completed","reset_type":"banked","announced_at":"2029-12-25T00:00:00Z","source":{"url":source}},
        "scheduled_reset":{"id":"announcement","scheduled_for":"2030-01-02T00:00:00Z"},
        "active_watch":{"level":"high","reset_chance_percent":80,"forecast_window":"soon","expires_at":"2030-01-01T12:00:00Z","source":{"url":source}}}})
}

#[test]
fn public_announcements_and_forecasts_remain_distinct_typed_data() {
    let value = feed("https://example.test/source");
    let resets = parse_resets(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(resets.latest.as_ref().unwrap().id, "completed");
    let scheduled = resets.scheduled.as_ref().unwrap();
    assert_eq!(scheduled.id, "announcement");
    assert!(scheduled.scheduled_for.unwrap() < timestamp("2030-01-03T00:00:00Z").unwrap());
    assert_eq!(scheduled.announced_at, None);
    assert_eq!(resets.watch.as_ref().unwrap().chance, Some(80.0));
    assert_eq!(
        resets.watch.as_ref().unwrap().expires_at,
        timestamp("2030-01-01T12:00:00Z")
    );
    assert_eq!(resets.average_days, Some(7.5));
}

#[test]
fn public_source_links_reject_userinfo_non_https_and_control_characters() {
    for source in [
        "javascript:alert(1)",
        "http://example.test/source",
        "https://user:secret@example.test/source",
        "https://@example.test/source",
        "https://example.test/source\ncommand",
        "https://example.test/a b",
        "https:///",
        "https://example.test/\u{0}",
    ] {
        let resets = parse_resets(&serde_json::to_vec(&feed(source)).unwrap()).unwrap();
        assert_eq!(resets.latest.unwrap().url, None, "{source:?}");
        assert_eq!(resets.watch.unwrap().url, None, "{source:?}");
    }
    let resets =
        parse_resets(&serde_json::to_vec(&feed("https://example.test/source?q=1#event")).unwrap())
            .unwrap();
    assert_eq!(
        resets.latest.unwrap().url.as_deref(),
        Some("https://example.test/source?q=1#event")
    );
}

#[test]
fn malformed_config_fields_cannot_erase_explicit_disabled_opt_outs() {
    let config = AiUsageConfig::from_json(&json!({
        "enabled":false, "resets_enabled":false, "refresh_minutes":"bad",
        "resets_refresh_minutes":0, "show_countdown":false, "provider":"openai@missing"
    }));
    assert!(!config.enabled);
    assert!(!config.resets_enabled);
    assert!(!config.show_countdown);
    assert_eq!(config.refresh_minutes, 5);
    assert_eq!(config.resets_refresh_minutes, 5);
    assert_eq!(config.provider, "openai@missing");
    assert_eq!(
        AiUsageConfig::from_json(&json!(null)),
        AiUsageConfig::default()
    );
    let config = AiUsageConfig {
        refresh_minutes: u64::MAX,
        resets_refresh_minutes: u64::MAX,
        ..config
    }
    .normalized();
    assert_eq!(config.refresh_minutes, 120);
    assert_eq!(config.resets_refresh_minutes, 120);
    assert!(!config.resets_enabled);
    let encoded = serde_json::to_value(&config).unwrap();
    assert_eq!(AiUsageConfig::from_json(&encoded), config);
}
