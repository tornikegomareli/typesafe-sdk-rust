//! Retry defaults, delay calculation, and cancellable waits.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use reqwest::header::HeaderMap;
use tokio_util::sync::CancellationToken;

use crate::errors::{Error, Result};

pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;

/// Retry configuration. Partial overrides inherit unset fields from the client or SDK defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct RetryPolicy {
    /// Maximum retries after the initial attempt; `0` disables retries. Default: 2.
    pub max_retries: u32,
    /// First backoff delay in milliseconds, doubled up to `backoff_max_ms`. Default: 500.
    pub backoff_initial_ms: f64,
    /// Maximum backoff delay in milliseconds. Default: 5000.
    pub backoff_max_ms: f64,
    /// Fraction of each backoff delay randomly subtracted, from 0 to 1. Default: 0.25.
    pub backoff_jitter: f64,
    /// HTTP status codes to retry. Default: 408, 429, and 500–599.
    pub http_statuses: BTreeSet<u16>,
    /// Honor `Retry-After` and `retry-after-ms` up to `max_retry_after_ms`. Default: true.
    pub respect_retry_after: bool,
    /// Maximum server retry delay in milliseconds; longer delays use backoff. Default: 60000.
    pub max_retry_after_ms: f64,
    /// Retry connection failures, including interrupted response bodies. Default: true.
    pub api_connection_error: bool,
    /// Whether to retry a timeout. Default: true.
    pub api_timeout_error: bool,
}

impl Default for RetryPolicy {
    /// Default SDK retry policy.
    fn default() -> Self {
        RetryPolicy {
            max_retries: 2,
            backoff_initial_ms: 500.0,
            backoff_max_ms: 5_000.0,
            backoff_jitter: 0.25,
            http_statuses: [408, 429].into_iter().chain(500..600).collect(),
            respect_retry_after: true,
            max_retry_after_ms: 60_000.0,
            api_connection_error: true,
            api_timeout_error: true,
        }
    }
}

/// Overrides for a `RetryPolicy`. A field that is `None` inherits the value of the base policy.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RetryOverrides {
    pub max_retries: Option<u32>,
    pub backoff_initial_ms: Option<f64>,
    pub backoff_max_ms: Option<f64>,
    pub backoff_jitter: Option<f64>,
    pub http_statuses: Option<BTreeSet<u16>>,
    pub respect_retry_after: Option<bool>,
    pub max_retry_after_ms: Option<f64>,
    pub api_connection_error: Option<bool>,
    pub api_timeout_error: Option<bool>,
}

fn non_negative_ms(name: &str, value: f64) -> Result<f64> {
    if !value.is_finite() || value < 0.0 {
        return Err(Error::TypeSafe(format!(
            "`{name}` must be a non-negative number of milliseconds, got {value}."
        )));
    }
    Ok(value)
}

impl RetryPolicy {
    /// Merge and validate retry overrides.
    pub(crate) fn resolve(&self, overrides: Option<&RetryOverrides>) -> Result<RetryPolicy> {
        let Some(o) = overrides else { return Ok(self.clone()) };
        let ms = |name: &str, value: Option<f64>, base: f64| value.map_or(Ok(base), |value| non_negative_ms(name, value));

        let backoff_jitter = match o.backoff_jitter {
            Some(value) if !value.is_finite() || !(0.0..=1.0).contains(&value) => {
                return Err(Error::TypeSafe(format!(
                    "`retry.backoffJitter` must be between 0 and 1, got {value}."
                )));
            }
            Some(value) => value,
            None => self.backoff_jitter,
        };
        if let Some(status) = o.http_statuses.iter().flatten().find(|status| !(100..=999).contains(*status)) {
            return Err(Error::TypeSafe(format!(
                "`retry.httpStatuses` must contain HTTP status codes, got {status}."
            )));
        }
        Ok(RetryPolicy {
            max_retries: o.max_retries.unwrap_or(self.max_retries),
            backoff_initial_ms: ms("retry.backoffInitialMs", o.backoff_initial_ms, self.backoff_initial_ms)?,
            backoff_max_ms: ms("retry.backoffMaxMs", o.backoff_max_ms, self.backoff_max_ms)?,
            backoff_jitter,
            http_statuses: o.http_statuses.clone().unwrap_or_else(|| self.http_statuses.clone()),
            respect_retry_after: o.respect_retry_after.unwrap_or(self.respect_retry_after),
            max_retry_after_ms: ms("retry.maxRetryAfterMs", o.max_retry_after_ms, self.max_retry_after_ms)?,
            api_connection_error: o.api_connection_error.unwrap_or(self.api_connection_error),
            api_timeout_error: o.api_timeout_error.unwrap_or(self.api_timeout_error),
        })
    }

