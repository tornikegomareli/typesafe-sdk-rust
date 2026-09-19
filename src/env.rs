//! Environment variable names for client configuration. Explicit options take precedence.

/// Required API key; used when `api_key` is omitted.
pub const API_KEY: &str = "TYPESAFE_API_KEY";
/// API root; defaults to `https://api.typesafe.ai`.
pub const BASE_URL: &str = "TYPESAFE_BASE_URL";
/// Default model name; defaults to `jev-latest`.
pub const DEFAULT_MODEL: &str = "TYPESAFE_DEFAULT_MODEL";
/// Log level; defaults to `warn`.
pub const LOG_LEVEL: &str = "TYPESAFE_LOG_LEVEL";

/// Read a trimmed environment value, returning `None` for missing or blank values.
pub(crate) fn read_env(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Return the explicit value, falling back to the environment.
pub(crate) fn from_code_or_env(from_code: Option<String>, name: &str) -> Option<String> {
    from_code.or_else(|| read_env(name))
}
