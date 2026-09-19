//! Port of `test/reliability.test.ts`: retries, the retry policy, timeouts, and cancellation.
//!
//! The JavaScript tests use fake timers. These tests use real time with small delays, and they
//! read the calculated delays from the `info` log lines.

mod common;

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use common::*;
use serde_json::json;
use typesafe::{
    ApiErrorKind, CancellationToken, Error, LogLevel, RequestOptions, RetryOverrides, RetryPolicy, TypeSafeClient, TypeSafeClientConfig,
};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

/// Mock server that answers every request with the responder.
async fn server_with(responder: impl Respond + 'static) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any()).respond_with(responder).mount(&server).await;
    server
}

/// Mock server that answers every request with a status and an empty JSON object.
async fn always(status: u16) -> MockServer {
    server_with(json_response(status, json!({}))).await
}

fn ok_models() -> ResponseTemplate {
    json_response(200, models_response())
}

fn client_with_retry(server: &MockServer, retry: RetryOverrides) -> TypeSafeClient {
    TypeSafeClient::new(TypeSafeClientConfig {
        retry: Some(retry),
        ..config(server)
    })
    .unwrap()
}

fn retry_options(retry: RetryOverrides) -> RequestOptions {
    RequestOptions {
        retry: Some(retry),
        ..Default::default()
    }
}

/// Base URL of a local port with no listener.
fn closed_port_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

fn kind_of(error: &Error) -> ApiErrorKind {
    error
        .as_api_error()
        .unwrap_or_else(|| panic!("expected an API error, got {error:?}"))
        .kind()
}

// ---------------------------------------------------------------------------
// Retries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn retries_a_500_up_to_max_retries_then_fails_with_the_last_error() {
    let server = server_with(json_response(500, json!({"message": "down"}))).await;
    let error = client_with_retry(&server, fast_retry_max(3))
        .models()
        .list(Default::default())
        .await
        .unwrap_err();
    assert_eq!(kind_of(&error), ApiErrorKind::InternalServer);
    assert_eq!(error.to_string(), "500 down");
    assert_eq!(received(&server).await.len(), 4);
}

#[tokio::test]
async fn retries_a_429_up_to_max_retries_then_fails() {
    let server = always(429).await;
    let error = client(&server).system_one(request(), Default::default()).await.unwrap_err();
    assert_eq!(kind_of(&error), ApiErrorKind::RateLimit);
    assert_eq!(received(&server).await.len(), 3);
}

#[tokio::test]
async fn succeeds_when_a_later_attempt_succeeds() {
    let server = server_with(sequence(vec![
        json_response(503, json!({})),
        json_response(503, json!({})),
        ok_models(),
    ]))
    .await;
    let models = client(&server).models().list(Default::default()).await.unwrap();
    assert_eq!(models[0].name, "m");
    assert_eq!(received(&server).await.len(), 3);
}

#[tokio::test]
async fn retry_count_header_is_absent_on_the_first_attempt_then_1_and_2() {
    let server = server_with(sequence(vec![
        json_response(503, json!({})),
        json_response(503, json!({})),
        ok_models(),
    ]))
    .await;
    client(&server).models().list(Default::default()).await.unwrap();

    let requests = received(&server).await;
    assert_eq!(requests.len(), 3);
    assert_eq!(header(&requests[0], "x-typesafe-retry-count"), None);
    assert_eq!(header(&requests[1], "x-typesafe-retry-count"), Some("1"));
    assert_eq!(header(&requests[2], "x-typesafe-retry-count"), Some("2"));
}

#[tokio::test]
async fn does_not_retry_a_400() {
    let server = always(400).await;
    let error = client(&server).models().list(Default::default()).await.unwrap_err();
    assert_eq!(kind_of(&error), ApiErrorKind::BadRequest);
    assert_eq!(received(&server).await.len(), 1);
}

#[tokio::test]
async fn max_retries_zero_disables_retries() {
    let server = always(503).await;
    let error = client_with_retry(&server, fast_retry_max(0))
        .models()
        .list(Default::default())
        .await
        .unwrap_err();
    assert_eq!(kind_of(&error), ApiErrorKind::InternalServer);
    assert_eq!(received(&server).await.len(), 1);
}

