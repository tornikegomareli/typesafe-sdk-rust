use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Method;
use serde_json::{json, Value};

use crate::env::{self, from_code_or_env, read_env};
use crate::errors::{ApiError, Error, Result};
use crate::logging::{parse_log_level, redact_headers, with_level, ConsoleLogger, LeveledLogger, LogLevel, Logger, DEFAULT_LOG_LEVEL};
use crate::models::Models;
use crate::questions::validate_questions;
use crate::response::{request_id_from, ApiRequest, RawResponse};
use crate::retry::{retry_delay_ms, sleep, RetryOverrides, RetryPolicy, DEFAULT_TIMEOUT_MS};
use crate::runtime::describe_runtime;
use crate::types::{RequestOptions, SystemOneRequest, SystemOneResult};
use crate::version::VERSION;

pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";
pub const DEFAULT_MODEL: &str = "jev-latest";

/// Client options. Explicit values take precedence over environment variables, then SDK defaults.
#[derive(Clone, Default)]
pub struct TypeSafeClientConfig {
    /// Required API key; falls back to `TYPESAFE_API_KEY`.
    pub api_key: Option<String>,
    /// API root; falls back to `TYPESAFE_BASE_URL`, then `https://api.typesafe.ai`.
    pub base_url: Option<String>,
    /// Default model; falls back to `TYPESAFE_DEFAULT_MODEL`, then `jev-latest`.
    pub default_model: Option<String>,
    /// Log level; falls back to `TYPESAFE_LOG_LEVEL`, then `warn`.
    /// `info` logs request summaries; `debug` adds headers and bodies.
    /// Known credential headers are redacted; bodies are not.
    pub log_level: Option<LogLevel>,
    /// Logger filtered to `log_level` and above. Default: `ConsoleLogger`.
    pub logger: Option<Arc<dyn Logger>>,
    /// Retry overrides; omitted fields use the defaults in `RetryPolicy`.
    pub retry: Option<RetryOverrides>,
    /// Timeout per attempt in milliseconds, without a total retry budget. Default: 10000.
    pub timeout_ms: Option<u64>,
    /// Additional request headers; per-call headers take precedence.
    pub default_headers: Vec<(String, String)>,
    /// Custom HTTP client for transport configuration or tests. Default: a new `reqwest::Client`.
    pub http_client: Option<reqwest::Client>,
}

fn positive_ms(name: &str, value: u64) -> Result<u64> {
    if value == 0 {
        return Err(Error::TypeSafe(format!(
            "`{name}` must be a positive number of milliseconds, got {value}."
        )));
    }
    Ok(value)
}

/// Last value wins regardless of casing; `None` removes a header.
fn merge_headers(sources: &[&[(String, Option<String>)]]) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String, String)> = Vec::new();
    for (name, value) in sources.iter().flat_map(|source| source.iter()) {
        let key = name.to_lowercase();
        entries.retain(|(existing, _, _)| *existing != key);
        if let Some(value) = value {
            entries.push((key, name.clone(), value.clone()));
        }
    }
    entries.into_iter().map(|(_, name, value)| (name, value)).collect()
}

struct Inner {
    /// API key excluded from `Debug` and public accessors.
    api_key: String,
    base_url: String,
    default_model: String,
    log_level: LogLevel,
    logger: LeveledLogger,
    retry: RetryPolicy,
    timeout_ms: u64,
    default_headers: Vec<(String, String)>,
    http: reqwest::Client,
    runtime: String,
    request_count: AtomicU64,
}

/// Client for the TypeSafe AI API. A clone shares the connection pool.
#[derive(Clone)]
pub struct TypeSafeClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for TypeSafeClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypeSafeClient")
            .field("base_url", &self.inner.base_url)
            .field("default_model", &self.inner.default_model)
            .field("timeout_ms", &self.inner.timeout_ms)
            .finish_non_exhaustive()
    }
}

struct ResolvedRequest {
    method: Method,
    path: String,
    body: Option<Value>,
    headers: Vec<(String, String)>,
    signal: Option<tokio_util::sync::CancellationToken>,
    timeout_ms: u64,
    retry: RetryPolicy,
}

