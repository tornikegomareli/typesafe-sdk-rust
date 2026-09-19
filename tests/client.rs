//! Port of `test/client.test.ts`: request shape, headers, validation, answers, errors, and models.
//!
//! The tests that read environment variables are in `config_env.rs`.

mod common;

use common::*;
use serde_json::{json, Value};
use typesafe::{
    choice, describe_runtime, noul, noul_with_criteria, score, ApiErrorKind, Error, ModelCard, Questions, RequestOptions, SystemOneRequest,
    TypeSafeClient, TypeSafeClientConfig, VERSION,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mock server that answers `POST /v1/systemone` with a body.
async fn system_one_server(body: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(json_response(200, body))
        .mount(&server)
        .await;
    server
}

/// Mock server that answers `GET /v1/models` with a status and a body.
async fn models_server(status: u16, body: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(json_response(status, body))
        .mount(&server)
        .await;
    server
}

/// The JSON body of the only request that the server received.
async fn sent_body(server: &MockServer) -> Value {
    let requests = received(server).await;
    assert_eq!(requests.len(), 1);
    requests[0].body_json().unwrap()
}

// ---------------------------------------------------------------------------
// Request shape
// ---------------------------------------------------------------------------

#[tokio::test]
async fn posts_state_questions_and_model_to_v1_systemone() {
    let server = system_one_server(system_one_response()).await;
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        default_model: Some("jev-latest".into()),
        ..config(&server)
    })
    .unwrap();
    let result = client
        .system_one(SystemOneRequest::new(json!({"a": 1}), one_question()), Default::default())
        .await
        .unwrap();

    let requests = received(&server).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "POST");
    assert_eq!(requests[0].url.path(), "/v1/systemone");
    assert_eq!(
        requests[0].body_json::<Value>().unwrap(),
        json!({
            "state": {"a": 1},
            "model": "jev-latest",
            "questions": {"q1": {"type": "noul", "instructions": "x"}},
        })
    );
    assert_eq!(result.noul("q1").unwrap().noul, 0.5);
}

#[tokio::test]
async fn per_request_model_overrides_the_client_default_model() {
    let server = system_one_server(system_one_response()).await;
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        default_model: Some("client-default".into()),
        ..config(&server)
    })
    .unwrap();
    assert_eq!(client.default_model(), "client-default");

    client.system_one(request(), Default::default()).await.unwrap();
    client.system_one(request().model("per-call"), Default::default()).await.unwrap();

    let requests = received(&server).await;
    assert_eq!(requests[0].body_json::<Value>().unwrap()["model"], "client-default");
    assert_eq!(requests[1].body_json::<Value>().unwrap()["model"], "per-call");
}

#[tokio::test]
async fn preserves_null_state_instructions_and_criteria() {
    let server = system_one_server(system_one_response()).await;
    let mut questions = Questions::new();
    questions.insert("noul".into(), noul_with_criteria(Value::Null, Some(Value::Null), Some(Value::Null)));
    questions.insert("choice".into(), choice(Value::Null, [("yes", Value::Null), ("no", Value::Null)]));
    questions.insert("score".into(), score(Value::Null, [Value::Null, json!("high")]));
    client(&server)
        .system_one(SystemOneRequest::new(Value::Null, questions), Default::default())
        .await
        .unwrap();

    let body = sent_body(&server).await;
    assert_eq!(body["state"], Value::Null);
    assert_eq!(
        body["questions"],
        json!({
            "noul": {"type": "noul", "instructions": null, "criteria": {"true": null, "false": null}},
            "choice": {"type": "choice", "instructions": null, "criteria": {"yes": null, "no": null}},
            "score": {"type": "score", "instructions": null, "criteria": [null, "high"]},
        })
    );
}

