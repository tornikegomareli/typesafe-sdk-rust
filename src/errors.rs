use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::response::request_id_from;
use crate::retry::parse_retry_after;

/// The result type of this SDK.
pub type Result<T> = std::result::Result<T, Error>;

type Source = Box<dyn std::error::Error + Send + Sync>;

/// All errors of this SDK.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The configuration or the request is invalid. No request was sent.
    #[error("{0}")]
    TypeSafe(String),
    /// An unsuccessful HTTP response from the API.
    #[error("{0}")]
    Api(Box<ApiError>),
    /// The request or response-body delivery failed (DNS, TLS, connection closed, etc.).
    #[error("{message}")]
    Connection {
        message: String,
        #[source]
        source: Option<Source>,
    },
    /// The full response did not arrive within the timeout. A kind of connection error.
    #[error("Request timed out after {timeout_ms}ms.")]
    Timeout {
        /// Configured timeout in milliseconds.
        timeout_ms: u64,
        #[source]
        source: Option<Source>,
    },
    /// The caller cancelled the request through a `CancellationToken`.
    #[error("Request was aborted.")]
    UserAbort,
}

impl Error {
    pub(crate) fn connection(source: Option<Source>) -> Self {
        let message = match &source {
            Some(source) => format!("Connection error: {source}"),
            None => "Connection error.".to_string(),
        };
        Error::Connection { message, source }
    }

    /// `true` for a connection error and for a timeout, which is a kind of connection error.
    pub fn is_connection_error(&self) -> bool {
        matches!(self, Error::Connection { .. } | Error::Timeout { .. })
    }

    /// The API error, when the server returned an unsuccessful response.
    pub fn as_api_error(&self) -> Option<&ApiError> {
        match self {
            Error::Api(error) => Some(error),
            _ => None,
        }
    }
}

/// The class of an unsuccessful HTTP response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorKind {
    /// HTTP 400: the request is invalid.
    BadRequest,
    /// HTTP 401: authentication failed.
    Authentication,
    /// HTTP 403: access is denied.
    PermissionDenied,
    /// HTTP 404: the resource was not found.
    NotFound,
    /// HTTP 422: request validation failed.
    UnprocessableEntity,
    /// HTTP 429: the rate limit was exceeded.
    RateLimit,
    /// HTTP 5xx: the server failed to handle the request.
    InternalServer,
    /// Any other status.
    Other,
}

const MAX_RAW_BODY_IN_MESSAGE: usize = 200;

/// An unsuccessful HTTP response from the API.
#[derive(Debug)]
pub struct ApiError {
    /// HTTP response status code.
    pub status: u16,
    /// HTTP response headers.
    pub headers: HeaderMap,
    /// Parsed JSON, response text as a JSON string, or `None` for an empty body.
    pub body: Option<Value>,
    /// Request ID from `x-typesafe-request-id`, or `None` when absent.
    pub request_id: Option<String>,
    message: String,
}

impl ApiError {
    /// Create the error for an HTTP status code.
    pub fn from_response(status: u16, body: Option<Value>, headers: HeaderMap) -> Self {
        let message = describe(status, body.as_ref());
        let request_id = request_id_from(&headers);
        ApiError {
            status,
            headers,
            body,
            request_id,
            message,
        }
    }

    pub fn kind(&self) -> ApiErrorKind {
        match self.status {
            400 => ApiErrorKind::BadRequest,
            401 => ApiErrorKind::Authentication,
            403 => ApiErrorKind::PermissionDenied,
            404 => ApiErrorKind::NotFound,
            422 => ApiErrorKind::UnprocessableEntity,
            429 => ApiErrorKind::RateLimit,
            status if status >= 500 => ApiErrorKind::InternalServer,
            _ => ApiErrorKind::Other,
        }
    }

