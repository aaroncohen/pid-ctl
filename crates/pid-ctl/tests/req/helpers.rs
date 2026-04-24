//! Shared helpers for `pid-ctl` requirement tests.

use serde_json::Value;

/// Asserts `value["ts"]` is present and matches ISO 8601 UTC with second precision (`YYYY-MM-DDTHH:MM:SSZ`),
/// consistent with [`pid_ctl::app::now_iso8601`].
pub(crate) fn assert_json_ts_iso8601_utc(value: &Value) {
    let ts = value["ts"].as_str().expect("ts field should be a string");
    assert_eq!(
        ts.len(),
        20,
        "ts should be 20 chars (YYYY-MM-DDTHH:MM:SSZ), got {ts:?}"
    );
    assert!(ts.ends_with('Z'), "ts should use UTC suffix Z, got {ts:?}");
    let bytes = ts.as_bytes();
    assert_eq!(bytes[4], b'-');
    assert_eq!(bytes[7], b'-');
    assert_eq!(bytes[10], b'T');
    assert_eq!(bytes[13], b':');
    assert_eq!(bytes[16], b':');
}
