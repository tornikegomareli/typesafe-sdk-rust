//! Request logging through a recording `Logger`, as in `test/logging.test.ts`.

mod common;

use std::sync::Arc;

use common::*;
use serde_json::json;
use typesafe::{LogLevel, TypeSafeClient, TypeSafeClientConfig};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "sk-test-1234567890";

async fn server_with(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(any()).respond_with(response).mount(&server).await;
    server
}

fn logging_client(server: &MockServer, logger: &Arc<Recorder>, level: LogLevel) -> TypeSafeClient {
    TypeSafeClient::new(TypeSafeClientConfig {
        api_key: Some(SECRET.into()),
        logger: Some(logger.clone()),
        log_level: Some(level),
        retry: Some(fast_retry_max(0)),
        ..config(server)
    })
    .unwrap()
}

#[tokio::test]
async fn debug_log_redacts_the_authorization_value_in_the_headers() {
    let server = server_with(json_response(200, system_one_response())).await;
    let logger = Recorder::new();
    logging_client(&server, &logger, LogLevel::Debug)
        .system_one(request(), Default::default())
        .await
        .unwrap();

    let lines = logger.lines();
    let sent = lines
        .iter()
        .find(|line| line.message == format!("#1 POST /v1/systemone -> {}/v1/systemone", server.uri()));
    let data = sent.expect("no request line").data.clone().unwrap();
    assert_eq!(data["headers"]["Authorization"], "Bearer ***7890");
    assert_eq!(data["headers"]["Accept"], "application/json");
    assert_eq!(data["body"]["state"], "s");
    assert_eq!(data["body"]["questions"]["q1"]["type"], "noul");

    // The raw key is in no message and in no data.
    for line in &lines {
        assert!(!line.message.contains(SECRET), "{line:?}");
        assert!(
            !line.data.as_ref().map(|data| data.to_string()).unwrap_or_default().contains(SECRET),
            "{line:?}"
        );
    }
}

#[tokio::test]
async fn debug_log_has_the_response_body() {
    let server = server_with(json_response(200, system_one_response())).await;
    let logger = Recorder::new();
    logging_client(&server, &logger, LogLevel::Debug)
        .system_one(request(), Default::default())
        .await
        .unwrap();

    let lines = logger.lines();
    let body = lines
        .iter()
        .find(|line| line.message == "#1 POST /v1/systemone <- body")
        .expect("no body line");
    assert_eq!(body.level, "debug");
    assert_eq!(body.data, Some(system_one_response()));
}

#[tokio::test]
async fn info_line_has_the_status_and_the_request_id() {
    let server = server_with(json_response(200, system_one_response()).insert_header("x-typesafe-request-id", "req_123")).await;
    let logger = Recorder::new();
    logging_client(&server, &logger, LogLevel::Debug)
        .system_one(request(), Default::default())
        .await
        .unwrap();

    let info = logger.messages("info");
    assert_eq!(info.len(), 1, "{info:?}");
    assert!(info[0].starts_with("#1 POST /v1/systemone <- 200 in "), "{info:?}");
    assert!(info[0].ends_with("ms (request req_123)"), "{info:?}");
}

#[tokio::test]
async fn info_level_logs_one_summary_line_and_no_debug_lines() {
    let server = server_with(json_response(200, models_response())).await;
    let logger = Recorder::new();
    logging_client(&server, &logger, LogLevel::Info)
        .models()
        .list(Default::default())
        .await
        .unwrap();

    let lines = logger.lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].level, "info");
    assert!(lines[0].message.starts_with("#1 GET /v1/models <- 200 in "), "{lines:?}");
    assert!(lines[0].message.ends_with("ms"), "{lines:?}");
}

#[tokio::test]
async fn warn_and_off_levels_log_nothing_for_a_successful_request() {
    let server = server_with(json_response(200, models_response())).await;
    for level in [LogLevel::Warn, LogLevel::Off] {
        let logger = Recorder::new();
        logging_client(&server, &logger, level)
            .models()
            .list(Default::default())
            .await
            .unwrap();
        assert!(logger.lines().is_empty(), "{level:?}");
    }
}

#[tokio::test]
async fn numbers_the_requests_of_one_client() {
    let server = server_with(json_response(200, models_response())).await;
    let logger = Recorder::new();
    let client = logging_client(&server, &logger, LogLevel::Info);
    client.models().list(Default::default()).await.unwrap();
    client.models().list(Default::default()).await.unwrap();

    let info = logger.messages("info");
    assert!(info[0].starts_with("#1 GET /v1/models"), "{info:?}");
    assert!(info[1].starts_with("#2 GET /v1/models"), "{info:?}");
}

#[tokio::test]
async fn logs_an_error_response_as_a_summary_and_a_debug_body_without_a_warning() {
    let server = server_with(json_response(400, json!({"message": "bad"}))).await;
    let logger = Recorder::new();
    logging_client(&server, &logger, LogLevel::Debug)
        .models()
        .list(Default::default())
        .await
        .unwrap_err();

    let lines = logger.lines();
    assert!(lines
        .iter()
        .any(|line| line.level == "info" && line.message.starts_with("#1 GET /v1/models <- 400 in ")));
    let body = lines
        .iter()
        .find(|line| line.message == "#1 GET /v1/models <- error body")
        .expect("no error body line");
    assert_eq!(body.level, "debug");
    assert_eq!(body.data, Some(json!({"message": "bad"})));
    assert!(lines.iter().all(|line| line.level != "warn" && line.level != "error"), "{lines:?}");
}