#[tokio::test]
async fn per_call_max_retries_wins_over_the_client_policy() {
    let server = always(503).await;
    let client = client_with_retry(&server, fast_retry_max(5));
    let options = retry_options(RetryOverrides {
        max_retries: Some(0),
        ..Default::default()
    });
    let error = client.models().list(options).await.unwrap_err();
    assert_eq!(kind_of(&error), ApiErrorKind::InternalServer);
    assert_eq!(received(&server).await.len(), 1);
}

#[tokio::test]
async fn per_call_retry_overrides_field_by_field_and_keeps_the_client_policy() {
    let server = always(503).await;
    let logger = Recorder::new();
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        logger: Some(logger.clone()),
        log_level: Some(LogLevel::Info),
        retry: Some(RetryOverrides {
            max_retries: Some(5),
            backoff_initial_ms: Some(20.0),
            backoff_jitter: Some(0.0),
            ..Default::default()
        }),
        ..config(&server)
    })
    .unwrap();
    let options = retry_options(RetryOverrides {
        max_retries: Some(1),
        ..Default::default()
    });
    let error = client.models().list(options).await.unwrap_err();

    assert_eq!(kind_of(&error), ApiErrorKind::InternalServer);
    assert_eq!(received(&server).await.len(), 2);
    // `backoff_initial_ms` came from the client, `max_retries` from the call.
    assert!(logger
        .messages("info")
        .contains(&"#1 GET /v1/models retrying in 20ms (retry 1/1) after 503".to_string()));
    assert_eq!(client.retry().max_retries, 5);
}

// ---------------------------------------------------------------------------
// Retry policy
// ---------------------------------------------------------------------------

#[test]
fn client_exposes_the_resolved_retry_policy() {
    let overrides = RetryOverrides {
        max_retries: Some(7),
        backoff_jitter: Some(0.0),
        ..Default::default()
    };
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        api_key: Some("k".into()),
        retry: Some(overrides),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        *client.retry(),
        RetryPolicy {
            max_retries: 7,
            backoff_jitter: 0.0,
            ..RetryPolicy::default()
        }
    );
}

#[tokio::test]
async fn retries_only_the_statuses_in_http_statuses() {
    let conflict = always(409).await;
    let retry = RetryOverrides {
        http_statuses: Some([409].into()),
        ..fast_retry()
    };
    let error = client_with_retry(&conflict, retry)
        .models()
        .list(Default::default())
        .await
        .unwrap_err();
    assert_eq!(kind_of(&error), ApiErrorKind::Other);
    assert_eq!(received(&conflict).await.len(), 3);

    let unavailable = always(503).await;
    let retry = RetryOverrides {
        http_statuses: Some([].into()),
        ..fast_retry()
    };
    let error = client_with_retry(&unavailable, retry)
        .models()
        .list(Default::default())
        .await
        .unwrap_err();
    assert_eq!(kind_of(&error), ApiErrorKind::InternalServer);
    assert_eq!(received(&unavailable).await.len(), 1);
}

#[tokio::test]
async fn backs_off_from_the_configured_initial_delay_and_cap() {
    let unavailable = json_response(503, json!({}));
    let server = server_with(sequence(vec![unavailable.clone(), unavailable.clone(), unavailable, ok_models()])).await;
    let logger = Recorder::new();
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        logger: Some(logger.clone()),
        log_level: Some(LogLevel::Info),
        retry: Some(RetryOverrides {
            max_retries: Some(3),
            backoff_initial_ms: Some(10.0),
            backoff_max_ms: Some(25.0),
            backoff_jitter: Some(0.0),
            ..Default::default()
        }),
        ..config(&server)
    })
    .unwrap();
    client.models().list(Default::default()).await.unwrap();

    let retries: Vec<String> = logger
        .messages("info")
        .into_iter()
        .filter(|line| line.contains("retrying"))
        .collect();
    assert_eq!(
        retries,
        [
            "#1 GET /v1/models retrying in 10ms (retry 1/3) after 503",
            "#1 GET /v1/models retrying in 20ms (retry 2/3) after 503",
            "#1 GET /v1/models retrying in 25ms (retry 3/3) after 503",
        ]
    );
}

