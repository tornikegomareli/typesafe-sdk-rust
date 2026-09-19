//! Port of `test/release-regressions.test.ts`.

mod common;

use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use common::*;
use serde_json::{json, Value};
use typesafe::{noul, score, Error, Questions, RequestOptions, RetryOverrides, SystemOneRequest, TypeSafeClient, TypeSafeClientConfig};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Start a TCP server that sends `response` on each connection. It then keeps the connection
/// open, or it closes the connection. Returns the base URL and the number of connections.
fn raw_server(response: String, keep_open: bool) -> (String, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let connections = Arc::new(AtomicUsize::new(0));
    let count = connections.clone();
    std::thread::spawn(move || {
        let mut open = Vec::new();
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            let mut received = Vec::new();
            let mut chunk = [0u8; 1024];
            while !received.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(size) => received.extend_from_slice(&chunk[..size]),
                }
            }
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
            if keep_open {
                open.push(stream);
            }
        }
    });
    (url, connections)
}

/// Response head that promises 100 body bytes, with only a part of the body.
fn incomplete_response(status: u16) -> String {
    format!("HTTP/1.1 {status} Status\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\r\n{{\"models\"")
}

fn raw_client(url: String, timeout_ms: u64, retry: RetryOverrides) -> TypeSafeClient {
    TypeSafeClient::new(TypeSafeClientConfig {
        base_url: Some(url),
        api_key: Some(API_KEY.into()),
        timeout_ms: Some(timeout_ms),
        retry: Some(retry),
        ..Default::default()
    })
    .unwrap()
}

#[tokio::test]
async fn replaces_mixed_case_defaults_and_protects_every_sdk_header_on_every_attempt() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(sequence(vec![
            json_response(503, json!({})),
            json_response(200, system_one_response()),
        ]))
        .mount(&server)
        .await;
    let pair = |name: &str, value: &str| (name.to_string(), value.to_string());
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        api_key: Some("secret".into()),
        default_headers: vec![
            pair("X-Team", "default"),
            pair("authorization", "bad"),
            pair("content-type", "text/plain"),
            pair("x-typesafe-retry-count", "99"),
        ],
        retry: Some(RetryOverrides {
            max_retries: Some(1),
            backoff_initial_ms: Some(0.0),
            ..Default::default()
        }),
        ..config(&server)
    })
    .unwrap();
    let call = |name: &str, value: &str| (name.to_string(), Some(value.to_string()));
    let options = RequestOptions {
        headers: vec![
            call("x-team", "call"),
            call("AUTHORIZATION", "bad-again"),
            call("ACCEPT", "text/plain"),
            call("USER-AGENT", "bad"),
            call("X-TYPESAFE-SDK", "bad"),
            call("X-TYPESAFE-RUNTIME", "bad"),
            call("CONTENT-TYPE", "text/html"),
            call("X-TYPESAFE-RETRY-COUNT", "88"),
        ],
        ..Default::default()
    };
    client.system_one(request(), options).await.unwrap();

    let requests = received(&server).await;
    assert_eq!(requests.len(), 2);
    for (index, request) in requests.iter().enumerate() {
        let all = |name: &str| -> Vec<&str> { request.headers.get_all(name).iter().map(|value| value.to_str().unwrap()).collect() };
        assert_eq!(all("authorization"), ["Bearer secret"]);
        assert_eq!(all("x-team"), ["call"]);
        assert_eq!(all("content-type"), ["application/json"]);
        assert_eq!(all("accept"), ["application/json"]);
        assert_eq!(all("user-agent"), [format!("typesafe-sdk-rust/{}", typesafe::VERSION)]);
        assert_eq!(all("x-typesafe-sdk"), [format!("typesafe-sdk-rust/{}", typesafe::VERSION)]);
        assert_eq!(all("x-typesafe-runtime"), [typesafe::describe_runtime()]);
        let retry_count: &[&str] = if index == 0 { &[] } else { &["1"] };
        assert_eq!(all("x-typesafe-retry-count"), retry_count);
    }
}

#[tokio::test]
async fn does_not_send_a_caller_content_type_or_retry_count_on_get() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(json_response(200, models_response()))
        .mount(&server)
        .await;
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        default_headers: vec![
            ("content-type".into(), "bad".into()),
            ("x-typesafe-retry-count".into(), "99".into()),
        ],
        ..config(&server)
    })
    .unwrap();
    client.models().list(Default::default()).await.unwrap();

    let requests = received(&server).await;
    assert_eq!(header(&requests[0], "content-type"), None);
    assert_eq!(header(&requests[0], "x-typesafe-retry-count"), None);
}

#[tokio::test]
async fn sends_a_question_named_proto_as_an_ordinary_key() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(json_response(200, system_one_response()))
        .mount(&server)
        .await;
    let mut questions = Questions::new();
    questions.insert("__proto__".into(), noul("?"));
    questions.insert("score".into(), score("?", ["no", "yes"]));
    client(&server)
        .system_one(SystemOneRequest::new("s", questions), Default::default())
        .await
        .unwrap();

    let body: Value = received(&server).await[0].body_json().unwrap();
    assert_eq!(body["questions"]["__proto__"], json!({"type": "noul", "instructions": "?"}));
    assert_eq!(body["questions"]["score"]["criteria"], json!(["no", "yes"]));
}

#[tokio::test]
async fn times_out_a_stalled_body_and_retries_it() {
    for status in [200, 503] {
        let (url, connections) = raw_server(incomplete_response(status), true);
        let retry = RetryOverrides {
            max_retries: Some(1),
            backoff_initial_ms: Some(0.0),
            ..Default::default()
        };
        let error = raw_client(url, 100, retry).models().list(Default::default()).await.unwrap_err();
        assert!(matches!(error, Error::Timeout { timeout_ms: 100, .. }), "{status}: {error:?}");
        assert_eq!(connections.load(Ordering::SeqCst), 2, "{status}");
    }
}

#[tokio::test]
async fn body_failure_gives_a_connection_error_and_honors_disabled_connection_retries() {
    for status in [200, 503] {
        let (url, connections) = raw_server(incomplete_response(status), false);
        let retry = RetryOverrides {
            api_connection_error: Some(false),
            ..fast_retry()
        };
        let error = raw_client(url, 5_000, retry).models().list(Default::default()).await.unwrap_err();
        assert!(matches!(error, Error::Connection { .. }), "{status}: {error:?}");
        assert!(std::error::Error::source(&error).is_some());
        assert_eq!(connections.load(Ordering::SeqCst), 1, "{status}");
    }
}

#[tokio::test]
async fn body_failure_is_retried_by_default() {
    let (url, connections) = raw_server(incomplete_response(200), false);
    let error = raw_client(url, 5_000, fast_retry())
        .models()
        .list(Default::default())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Connection { .. }), "{error:?}");
    assert_eq!(connections.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn gives_a_response_with_an_empty_body_for_a_204() {
    let server = MockServer::start().await;
    Mock::given(any()).respond_with(ResponseTemplate::new(204)).mount(&server).await;
    let response = client(&server).models().list(Default::default()).as_response().await.unwrap();
    assert_eq!(response.status, 204);
    assert!(response.body.is_empty());
    assert_eq!(response.parsed_body(), None);
}