#[tokio::test]
async fn preserves_json_arrays_and_objects_in_state_and_descriptions() {
    let server = system_one_server(system_one_response()).await;
    let rich = json!({"summary": "warm", "examples": ["hi!", "welcome"]});
    let state = json!([null, {"messages": ["hello"]}]);
    let mut questions = Questions::new();
    questions.insert(
        "q".into(),
        choice(json!([null, {"examples": [1, false]}]), [("friendly", rich.clone())]),
    );
    client(&server)
        .system_one(SystemOneRequest::new(state.clone(), questions), Default::default())
        .await
        .unwrap();

    let body = sent_body(&server).await;
    assert_eq!(body["state"], state);
    assert_eq!(body["questions"]["q"]["instructions"], json!([null, {"examples": [1, false]}]));
    assert_eq!(body["questions"]["q"]["criteria"]["friendly"], rich);
}

#[tokio::test]
async fn forwards_extra_fields_including_null() {
    let server = system_one_server(system_one_response()).await;
    let client = client(&server);
    let mut with_extra = request();
    with_extra.extra.insert("future_option".into(), Value::Null);
    with_extra.extra.insert("nested".into(), json!({"enabled": true}));
    client.system_one(with_extra, Default::default()).await.unwrap();
    client.system_one(request(), Default::default()).await.unwrap();

    let requests = received(&server).await;
    let first: Value = requests[0].body_json().unwrap();
    let object = first.as_object().unwrap();
    assert_eq!(object.get("future_option"), Some(&Value::Null));
    assert_eq!(first["nested"], json!({"enabled": true}));
    let second: Value = requests[1].body_json().unwrap();
    assert!(second.as_object().unwrap().get("future_option").is_none());
}

// ---------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sends_auth_and_identifying_headers_on_post() {
    let server = system_one_server(system_one_response()).await;
    client(&server).system_one(request(), Default::default()).await.unwrap();

    let requests = received(&server).await;
    let sdk = format!("typesafe-sdk-rust/{VERSION}");
    assert_eq!(header(&requests[0], "authorization"), Some("Bearer test-key"));
    assert_eq!(header(&requests[0], "accept"), Some("application/json"));
    assert_eq!(header(&requests[0], "content-type"), Some("application/json"));
    assert_eq!(header(&requests[0], "user-agent"), Some(sdk.as_str()));
    assert_eq!(header(&requests[0], "x-typesafe-sdk"), Some(sdk.as_str()));
    assert_eq!(header(&requests[0], "x-typesafe-runtime"), Some(describe_runtime().as_str()));
    assert!(describe_runtime().starts_with("rust ("));
}

#[tokio::test]
async fn sends_no_content_type_on_get() {
    let server = models_server(200, models_response()).await;
    client(&server).models().list(Default::default()).await.unwrap();

    let requests = received(&server).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "GET");
    assert_eq!(requests[0].url.path(), "/v1/models");
    assert_eq!(header(&requests[0], "authorization"), Some("Bearer test-key"));
    assert_eq!(header(&requests[0], "content-type"), None);
}

#[tokio::test]
async fn per_call_headers_win_over_default_headers_and_never_replace_auth() {
    let server = models_server(200, models_response()).await;
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        default_headers: vec![
            ("X-Trace".into(), "client".into()),
            ("X-Only-Default".into(), "yes".into()),
            ("Authorization".into(), "nope".into()),
        ],
        ..config(&server)
    })
    .unwrap();
    let options = RequestOptions {
        headers: vec![("X-Trace".into(), Some("call".into())), ("X-Only-Call".into(), Some("yes".into()))],
        ..Default::default()
    };
    client.models().list(options).await.unwrap();

    let requests = received(&server).await;
    assert_eq!(header(&requests[0], "x-trace"), Some("call"));
    assert_eq!(header(&requests[0], "x-only-default"), Some("yes"));
    assert_eq!(header(&requests[0], "x-only-call"), Some("yes"));
    assert_eq!(header(&requests[0], "authorization"), Some("Bearer test-key"));
}