    /// Server retry delay in milliseconds of a rate limit error, or `None` when absent or invalid.
    pub fn retry_after_ms(&self) -> Option<u64> {
        (self.kind() == ApiErrorKind::RateLimit)
            .then(|| parse_retry_after(&self.headers, None))
            .flatten()
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

fn describe(status: u16, body: Option<&Value>) -> String {
    let Some(body) = body else {
        return format!("{status} status code (no body)");
    };
    if let Some(detail) = extract_message(body) {
        return format!("{status} {detail}");
    }
    let raw = match body {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    if raw.chars().count() > MAX_RAW_BODY_IN_MESSAGE {
        format!("{status} {}…", raw.chars().take(MAX_RAW_BODY_IN_MESSAGE).collect::<String>())
    } else {
        format!("{status} {raw}")
    }
}

/// Extract a message from a text, error, or validation response body.
fn extract_message(body: &Value) -> Option<String> {
    if let Value::String(text) = body {
        return (!text.is_empty()).then(|| text.clone());
    }
    let record = body.as_object()?;
    let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_string);
    let nested = |value: Option<&Value>| text(value.and_then(Value::as_object).and_then(|object| object.get("message")));

    let (error, message, detail) = (record.get("error"), record.get("message"), record.get("detail"));
    text(error)
        .or_else(|| nested(error))
        .or_else(|| text(message))
        .or_else(|| text(detail))
        .or_else(|| nested(detail))
        .or_else(|| {
            detail
                .and_then(Value::as_array)
                .and_then(|errors| describe_validation_errors(errors))
        })
}

/// Format validation errors as semicolon-separated `path: message` entries.
fn describe_validation_errors(errors: &[Value]) -> Option<String> {
    let parts: Vec<String> = errors
        .iter()
        .filter_map(|error| {
            let message = error.get("msg")?.as_str()?;
            let location = error
                .get("loc")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter(|part| part.as_str() != Some("body"))
                        .map(|part| part.as_str().map(str::to_string).unwrap_or_else(|| part.to_string()))
                        .collect::<Vec<_>>()
                        .join(".")
                })
                .unwrap_or_default();
            Some(if location.is_empty() {
                message.to_string()
            } else {
                format!("{location}: {message}")
            })
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message(status: u16, body: Option<Value>) -> String {
        ApiError::from_response(status, body, HeaderMap::new()).to_string()
    }

    #[test]
    fn describes_the_body() {
        assert_eq!(message(500, None), "500 status code (no body)");
        assert_eq!(message(400, Some(json!("plain text"))), "400 plain text");
        assert_eq!(message(401, Some(json!({"error": "bad key"}))), "401 bad key");
        assert_eq!(message(401, Some(json!({"error": {"message": "nested"}}))), "401 nested");
        assert_eq!(message(400, Some(json!({"message": "top"}))), "400 top");
        assert_eq!(message(400, Some(json!({"detail": "detail text"}))), "400 detail text");
        assert_eq!(message(400, Some(json!({"detail": {"message": "in detail"}}))), "400 in detail");
        assert_eq!(message(418, Some(json!({"other": 1}))), "418 {\"other\":1}");
    }

    #[test]
    fn describes_validation_errors() {
        let body = json!({"detail": [
            {"loc": ["body", "questions", "q1"], "msg": "field required"},
            {"loc": ["body"], "msg": "invalid"},
            {"no_msg": true},
        ]});
        assert_eq!(message(422, Some(body)), "422 questions.q1: field required; invalid");
    }

    #[test]
    fn truncates_a_long_raw_body() {
        let text = message(418, Some(json!({"data": "x".repeat(400)})));
        assert!(text.ends_with('…'));
        assert_eq!(text.chars().count(), "418 ".len() + MAX_RAW_BODY_IN_MESSAGE + 1);
    }

    #[test]
    fn maps_the_status_to_a_kind() {
        let kind = |status| ApiError::from_response(status, None, HeaderMap::new()).kind();
        assert_eq!(kind(400), ApiErrorKind::BadRequest);
        assert_eq!(kind(401), ApiErrorKind::Authentication);
        assert_eq!(kind(403), ApiErrorKind::PermissionDenied);
        assert_eq!(kind(404), ApiErrorKind::NotFound);
        assert_eq!(kind(422), ApiErrorKind::UnprocessableEntity);
        assert_eq!(kind(429), ApiErrorKind::RateLimit);
        assert_eq!(kind(503), ApiErrorKind::InternalServer);
        assert_eq!(kind(418), ApiErrorKind::Other);
    }

    #[test]
    fn reads_retry_after_only_for_a_rate_limit() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after-ms", "1500".parse().unwrap());
        assert_eq!(ApiError::from_response(429, None, headers.clone()).retry_after_ms(), Some(1500));
        assert_eq!(ApiError::from_response(500, None, headers).retry_after_ms(), None);
    }
}