#[tokio::test]
async fn honors_the_retry_after_ms_header() {
    let limited = json_response(429, json!({})).insert_header("retry-after-ms", "80");
    let server = server_with(sequence(vec![limited, ok_models()])).await;
    let logger = Recorder::new();
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        logger: Some(logger.clone()),
        log_level: Some(LogLevel::Info),
        ..config(&server)
    })
    .unwrap();

    let started = Instant::now();
    client.models().list(Default::default()).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(80), "{:?}", started.elapsed());
    assert_eq!(received(&server).await.len(), 2);

    let lines = logger.messages("info");
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].starts_with("#1 GET /v1/models <- 429 in "), "{lines:?}");
    assert_eq!(lines[1], "#1 GET /v1/models retrying in 80ms (retry 1/2) after 429");
    assert!(lines[2].starts_with("#1 GET /v1/models <- 200 in "), "{lines:?}");
}

#[tokio::test]
async fn honors_the_retry_after_header_in_seconds() {
    let limited = json_response(429, json!({})).insert_header("retry-after", "1");
    let server = server_with(sequence(vec![limited, ok_models()])).await;

    let started = Instant::now();
    client(&server).models().list(Default::default()).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(1000), "{:?}", started.elapsed());
    assert_eq!(received(&server).await.len(), 2);
}

#[tokio::test]
async fn can_ignore_retry_after_and_caps_how_long_it_may_be() {
    let limited = || json_response(429, json!({})).insert_header("retry-after", "30");

    let server = server_with(sequence(vec![limited(), ok_models()])).await;
    let ignore = RetryOverrides {
        respect_retry_after: Some(false),
        ..fast_retry()
    };
    let started = Instant::now();
    client_with_retry(&server, ignore).models().list(Default::default()).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert_eq!(received(&server).await.len(), 2);

    // A server delay over `max_retry_after_ms` falls back to backoff.
    let server = server_with(sequence(vec![limited(), ok_models()])).await;
    let capped = RetryOverrides {
        max_retry_after_ms: Some(1000.0),
        ..fast_retry()
    };
    let started = Instant::now();
    client_with_retry(&server, capped).models().list(Default::default()).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert_eq!(received(&server).await.len(), 2);
}

#[tokio::test]
async fn rejects_invalid_retry_overrides_from_the_config_and_per_call() {
    let bad = |retry: RetryOverrides| {
        TypeSafeClient::new(TypeSafeClientConfig {
            api_key: Some("k".into()),
            retry: Some(retry),
            ..Default::default()
        })
        .unwrap_err()
        .to_string()
    };
    assert!(bad(RetryOverrides {
        backoff_initial_ms: Some(-1.0),
        ..Default::default()
    })
    .contains("retry.backoffInitialMs"));
    assert!(bad(RetryOverrides {
        backoff_max_ms: Some(f64::NAN),
        ..Default::default()
    })
    .contains("retry.backoffMaxMs"));
    assert!(bad(RetryOverrides {
        backoff_jitter: Some(1.5),
        ..Default::default()
    })
    .contains("retry.backoffJitter"));
    assert!(bad(RetryOverrides {
        backoff_jitter: Some(-0.1),
        ..Default::default()
    })
    .contains("retry.backoffJitter"));
    assert!(bad(RetryOverrides {
        max_retry_after_ms: Some(f64::INFINITY),
        ..Default::default()
    })
    .contains("retry.maxRetryAfterMs"));
    assert!(bad(RetryOverrides {
        http_statuses: Some([503, 42].into()),
        ..Default::default()
    })
    .contains("retry.httpStatuses"));

    let zeros = RetryOverrides {
        backoff_initial_ms: Some(0.0),
        backoff_max_ms: Some(0.0),
        backoff_jitter: Some(0.0),
        max_retry_after_ms: Some(0.0),
        ..Default::default()
    };
    assert!(TypeSafeClient::new(TypeSafeClientConfig {
        api_key: Some("k".into()),
        retry: Some(zeros),
        ..Default::default()
    })
    .is_ok());

    let server = always(200).await;
    let options = retry_options(RetryOverrides {
        backoff_jitter: Some(2.0),
        ..Default::default()
    });
    let error = client(&server).models().list(options).await.unwrap_err();
    assert!(matches!(error, Error::TypeSafe(_)));
    assert!(error.to_string().contains("retry.backoffJitter"));
    assert!(received(&server).await.is_empty());
}

