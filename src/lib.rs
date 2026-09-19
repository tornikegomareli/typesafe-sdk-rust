//! Community Rust SDK for the [TypeSafe AI](https://typesafe.ai) API.
//!
//! A port of the official JavaScript SDK, `@typesafe-ai/sdk`.
//!
//! ```no_run
//! use typesafe::{choice, Questions, SystemOneRequest, TypeSafeClient};
//!
//! # async fn example() -> typesafe::Result<()> {
//! let client = TypeSafeClient::new(Default::default())?;
//! let mut questions = Questions::new();
//! questions.insert(
//!     "category".into(),
//!     choice("What is this ticket about?", [("billing", ()), ("technical", ()), ("other", ())]),
//! );
//! let result = client
//!     .system_one(SystemOneRequest::new("I was charged twice. Please fix this ASAP.", questions), Default::default())
//!     .await?;
//! println!("{}", result.choice("category").unwrap().choice);
//! # Ok(())
//! # }
//! ```

mod client;
pub mod env;
mod errors;
mod logging;
mod models;
mod questions;
mod response;
mod retry;
mod runtime;
mod types;
mod version;

pub use client::{TypeSafeClient, TypeSafeClientConfig, DEFAULT_BASE_URL, DEFAULT_MODEL};
pub use errors::{ApiError, ApiErrorKind, Error, Result};
pub use logging::{parse_log_level, redact_headers, with_level, ConsoleLogger, LeveledLogger, LogLevel, Logger, LOG_LEVELS};
pub use models::Models;
pub use questions::{choice, noul, noul_with_criteria, score, validate_questions, NoulCriteria, Question, Questions};
pub use response::{ApiRequest, RawResponse, WithResponse, REQUEST_ID_HEADER};
pub use retry::{parse_retry_after, retry_delay_ms, RetryOverrides, RetryPolicy, DEFAULT_TIMEOUT_MS};
pub use runtime::describe_runtime;
pub use types::{Answer, ChoiceResponse, ModelCard, NoulResponse, RequestOptions, ScoreResponse, SystemOneRequest, SystemOneResult, Usage};
pub use version::VERSION;

pub use tokio_util::sync::CancellationToken;