    /// Whether the policy retries an HTTP status code.
    pub fn is_retryable_status(&self, status: u16) -> bool {
        self.http_statuses.contains(&status)
    }

    /// Whether the policy retries a connection error or timeout.
    pub(crate) fn is_retryable_error(&self, error: &Error) -> bool {
        match error {
            Error::Timeout { .. } => self.api_timeout_error,
            Error::Connection { .. } => self.api_connection_error,
            _ => false,
        }
    }
}

/// A header value as a number. An empty value is zero, as in JavaScript.
fn number(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Some(0.0);
    }
    trimmed.parse::<f64>().ok().filter(|number| number.is_finite())
}

/// Parse `retry-after-ms` or `Retry-After` into milliseconds, preferring `retry-after-ms`.
///
/// Return `None` when neither header contains a valid delay. `now` is for tests.
pub fn parse_retry_after(headers: &HeaderMap, now: Option<SystemTime>) -> Option<u64> {
    let text = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());

    if let Some(ms) = text("retry-after-ms").and_then(number).filter(|ms| *ms >= 0.0) {
        return Some(ms.round() as u64);
    }
    let raw = text("retry-after")?;
    if let Some(seconds) = number(raw) {
        return (seconds >= 0.0).then(|| (seconds * 1000.0).round() as u64);
    }
    let date = httpdate::parse_http_date(raw.trim()).ok()?;
    let now = now.unwrap_or_else(SystemTime::now);
    Some(date.duration_since(now).map_or(0, |delay| delay.as_millis() as u64))
}

/// Calculate the delay in milliseconds for a zero-based retry attempt.
///
/// Use an allowed server delay; otherwise use capped exponential backoff with jitter.
/// `random` returns a number from 0 to 1.
pub fn retry_delay_ms(attempt: u32, headers: Option<&HeaderMap>, policy: &RetryPolicy, random: impl FnOnce() -> f64) -> u64 {
    if policy.respect_retry_after {
        if let Some(retry_after) = headers.and_then(|headers| parse_retry_after(headers, None)) {
            if retry_after as f64 <= policy.max_retry_after_ms {
                return retry_after;
            }
        }
    }
    let exponential = (policy.backoff_initial_ms * 2f64.powi(attempt as i32)).min(policy.backoff_max_ms);
    (exponential * (1.0 - random() * policy.backoff_jitter)).round() as u64
}