// ---------------------------------------------------------------------------
// Connection errors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connection_errors_are_retried_and_give_a_connection_error() {
    let logger = Recorder::new();
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        base_url: Some(closed_port_url()),
        api_key: Some(API_KEY.into()),
        retry: Some(fast_retry()),
        logger: Some(logger.clone()),
        log_level: Some(LogLevel::Info),
        ..Default::default()
    })
    .unwrap();
    let error = client.models().list(Default::default()).await.unwrap_err();

    assert!(matches!(error, Error::Connection { .. }), "{error:?}");
    assert!(error.is_connection_error());
    assert!(error.to_string().starts_with("Connection error: "), "{error}");
    assert!(std::error::Error::source(&error).is_some());

    let lines = logger.messages("info");
    assert_eq!(
        lines.iter().filter(|line| line.contains("connection error after")).count(),
        3,
        "{lines:?}"
    );
    assert_eq!(lines.iter().filter(|line| line.contains("retrying in")).count(), 2, "{lines:?}");
}

#[tokio::test]
async fn retries_a_connection_error_then_succeeds() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // The first connection closes without a response. The second one gets the model list.
    let accepted = std::thread::spawn(move || {
        drop(listener.accept().unwrap());
        let (mut stream, _) = listener.accept().unwrap();
        let mut received = Vec::new();
        let mut chunk = [0u8; 1024];
        while !received.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut chunk).unwrap();
            received.extend_from_slice(&chunk[..count]);
        }
        let body = models_response().to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        String::from_utf8_lossy(&received).to_lowercase()
    });

    let client = TypeSafeClient::new(TypeSafeClientConfig {
        base_url: Some(format!("http://127.0.0.1:{port}")),
        api_key: Some(API_KEY.into()),
        retry: Some(fast_retry()),
        ..Default::default()
    })
    .unwrap();
    let models = client.models().list(Default::default()).await.unwrap();
    assert_eq!(models[0].name, "m");
    assert!(accepted.join().unwrap().contains("x-typesafe-retry-count: 1"));
}

#[tokio::test]
async fn can_stop_retrying_connection_errors_while_still_retrying_timeouts() {
    let logger = Recorder::new();
    let retry = RetryOverrides {
        api_connection_error: Some(false),
        ..fast_retry_max(1)
    };
    let dropped = TypeSafeClient::new(TypeSafeClientConfig {
        base_url: Some(closed_port_url()),
        api_key: Some(API_KEY.into()),
        retry: Some(retry.clone()),
        logger: Some(logger.clone()),
        log_level: Some(LogLevel::Info),
        ..Default::default()
    })
    .unwrap();
    let error = dropped.models().list(Default::default()).await.unwrap_err();
    assert!(matches!(error, Error::Connection { .. }), "{error:?}");
    let lines = logger.messages("info");
    assert_eq!(
        lines.iter().filter(|line| line.contains("connection error after")).count(),
        1,
        "{lines:?}"
    );

    let hung = server_with(ok_models().set_delay(Duration::from_millis(500))).await;
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        timeout_ms: Some(50),
        retry: Some(retry),
        ..config(&hung)
    })
    .unwrap();
    let error = client.models().list(Default::default()).await.unwrap_err();
    assert!(matches!(error, Error::Timeout { .. }), "{error:?}");
    assert_eq!(received(&hung).await.len(), 2);
}

// ---------------------------------------------------------------------------
// Timeouts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn slow_response_gives_a_timeout_error_which_is_a_connection_error() {
    let server = server_with(ok_models().set_delay(Duration::from_millis(500))).await;
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        timeout_ms: Some(50),
        retry: Some(fast_retry_max(0)),
        ..config(&server)
    })
    .unwrap();
    let started = Instant::now();
    let error = client.models().list(Default::default()).await.unwrap_err();

    assert!(matches!(error, Error::Timeout { timeout_ms: 50, .. }), "{error:?}");
    assert!(error.is_connection_error());
    assert_eq!(error.to_string(), "Request timed out after 50ms.");
    assert!(started.elapsed() >= Duration::from_millis(50));
    assert!(started.elapsed() < Duration::from_millis(450), "{:?}", started.elapsed());
}

