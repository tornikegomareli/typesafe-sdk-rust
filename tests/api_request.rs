//! Port of `test/api-promise.test.ts`: the three ways to consume an `ApiRequest`.

mod common;

use common::*;
use serde_json::json;
use typesafe::{ApiErrorKind, Error};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mock server that answers `GET /v1/models` with the response.
async fn models_server(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(response)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn await_gives_the_parsed_data() {
    let server = models_server(json_response(200, models_response())).await;
    let models = client_without_retries(&server).models().list(Default::default()).await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "m");
    assert_eq!(models[0].description, "d");
    assert_eq!(models[0].release_date, "2026");
}

#[tokio::test]
async fn with_response_gives_data_status_headers_and_request_id() {
    let server = models_server(json_response(200, models_response()).insert_header("x-typesafe-request-id", "req_abc")).await;
    let result = client_without_retries(&server)
        .models()
        .list(Default::default())
        .with_response()
        .await
        .unwrap();

    assert_eq!(result.data[0].name, "m");
    assert_eq!(result.response.status, 200);
    assert_eq!(result.response.headers.get("x-typesafe-request-id").unwrap(), "req_abc");
    assert_eq!(result.response.headers.get("content-type").unwrap(), "application/json");
    assert_eq!(result.response.parsed_body(), Some(models_response()));
    assert_eq!(result.request_id.as_deref(), Some("req_abc"));
    assert_eq!(received(&server).await.len(), 1);
}

#[tokio::test]
async fn with_response_gives_no_request_id_when_the_header_is_absent() {
    let server = models_server(json_response(200, models_response())).await;
    let result = client_without_retries(&server)
        .models()
        .list(Default::default())
        .with_response()
        .await
        .unwrap();
    assert_eq!(result.request_id, None);
}

#[tokio::test]
async fn with_response_on_system_one_gives_the_result_and_the_request_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(json_response(200, system_one_response()).insert_header("x-typesafe-request-id", "req_1"))
        .mount(&server)
        .await;
    let result = client(&server)
        .system_one(request(), Default::default())
        .with_response()
        .await
        .unwrap();
    assert_eq!(result.data.model, "m");
    assert_eq!(result.data.noul("q1").unwrap().noul, 0.5);
    assert_eq!(result.request_id.as_deref(), Some("req_1"));
    assert_eq!(received(&server).await.len(), 1);
}

#[tokio::test]
async fn as_response_gives_the_raw_body() {
    let server = models_server(json_response(200, models_response())).await;
    let response = client_without_retries(&server)
        .models()
        .list(Default::default())
        .as_response()
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&response.body).unwrap(),
        models_response()
    );
}

#[tokio::test]
async fn as_response_does_not_parse_the_body() {
    // The shape is wrong for `models().list()`, but the raw response is still available.
    let server = models_server(ResponseTemplate::new(200).set_body_string("not json")).await;
    let response = client_without_retries(&server)
        .models()
        .list(Default::default())
        .as_response()
        .await
        .unwrap();
    assert_eq!(response.body, b"not json");
    assert_eq!(response.parsed_body(), Some(json!("not json")));
}

#[tokio::test]
async fn non_2xx_response_gives_an_api_error_through_all_three() {
    let server = models_server(json_response(404, json!({"message": "nope"}))).await;
    let client = client_without_retries(&server);
    let check = |error: Error| {
        assert!(matches!(error, Error::Api(_)), "{error:?}");
        let api = error.as_api_error().unwrap();
        assert_eq!(api.kind(), ApiErrorKind::NotFound);
        assert_eq!(api.status, 404);
        assert_eq!(error.to_string(), "404 nope");
    };
    check(client.models().list(Default::default()).await.unwrap_err());
    check(client.models().list(Default::default()).with_response().await.unwrap_err());
    check(client.models().list(Default::default()).as_response().await.unwrap_err());
}

#[tokio::test]
async fn map_transforms_the_data_and_shares_the_response() {
    let server = models_server(json_response(200, models_response()).insert_header("x-typesafe-request-id", "req_m")).await;
    let client = client_without_retries(&server);
    let names = |models: Vec<typesafe::ModelCard>| Ok(models.into_iter().map(|model| model.name).collect::<Vec<_>>());

    assert_eq!(client.models().list(Default::default()).map(names).await.unwrap(), ["m"]);

    let result = client.models().list(Default::default()).map(names).with_response().await.unwrap();
    assert_eq!(result.data, ["m"]);
    assert_eq!(result.request_id.as_deref(), Some("req_m"));
    assert_eq!(result.response.status, 200);
    // One HTTP request for each awaited `ApiRequest`.
    assert_eq!(received(&server).await.len(), 2);
}

#[tokio::test]
async fn request_starts_when_it_is_awaited() {
    let server = models_server(json_response(200, models_response())).await;
    let client = client_without_retries(&server);

    let pending = client.models().list(Default::default());
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(received(&server).await.is_empty());

    pending.await.unwrap();
    assert_eq!(received(&server).await.len(), 1);
}
