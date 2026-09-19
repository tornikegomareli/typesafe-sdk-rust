//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use typesafe::{noul, Logger, Questions, RetryOverrides, SystemOneRequest, TypeSafeClient, TypeSafeClientConfig};
use wiremock::{MockServer, Request, Respond, ResponseTemplate};

pub const API_KEY: &str = "test-key";

/// Retry overrides with small backoff delays and no jitter, to keep the tests fast.
pub fn fast_retry() -> RetryOverrides {
    RetryOverrides {
        backoff_initial_ms: Some(1.0),
        backoff_max_ms: Some(5.0),
        backoff_jitter: Some(0.0),
        ..Default::default()
    }
}

/// Fast retry overrides with a maximum number of retries.
pub fn fast_retry_max(max_retries: u32) -> RetryOverrides {
    RetryOverrides {
        max_retries: Some(max_retries),
        ..fast_retry()
    }
}

/// Client configuration that points at the mock server.
pub fn config(server: &MockServer) -> TypeSafeClientConfig {
    TypeSafeClientConfig {
        base_url: Some(server.uri()),
        api_key: Some(API_KEY.into()),
        retry: Some(fast_retry()),
        ..Default::default()
    }
}

/// Client that points at the mock server, with fast retries.
pub fn client(server: &MockServer) -> TypeSafeClient {
    TypeSafeClient::new(config(server)).unwrap()
}

/// Client that points at the mock server, with retries disabled.
pub fn client_without_retries(server: &MockServer) -> TypeSafeClient {
    TypeSafeClient::new(TypeSafeClientConfig {
        retry: Some(fast_retry_max(0)),
        ..config(server)
    })
    .unwrap()
}

/// One noul question named `q1`.
pub fn one_question() -> Questions {
    let mut questions = Questions::new();
    questions.insert("q1".into(), noul("x"));
    questions
}

/// A valid request with one noul question.
pub fn request() -> SystemOneRequest {
    SystemOneRequest::new("s", one_question())
}

/// A valid `POST /v1/systemone` response body.
pub fn system_one_response() -> Value {
    json!({
        "model": "m",
        "answers": {"q1": {"type": "noul", "noul": 0.5}},
        "usage": {"input_tokens": 1, "output_tokens": 1},
    })
}

/// A valid `GET /v1/models` response body.
pub fn models_response() -> Value {
    json!({"models": [{"name": "m", "description": "d", "release_date": "2026"}]})
}

/// A JSON response with a status code.
pub fn json_response(status: u16, body: Value) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(body)
}

/// Responder that gives the responses in order. The last response repeats.
pub struct Sequence {
    responses: Vec<ResponseTemplate>,
    next: AtomicUsize,
}

pub fn sequence(responses: Vec<ResponseTemplate>) -> Sequence {
    Sequence {
        responses,
        next: AtomicUsize::new(0),
    }
}

impl Respond for Sequence {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let index = self.next.fetch_add(1, Ordering::SeqCst).min(self.responses.len() - 1);
        self.responses[index].clone()
    }
}

/// The requests that the mock server received.
pub async fn received(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap()
}

/// The value of a request header as text.
pub fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request.headers.get(name).map(|value| value.to_str().unwrap())
}

/// One recorded logger call.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub level: &'static str,
    pub message: String,
    pub data: Option<Value>,
}

/// Logger that records each call.
#[derive(Default)]
pub struct Recorder(Mutex<Vec<LogLine>>);

impl Recorder {
    pub fn new() -> Arc<Self> {
        Arc::new(Recorder::default())
    }

    pub fn lines(&self) -> Vec<LogLine> {
        self.0.lock().unwrap().clone()
    }

    /// The messages recorded at a level.
    pub fn messages(&self, level: &str) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|line| line.level == level)
            .map(|line| line.message)
            .collect()
    }

    fn push(&self, level: &'static str, message: &str, data: Option<&Value>) {
        self.0.lock().unwrap().push(LogLine {
            level,
            message: message.to_string(),
            data: data.cloned(),
        });
    }
}

impl Logger for Recorder {
    fn debug(&self, message: &str, data: Option<&Value>) {
        self.push("debug", message, data);
    }
    fn info(&self, message: &str, data: Option<&Value>) {
        self.push("info", message, data);
    }
    fn warn(&self, message: &str, data: Option<&Value>) {
        self.push("warn", message, data);
    }
    fn error(&self, message: &str, data: Option<&Value>) {
        self.push("error", message, data);
    }
}