#[tokio::test]
async fn timeout_is_retried_by_default() {
    let server = server_with(ok_models().set_delay(Duration::from_millis(500))).await;
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        timeout_ms: Some(50),
        retry: Some(fast_retry()),
        ..config(&server)
    })
    .unwrap();
    let error = client.models().list(Default::default()).await.unwrap_err();
    assert!(matches!(error, Error::Timeout { timeout_ms: 50, .. }), "{error:?}");
    assert_eq!(received(&server).await.len(), 3);
}

#[tokio::test]
async fn timeout_is_not_retried_when_api_timeout_error_is_false() {
    let server = server_with(ok_models().set_delay(Duration::from_millis(500))).await;
    let retry = RetryOverrides {
        api_timeout_error: Some(false),
        ..fast_retry()
    };
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        timeout_ms: Some(50),
        retry: Some(retry),
        ..config(&server)
    })
    .unwrap();
    let error = client.models().list(Default::default()).await.unwrap_err();
    assert!(matches!(error, Error::Timeout { .. }), "{error:?}");
    assert_eq!(received(&server).await.len(), 1);
}

#[tokio::test]
async fn retries_after_a_timeout_and_each_attempt_gets_its_own_timeout() {
    let server = server_with(sequence(vec![ok_models().set_delay(Duration::from_millis(500)), ok_models()])).await;
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        timeout_ms: Some(100),
        ..config(&server)
    })
    .unwrap();
    let models = client.models().list(Default::default()).await.unwrap();
    assert_eq!(models[0].name, "m");
    assert_eq!(received(&server).await.len(), 2);
}

#[tokio::test]
async fn per_call_timeout_overrides_the_client_timeout() {
    let server = server_with(ok_models().set_delay(Duration::from_millis(500))).await;
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        timeout_ms: Some(60_000),
        retry: Some(fast_retry_max(0)),
        ..config(&server)
    })
    .unwrap();
    let options = RequestOptions {
        timeout_ms: Some(50),
        ..Default::default()
    };
    let error = client.models().list(options).await.unwrap_err();
    assert!(matches!(error, Error::Timeout { timeout_ms: 50, .. }), "{error:?}");
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn token_cancelled_before_the_call_gives_user_abort_and_no_retry() {
    let server = always(503).await;
    let logger = Recorder::new();
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        logger: Some(logger.clone()),
        log_level: Some(LogLevel::Info),
        ..config(&server)
    })
    .unwrap();
    let signal = CancellationToken::new();
    signal.cancel();
    let error = client
        .models()
        .list(RequestOptions {
            signal: Some(signal),
            ..Default::default()
        })
        .await
        .unwrap_err();

    assert!(matches!(error, Error::UserAbort), "{error:?}");
    assert!(!error.is_connection_error());
    assert_eq!(error.to_string(), "Request was aborted.");
    assert!(received(&server).await.is_empty());
    let lines = logger.messages("info");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("#1 GET /v1/models aborted by caller after "), "{lines:?}");
}

#[tokio::test]
async fn token_cancelled_during_the_backoff_wait_gives_user_abort() {
    let server = always(503).await;
    let retry = RetryOverrides {
        backoff_initial_ms: Some(30_000.0),
        backoff_max_ms: Some(30_000.0),
        ..Default::default()
    };
    let client = client_with_retry(&server, retry);
    let signal = CancellationToken::new();
    let canceller = signal.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        canceller.cancel();
    });

    let started = Instant::now();
    let error = client
        .models()
        .list(RequestOptions {
            signal: Some(signal),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(error, Error::UserAbort), "{error:?}");
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    assert_eq!(received(&server).await.len(), 1);
}

#[tokio::test]
async fn token_cancelled_during_a_slow_request_gives_user_abort_not_a_timeout() {
    let server = server_with(ok_models().set_delay(Duration::from_secs(5))).await;
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        timeout_ms: Some(60_000),
        ..config(&server)
    })
    .unwrap();
    let signal = CancellationToken::new();
    let canceller = signal.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        canceller.cancel();
    });

    let error = client
        .models()
        .list(RequestOptions {
            signal: Some(signal),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(error, Error::UserAbort), "{error:?}");
    assert_eq!(received(&server).await.len(), 1);
}