#[tokio::test]
async fn per_call_none_header_removes_a_default_header() {
    let server = models_server(200, models_response()).await;
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        default_headers: vec![("X-Team".into(), "default".into())],
        ..config(&server)
    })
    .unwrap();
    client
        .models()
        .list(RequestOptions {
            headers: vec![("x-team".into(), None)],
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(header(&received(&server).await[0], "x-team"), None);
}

// ---------------------------------------------------------------------------
// Configuration without environment variables
// ---------------------------------------------------------------------------

#[tokio::test]
async fn removes_trailing_slashes_from_the_configured_base_url() {
    let server = models_server(200, models_response()).await;
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        base_url: Some(format!("{}///", server.uri())),
        ..config(&server)
    })
    .unwrap();
    assert_eq!(client.base_url(), server.uri());

    client.models().list(Default::default()).await.unwrap();
    assert_eq!(received(&server).await[0].url.path(), "/v1/models");
}

#[test]
fn rejects_a_timeout_of_zero_in_the_config() {
    let error = TypeSafeClient::new(TypeSafeClientConfig {
        api_key: Some("k".into()),
        timeout_ms: Some(0),
        ..Default::default()
    })
    .unwrap_err();
    assert!(matches!(error, Error::TypeSafe(_)));
    assert_eq!(error.to_string(), "`timeout` must be a positive number of milliseconds, got 0.");
}

#[tokio::test]
async fn rejects_a_per_call_timeout_of_zero_before_any_request() {
    let server = models_server(200, models_response()).await;
    let error = client(&server)
        .models()
        .list(RequestOptions {
            timeout_ms: Some(0),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(error, Error::TypeSafe(_)));
    assert!(error.to_string().contains("`timeout`"));
    assert!(received(&server).await.is_empty());
}

#[test]
fn debug_output_of_the_client_does_not_show_the_api_key() {
    let client = TypeSafeClient::new(TypeSafeClientConfig {
        api_key: Some("super-secret".into()),
        ..Default::default()
    })
    .unwrap();
    assert!(!format!("{client:?}").contains("super-secret"));
}

// ---------------------------------------------------------------------------
// Validation before any request
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rejects_empty_questions_before_any_request() {
    let server = system_one_server(system_one_response()).await;
    let error = client(&server)
        .system_one(SystemOneRequest::new("s", Questions::new()), Default::default())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::TypeSafe(_)));
    assert_eq!(error.to_string(), "At least one question is required.");
    assert!(received(&server).await.is_empty());
}

#[tokio::test]
async fn rejects_a_score_with_fewer_than_two_criteria_before_any_request() {
    let server = system_one_server(system_one_response()).await;
    let client = client(&server);
    for (criteria, message) in [
        (vec![], "Score question \"q\" has 0 criteria; at least two scores are required."),
        (
            vec!["only"],
            "Score question \"q\" has 1 criteria; at least two scores are required.",
        ),
    ] {
        let mut questions = Questions::new();
        questions.insert("q".into(), score("?", criteria));
        let error = client
            .system_one(SystemOneRequest::new("s", questions), Default::default())
            .await
            .unwrap_err();
        assert!(matches!(error, Error::TypeSafe(_)));
        assert_eq!(error.to_string(), message);
    }
    assert!(received(&server).await.is_empty());
}

#[tokio::test]
async fn validation_errors_come_through_with_response_and_as_response() {
    let server = system_one_server(system_one_response()).await;
    let client = client(&server);
    let empty = || SystemOneRequest::new("s", Questions::new());
    assert!(matches!(
        client.system_one(empty(), Default::default()).with_response().await,
        Err(Error::TypeSafe(_))
    ));
    assert!(matches!(
        client.system_one(empty(), Default::default()).as_response().await,
        Err(Error::TypeSafe(_))
    ));
    assert!(received(&server).await.is_empty());
}