impl TypeSafeClient {
    /// Create a client for the TypeSafe AI API.
    ///
    /// Explicit options take precedence over environment variables, then SDK defaults.
    /// Empty or whitespace-only environment values are ignored.
    ///
    /// Returns `Error::TypeSafe` when the API key is missing or the configuration is invalid.
    pub fn new(config: TypeSafeClientConfig) -> Result<Self> {
        let api_key = from_code_or_env(config.api_key, env::API_KEY).ok_or_else(|| {
            Error::TypeSafe(format!(
                "No API key was provided. Pass `api_key` to the TypeSafeClient constructor or set the {} environment variable.",
                env::API_KEY
            ))
        })?;
        let base_url = from_code_or_env(config.base_url, env::BASE_URL).unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let log_level = match config.log_level {
            Some(level) => level,
            None => match read_env(env::LOG_LEVEL) {
                Some(value) => parse_log_level(&value, env::LOG_LEVEL)?,
                None => DEFAULT_LOG_LEVEL,
            },
        };
        let sink: Arc<dyn Logger> = config.logger.unwrap_or_else(|| Arc::new(ConsoleLogger));

        Ok(TypeSafeClient {
            inner: Arc::new(Inner {
                api_key,
                base_url: base_url.trim_end_matches('/').to_string(),
                default_model: from_code_or_env(config.default_model, env::DEFAULT_MODEL).unwrap_or_else(|| DEFAULT_MODEL.to_string()),
                log_level,
                logger: with_level(sink, log_level),
                retry: RetryPolicy::default().resolve(config.retry.as_ref())?,
                timeout_ms: positive_ms("timeout", config.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS))?,
                default_headers: config.default_headers,
                http: config.http_client.unwrap_or_default(),
                runtime: describe_runtime(),
                request_count: AtomicU64::new(0),
            }),
        })
    }

    /// API root with trailing slashes removed.
    pub fn base_url(&self) -> &str {
        &self.inner.base_url
    }

    /// Model used when a request omits `model`.
    pub fn default_model(&self) -> &str {
        &self.inner.default_model
    }

    /// Configured log verbosity.
    pub fn log_level(&self) -> LogLevel {
        self.inner.log_level
    }

    /// Retry settings with constructor overrides applied.
    pub fn retry(&self) -> &RetryPolicy {
        &self.inner.retry
    }

    /// Timeout per attempt in milliseconds.
    pub fn timeout_ms(&self) -> u64 {
        self.inner.timeout_ms
    }

    /// The models available to the account.
    pub fn models(&self) -> Models<'_> {
        Models { client: self }
    }

    /// Answer named questions about text or structured state.
    ///
    /// The result has the answers keyed by question name, with model and token usage.
    ///
    /// Errors: `Error::TypeSafe` when the questions are empty or a score question has fewer than two
    /// criteria; `Error::Api` when the server returns a non-2xx response after retries;
    /// `Error::Connection` or `Error::Timeout` when the request cannot connect or times out after
    /// retries; `Error::UserAbort` when the caller cancels the request.
    pub fn system_one(&self, request: SystemOneRequest, options: RequestOptions) -> ApiRequest<SystemOneResult> {
        if let Err(error) = validate_questions(&request.questions) {
            return ApiRequest::new(Box::pin(async move { Err(error) }), Box::new(|_| unreachable!()));
        }
        let model = request.model.clone().unwrap_or_else(|| self.inner.default_model.clone());
        let mut body = json!(request);
        body["model"] = Value::String(model);

        self.request(Method::POST, "/v1/systemone", Some(body), options).map(|parsed| {
            serde_json::from_value(parsed.unwrap_or(Value::Null))
                .map_err(|error| Error::TypeSafe(format!("Unexpected response shape from POST /v1/systemone: {error}.")))
        })
    }

    /// Send a request and parse its response body.
    pub(crate) fn request(&self, method: Method, path: &str, body: Option<Value>, options: RequestOptions) -> ApiRequest<Option<Value>> {
        let client = self.clone();
        let path = path.to_string();
        // Numbered so concurrent requests, and the attempts within one, can be told apart in the logs.
        let tag = format!("#{} {method} {path}", self.inner.request_count.fetch_add(1, Ordering::Relaxed) + 1);
        let parse_tag = tag.clone();
        let parse_client = self.clone();

        let response = async move {
            let defaults: Vec<(String, Option<String>)> = client
                .inner
                .default_headers
                .iter()
                .map(|(name, value)| (name.clone(), Some(value.clone())))
                .collect();
            let resolved = ResolvedRequest {
                method,
                path,
                body,
                headers: merge_headers(&[&defaults, &options.headers]),
                signal: options.signal,
                timeout_ms: match options.timeout_ms {
                    Some(timeout) => positive_ms("timeout", timeout)?,
                    None => client.inner.timeout_ms,
                },
                retry: client.inner.retry.resolve(options.retry.as_ref())?,
            };
            client.fetch_with_retries(&tag, resolved).await
        };
        ApiRequest::new(
            Box::pin(response),
            Box::new(move |response| {
                let parsed = response.parsed_body();
                parse_client.inner.logger.debug(&format!("{parse_tag} <- body"), parsed.as_ref());
                Ok(parsed)
            }),
        )
    }

    /// Retry eligible failures, logging attempt summaries at `info` and headers and bodies at `debug`.
    async fn fetch_with_retries(&self, tag: &str, request: ResolvedRequest) -> Result<RawResponse> {
        let logger = &self.inner.logger;
        let url = format!("{}{}", self.inner.base_url, request.path);
        // User-supplied headers go first so they can't clobber auth or the JSON content type.
        let user: Vec<(String, Option<String>)> = request
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), Some(value.clone())))
            .collect();
        let sdk = format!("typesafe-sdk-rust/{VERSION}");
        let protected = [
            ("Authorization".to_string(), Some(format!("Bearer {}", self.inner.api_key))),
            ("Accept".to_string(), Some("application/json".to_string())),
            ("User-Agent".to_string(), Some(sdk.clone())),
            ("X-TypeSafe-SDK".to_string(), Some(sdk)),
            ("X-TypeSafe-Runtime".to_string(), Some(self.inner.runtime.clone())),
            (
                "Content-Type".to_string(),
                request.body.as_ref().map(|_| "application/json".to_string()),
            ),
            ("X-TypeSafe-Retry-Count".to_string(), None),
        ];
        let headers = merge_headers(&[&user, &protected]);
        let body = request.body.as_ref().map(Value::to_string);

        let mut attempt: u32 = 0;
        loop {
            let retries_left = request.retry.max_retries.saturating_sub(attempt);
            let mut attempt_headers = headers.clone();
            if attempt > 0 {
                attempt_headers.push(("X-TypeSafe-Retry-Count".to_string(), attempt.to_string()));
            }
            if logger.enabled(LogLevel::Debug) {
                let shown: serde_json::Map<String, Value> = redact_headers(&attempt_headers)
                    .into_iter()
                    .map(|(name, value)| (name, Value::String(value)))
                    .collect();
                logger.debug(&format!("{tag} -> {url}"), Some(&json!({"headers": shown, "body": request.body})));
            }

            let started = Instant::now();
            let response = match self.attempt(tag, &url, &attempt_headers, body.clone(), &request).await {
                Ok(response) => response,
                Err(error) => {
                    if matches!(error, Error::UserAbort) || retries_left == 0 || !request.retry.is_retryable_error(&error) {
                        return Err(error);
                    }
                    self.back_off(tag, attempt, retries_left, &error.to_string(), None, &request)
                        .await?;
                    attempt += 1;
                    continue;
                }
            };

            let request_id = request_id_from(&response.headers)
                .map(|id| format!(" (request {id})"))
                .unwrap_or_default();
            logger.info(
                &format!("{tag} <- {} in {}ms{request_id}", response.status, started.elapsed().as_millis()),
                None,
            );
            if (200..300).contains(&response.status) {
                return Ok(response);
            }

            let error_body = response.parsed_body();
            logger.debug(&format!("{tag} <- error body"), error_body.as_ref());
            let status = response.status;
            if retries_left == 0 || !request.retry.is_retryable_status(status) {
                return Err(Error::Api(Box::new(ApiError::from_response(status, error_body, response.headers))));
            }
            self.back_off(tag, attempt, retries_left, &status.to_string(), Some(&response.headers), &request)
                .await?;
            attempt += 1;
        }
    }

    /// One HTTP round trip, including body delivery, with a timeout. The cancellation of the caller
    /// wins over the timeout.
    async fn attempt(
        &self,
        tag: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<String>,
        request: &ResolvedRequest,
    ) -> Result<RawResponse> {
        let logger = &self.inner.logger;
        let mut header_map = HeaderMap::new();
        for (name, value) in headers {
            let name =
                HeaderName::from_bytes(name.as_bytes()).map_err(|_| Error::TypeSafe(format!("The header name `{name}` is not valid.")))?;
            let value =
                HeaderValue::from_str(value).map_err(|_| Error::TypeSafe(format!("The value of the header `{name}` is not valid.")))?;
            header_map.insert(name, value);
        }
        let mut builder = self.inner.http.request(request.method.clone(), url).headers(header_map);
        if let Some(body) = body {
            builder = builder.body(body);
        }

        let started = Instant::now();
        let round_trip = async {
            let response = builder.send().await?;
            let (status, headers) = (response.status().as_u16(), response.headers().clone());
            let body = response.bytes().await?.to_vec();
            Ok::<RawResponse, reqwest::Error>(RawResponse { status, headers, body })
        };
        let cancelled = async {
            match &request.signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending().await,
            }
        };

        let elapsed = || format!("{}ms", started.elapsed().as_millis());
        tokio::select! {
            biased;
            _ = cancelled => {
                logger.info(&format!("{tag} aborted by caller after {}", elapsed()), None);
                Err(Error::UserAbort)
            }
            outcome = tokio::time::timeout(Duration::from_millis(request.timeout_ms), round_trip) => match outcome {
                Err(_) => {
                    logger.info(&format!("{tag} timed out after {}", elapsed()), None);
                    Err(Error::Timeout { timeout_ms: request.timeout_ms, source: None })
                }
                Ok(Err(error)) => {
                    logger.info(&format!("{tag} connection error after {}", elapsed()), Some(&Value::String(error.to_string())));
                    Err(Error::connection(Some(Box::new(error))))
                }
                Ok(Ok(response)) => Ok(response),
            }
        }
    }

    /// Wait before retrying; caller cancellation gives `Error::UserAbort`.
    async fn back_off(
        &self,
        tag: &str,
        attempt: u32,
        retries_left: u32,
        reason: &str,
        headers: Option<&HeaderMap>,
        request: &ResolvedRequest,
    ) -> Result<()> {
        let logger = &self.inner.logger;
        let delay = retry_delay_ms(attempt, headers, &request.retry, fastrand::f64);
        let (nth, total) = (attempt + 1, attempt + retries_left);
        logger.info(&format!("{tag} retrying in {delay}ms (retry {nth}/{total}) after {reason}"), None);
        sleep(delay, request.signal.as_ref()).await.inspect_err(|_| {
            logger.info(&format!("{tag} aborted by caller while waiting to retry"), None);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(name: &str, value: Option<&str>) -> (String, Option<String>) {
        (name.to_string(), value.map(str::to_string))
    }

    #[test]
    fn merges_headers_without_regard_to_case() {
        let first = [pair("X-Custom", Some("a")), pair("authorization", Some("user value"))];
        let second = [pair("Authorization", Some("Bearer key")), pair("x-custom", None)];
        assert_eq!(
            merge_headers(&[&first, &second]),
            [("Authorization".to_string(), "Bearer key".to_string())]
        );
    }
}