/// Wait `ms` milliseconds. Returns `Error::UserAbort` on cancellation.
pub(crate) async fn sleep(ms: u64, signal: Option<&CancellationToken>) -> Result<()> {
    let wait = tokio::time::sleep(Duration::from_millis(ms));
    match signal {
        Some(signal) => tokio::select! {
            _ = signal.cancelled() => Err(Error::UserAbort),
            _ = wait => Ok(()),
        },
        None => {
            wait.await;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    }

    #[test]
    fn default_policy_retries_408_429_and_5xx() {
        let policy = RetryPolicy::default();
        for status in [408, 429, 500, 503, 599] {
            assert!(policy.is_retryable_status(status), "{status}");
        }
        for status in [200, 400, 401, 404, 422, 600] {
            assert!(!policy.is_retryable_status(status), "{status}");
        }
    }

    #[test]
    fn parses_retry_after_ms_first() {
        assert_eq!(
            parse_retry_after(&headers(&[("retry-after-ms", "250"), ("retry-after", "9")]), None),
            Some(250)
        );
        assert_eq!(
            parse_retry_after(&headers(&[("retry-after-ms", "nope"), ("retry-after", "2")]), None),
            Some(2000)
        );
        assert_eq!(
            parse_retry_after(&headers(&[("retry-after-ms", "-5"), ("retry-after", "1.5")]), None),
            Some(1500)
        );
    }

    #[test]
    fn parses_retry_after_seconds_and_dates() {
        assert_eq!(parse_retry_after(&headers(&[("retry-after", "3")]), None), Some(3000));
        assert_eq!(parse_retry_after(&headers(&[("retry-after", "-1")]), None), None);
        assert_eq!(parse_retry_after(&headers(&[("retry-after", "soon")]), None), None);
        assert_eq!(parse_retry_after(&HeaderMap::new(), None), None);

        let now = httpdate::parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT").unwrap();
        let later = headers(&[("retry-after", "Wed, 21 Oct 2015 07:28:10 GMT")]);
        assert_eq!(parse_retry_after(&later, Some(now)), Some(10_000));
        let earlier = headers(&[("retry-after", "Wed, 21 Oct 2015 07:27:00 GMT")]);
        assert_eq!(parse_retry_after(&earlier, Some(now)), Some(0));
    }

    #[test]
    fn backs_off_exponentially_with_a_cap_and_jitter() {
        let policy = RetryPolicy::default();
        assert_eq!(retry_delay_ms(0, None, &policy, || 0.0), 500);
        assert_eq!(retry_delay_ms(1, None, &policy, || 0.0), 1000);
        assert_eq!(retry_delay_ms(2, None, &policy, || 0.0), 2000);
        assert_eq!(retry_delay_ms(10, None, &policy, || 0.0), 5000);
        assert_eq!(retry_delay_ms(0, None, &policy, || 1.0), 375);
    }

    #[test]
    fn uses_the_server_delay_when_it_is_allowed() {
        let policy = RetryPolicy::default();
        assert_eq!(retry_delay_ms(0, Some(&headers(&[("retry-after", "2")])), &policy, || 0.0), 2000);
        // A delay over the maximum falls back to backoff.
        assert_eq!(retry_delay_ms(0, Some(&headers(&[("retry-after", "120")])), &policy, || 0.0), 500);
        let ignore = RetryPolicy {
            respect_retry_after: false,
            ..RetryPolicy::default()
        };
        assert_eq!(retry_delay_ms(0, Some(&headers(&[("retry-after", "2")])), &ignore, || 0.0), 500);
    }

    #[test]
    fn validates_overrides() {
        let base = RetryPolicy::default();
        let bad_jitter = RetryOverrides {
            backoff_jitter: Some(1.5),
            ..Default::default()
        };
        assert_eq!(
            base.resolve(Some(&bad_jitter)).unwrap_err().to_string(),
            "`retry.backoffJitter` must be between 0 and 1, got 1.5."
        );
        let bad_status = RetryOverrides {
            http_statuses: Some([42].into()),
            ..Default::default()
        };
        assert_eq!(
            base.resolve(Some(&bad_status)).unwrap_err().to_string(),
            "`retry.httpStatuses` must contain HTTP status codes, got 42."
        );
        let bad_ms = RetryOverrides {
            backoff_max_ms: Some(-1.0),
            ..Default::default()
        };
        assert!(base.resolve(Some(&bad_ms)).is_err());

        let merged = base
            .resolve(Some(&RetryOverrides {
                max_retries: Some(0),
                ..Default::default()
            }))
            .unwrap();
        assert_eq!(merged.max_retries, 0);
        assert_eq!(merged.backoff_initial_ms, 500.0);
    }

    #[tokio::test]
    async fn sleep_stops_on_cancellation() {
        let signal = CancellationToken::new();
        signal.cancel();
        assert!(matches!(sleep(10_000, Some(&signal)).await, Err(Error::UserAbort)));
        assert!(sleep(1, None).await.is_ok());
    }
}