// ---------------------------------------------------------------------------
// Answers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn parses_noul_choice_and_score_answers_and_usage() {
    let server = system_one_server(json!({
        "model": "jev-1",
        "answers": {
            "urgent": {"type": "noul", "noul": 0.87},
            "category": {"type": "choice", "choice": "b", "confidence": 0.9, "probabilities": {"a": 0.1, "b": 0.9}},
            "rating": {
                "type": "score",
                "score": 1.6,
                "confidence": 0.7,
                "legend": {"0": "bad", "1": "ok", "2": "great"},
                "probabilities": {"0": 0.1, "1": 0.2, "2": 0.7},
            },
        },
        "usage": {"input_tokens": 12, "output_tokens": 3},
    }))
    .await;
    let mut questions = Questions::new();
    questions.insert("urgent".into(), noul("Is it urgent?"));
    questions.insert("category".into(), choice("Which?", [("a", Value::Null), ("b", Value::Null)]));
    questions.insert("rating".into(), score("Rate it", ["bad", "ok", "great"]));
    let result = client(&server)
        .system_one(SystemOneRequest::new("s", questions), Default::default())
        .await
        .unwrap();

    assert_eq!(result.model, "jev-1");
    assert_eq!(result.usage.input_tokens, 12);
    assert_eq!(result.usage.output_tokens, 3);
    assert_eq!(result.answers.len(), 3);

    assert_eq!(result.noul("urgent").unwrap().noul, 0.87);

    let category = result.choice("category").unwrap();
    assert_eq!(category.choice, "b");
    assert_eq!(category.confidence, 0.9);
    assert_eq!(category.probabilities["a"], 0.1);
    assert_eq!(category.probabilities["b"], 0.9);

    let rating = result.score("rating").unwrap();
    assert_eq!(rating.score, 1.6);
    assert_eq!(rating.confidence, 0.7);
    assert_eq!(rating.legend["2"], "great");
    assert_eq!(rating.probabilities_by_score(), [0.1, 0.2, 0.7]);
}

#[tokio::test]
async fn answer_accessors_give_none_for_a_wrong_name_or_type() {
    let server = system_one_server(system_one_response()).await;
    let result = client(&server).system_one(request(), Default::default()).await.unwrap();
    assert!(result.noul("q1").is_some());
    assert!(result.noul("missing").is_none());
    assert!(result.choice("q1").is_none());
    assert!(result.score("q1").is_none());
}

#[tokio::test]
async fn unknown_response_shape_gives_a_typesafe_error() {
    for body in [
        json!({"ok": true}),
        json!({"model": "m", "answers": {"q1": {"type": "future"}}, "usage": {}}),
    ] {
        let server = system_one_server(body).await;
        let error = client(&server).system_one(request(), Default::default()).await.unwrap_err();
        assert!(matches!(error, Error::TypeSafe(_)), "{error:?}");
        assert!(
            error.to_string().starts_with("Unexpected response shape from POST /v1/systemone"),
            "{error}"
        );
    }
}

// ---------------------------------------------------------------------------
// API errors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn maps_each_status_to_its_api_error_kind() {
    for (status, kind) in [
        (400, ApiErrorKind::BadRequest),
        (401, ApiErrorKind::Authentication),
        (403, ApiErrorKind::PermissionDenied),
        (404, ApiErrorKind::NotFound),
        (422, ApiErrorKind::UnprocessableEntity),
        (429, ApiErrorKind::RateLimit),
        (500, ApiErrorKind::InternalServer),
        (503, ApiErrorKind::InternalServer),
        (418, ApiErrorKind::Other),
    ] {
        let server = models_server(status, json!({"message": "nope"})).await;
        let error = client_without_retries(&server).models().list(Default::default()).await.unwrap_err();
        let api = error.as_api_error().unwrap_or_else(|| panic!("{status} gave {error:?}"));
        assert!(matches!(error, Error::Api(_)));
        assert_eq!(api.status, status);
        assert_eq!(api.kind(), kind, "{status}");
        assert_eq!(api.body, Some(json!({"message": "nope"})));
        assert_eq!(error.to_string(), format!("{status} nope"));
    }
}

