use std::future::{Future, IntoFuture};
use std::pin::Pin;

use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::errors::Result;

pub const REQUEST_ID_HEADER: &str = "x-typesafe-request-id";

pub fn request_id_from(headers: &HeaderMap) -> Option<String> {
    headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// An HTTP response with its full body. The SDK reads the body under the request timeout.
#[derive(Debug, Clone)]
pub struct RawResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl RawResponse {
    /// The body as parsed JSON, as text in a JSON string, or `None` for an empty body.
    ///
    /// It is lenient: servers and proxies do not always set the content type.
    pub fn parsed_body(&self) -> Option<Value> {
        if self.body.is_empty() {
            return None;
        }
        let text = String::from_utf8_lossy(&self.body);
        Some(serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.into_owned())))
    }
}

/// Parsed data with its HTTP response and request ID.
#[derive(Debug, Clone)]
pub struct WithResponse<T> {
    /// The parsed response body.
    pub data: T,
    /// The HTTP response.
    pub response: RawResponse,
    /// Request ID from `x-typesafe-request-id`, or `None` when absent.
    pub request_id: Option<String>,
}

type Pending<T> = Pin<Box<dyn Future<Output = Result<T>> + Send>>;
type Parse<T> = Box<dyn FnOnce(&RawResponse) -> Result<T> + Send>;

/// A request for the parsed result with access to the HTTP response.
///
/// `.await` gives the parsed result. The request starts when it is awaited, not when it is created.
/// An unsuccessful response gives `Error::Api`, including through `as_response()`.
pub struct ApiRequest<T> {
    response: Pending<RawResponse>,
    parse: Parse<T>,
}

impl<T: Send + 'static> ApiRequest<T> {
    pub(crate) fn new(response: Pending<RawResponse>, parse: Parse<T>) -> Self {
        ApiRequest { response, parse }
    }

    /// Resolves to the raw response without parsing the body.
    pub async fn as_response(self) -> Result<RawResponse> {
        self.response.await
    }

    /// Return the parsed result, HTTP response, and request ID.
    pub async fn with_response(self) -> Result<WithResponse<T>> {
        let response = self.response.await?;
        let data = (self.parse)(&response)?;
        let request_id = request_id_from(&response.headers);
        Ok(WithResponse {
            data,
            response,
            request_id,
        })
    }

    /// Transform the parsed result, sharing the HTTP response and a single body parse.
    pub fn map<U: Send + 'static>(self, transform: impl FnOnce(T) -> Result<U> + Send + 'static) -> ApiRequest<U> {
        let parse = self.parse;
        ApiRequest {
            response: self.response,
            parse: Box::new(move |response| transform(parse(response)?)),
        }
    }
}

impl<T: Send + 'static> IntoFuture for ApiRequest<T> {
    type Output = Result<T>;
    type IntoFuture = Pending<T>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move { Ok(self.with_response().await?.data) })
    }
}