#[tokio::test]
async fn extracts_the_error_message_from_the_body() {
    let cases = [
        (ResponseTemplate::new(401).set_body_json(json!({"error": "bad key"})), "401 bad key"),
        (
            ResponseTemplate::new(401).set_body_json(json!({"error": {"message": "nested"}})),
            "401 nested",
        ),
        (
            ResponseTemplate::new(400).set_body_json(json!({"detail": "detail text"})),
            "400 detail text",
        ),
        (
            ResponseTemplate::new(422).set_body_json(json!({"detail": [{"loc": ["body", "questions", "q1"], "msg": "field required"}]})),
            "422 questions.q1: field required",
        ),
        (ResponseTemplate::new(400).set_body_string("plain text"), "400 plain text"),
        (ResponseTemplate::new(500), "500 status code (no body)"),
    ];
    for (response, message) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(response)
            .mount(&server)
            .await;
        let error = client_without_retries(&server).models().list(Default::default()).await.unwrap_err();
        assert_eq!(error.to_string(), message);
    }
}

#[tokio::test]
async fn api_error_has_the_request_id_and_the_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(json_response(400, json!({"message": "bad"})).insert_header("x-typesafe-request-id", "req_err"))
        .mount(&server)
        .await;
    let error = client(&server).system_one(request(), Default::default()).await.unwrap_err();
    let api = error.as_api_error().unwrap();
    assert_eq!(api.request_id.as_deref(), Some("req_err"));
    assert_eq!(api.headers.get("x-typesafe-request-id").unwrap(), "req_err");

    let without = models_server(404, json!({})).await;
    let error = client(&without).models().list(Default::default()).await.unwrap_err();
    assert_eq!(error.as_api_error().unwrap().request_id, None);
}

#[tokio::test]
async fn rate_limit_error_gives_retry_after_in_milliseconds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(json_response(429, json!({})).insert_header("retry-after", "7"))
        .mount(&server)
        .await;
    let error = client_without_retries(&server).models().list(Default::default()).await.unwrap_err();
    let api = error.as_api_error().unwrap();
    assert_eq!(api.kind(), ApiErrorKind::RateLimit);
    assert_eq!(api.retry_after_ms(), Some(7000));
}

#[tokio::test]
async fn rate_limit_error_without_retry_after_gives_none() {
    let server = models_server(429, json!({})).await;
    let error = client_without_retries(&server).models().list(Default::default()).await.unwrap_err();
    assert_eq!(error.as_api_error().unwrap().retry_after_ms(), None);
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

#[tokio::test]
async fn models_list_parses_the_documented_response() {
    let server = models_server(200, models_response()).await;
    let models = client(&server).models().list(Default::default()).await.unwrap();
    assert_eq!(
        models,
        [ModelCard {
            name: "m".into(),
            description: "d".into(),
            release_date: "2026".into()
        }]
    );

    let empty = models_server(200, json!({"models": []})).await;
    let listed = client(&empty).models().list(Default::default()).with_response().await.unwrap();
    assert!(listed.data.is_empty());
    assert_eq!(listed.response.status, 200);
}

#[tokio::test]
async fn models_list_fails_clearly_on_a_wrong_shape() {
    for wire in [
        Value::Null,
        json!([]),
        json!({"models": {"models": []}}),
        json!({"models": null}),
        json!({"models": "bad"}),
        json!({"ok": true}),
    ] {
        let server = models_server(200, wire.clone()).await;
        let error = client(&server).models().list(Default::default()).await.unwrap_err();
        assert!(matches!(error, Error::TypeSafe(_)), "{wire}");
        assert!(
            error.to_string().starts_with("Unexpected response shape from GET /v1/models"),
            "{wire}: {error}"
        );
    }
}

#[tokio::test]
async fn models_list_keeps_unknown_fields_in_the_raw_response() {
    let wire = json!({"models": [{"name": "m", "description": "d", "release_date": "2026", "tags": ["internal"]}]});
    let server = models_server(200, wire.clone()).await;
    let raw = client(&server).models().list(Default::default()).as_response().await.unwrap();
    assert_eq!(raw.parsed_body(), Some(wire));
}
