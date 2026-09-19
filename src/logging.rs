use std::sync::Arc;

use serde_json::Value;

use crate::errors::{Error, Result};

/// Log verbosity; `Off` disables logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
    Off,
}

/// Supported log levels, from most to least verbose.
pub const LOG_LEVELS: [LogLevel; 5] = [LogLevel::Debug, LogLevel::Info, LogLevel::Warn, LogLevel::Error, LogLevel::Off];

pub const DEFAULT_LOG_LEVEL: LogLevel = LogLevel::Warn;

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
            LogLevel::Off => "off",
        }
    }
}

/// Validate a configured log level, returning `Error::TypeSafe` for unknown values.
pub fn parse_log_level(value: &str, source: &str) -> Result<LogLevel> {
    LOG_LEVELS.into_iter().find(|level| level.as_str() == value).ok_or_else(|| {
        let expected: Vec<&str> = LOG_LEVELS.iter().map(|level| level.as_str()).collect();
        Error::TypeSafe(format!(
            "Invalid log level \"{value}\" from {source}. Expected one of: {}.",
            expected.join(", ")
        ))
    })
}

// ---------------------------------------------------------------------------
// Loggers
// ---------------------------------------------------------------------------

/// Log methods accepting a message and a structured value.
pub trait Logger: Send + Sync {
    fn debug(&self, message: &str, data: Option<&Value>);
    fn info(&self, message: &str, data: Option<&Value>);
    fn warn(&self, message: &str, data: Option<&Value>);
    fn error(&self, message: &str, data: Option<&Value>);
}

const PREFIX: &str = "[typesafe-sdk]";

/// Default logger. It writes to standard error with the `[typesafe-sdk]` prefix.
pub struct ConsoleLogger;

impl ConsoleLogger {
    fn write(level: &str, message: &str, data: Option<&Value>) {
        match data {
            Some(data) => eprintln!("{PREFIX} {level}: {message} {data}"),
            None => eprintln!("{PREFIX} {level}: {message}"),
        }
    }
}

impl Logger for ConsoleLogger {
    fn debug(&self, message: &str, data: Option<&Value>) {
        Self::write("debug", message, data);
    }
    fn info(&self, message: &str, data: Option<&Value>) {
        Self::write("info", message, data);
    }
    fn warn(&self, message: &str, data: Option<&Value>) {
        Self::write("warn", message, data);
    }
    fn error(&self, message: &str, data: Option<&Value>) {
        Self::write("error", message, data);
    }
}

/// A logger that filters the calls to the configured level and above.
pub struct LeveledLogger {
    sink: Arc<dyn Logger>,
    level: LogLevel,
}

/// Filter logger calls to the configured level and above.
pub fn with_level(sink: Arc<dyn Logger>, level: LogLevel) -> LeveledLogger {
    LeveledLogger { sink, level }
}

impl LeveledLogger {
    /// `true` when a call at `at` reaches the sink. Use it to skip the work of building the data.
    pub fn enabled(&self, at: LogLevel) -> bool {
        at >= self.level
    }
}

impl Logger for LeveledLogger {
    fn debug(&self, message: &str, data: Option<&Value>) {
        if self.enabled(LogLevel::Debug) {
            self.sink.debug(message, data);
        }
    }
    fn info(&self, message: &str, data: Option<&Value>) {
        if self.enabled(LogLevel::Info) {
            self.sink.info(message, data);
        }
    }
    fn warn(&self, message: &str, data: Option<&Value>) {
        if self.enabled(LogLevel::Warn) {
            self.sink.warn(message, data);
        }
    }
    fn error(&self, message: &str, data: Option<&Value>) {
        if self.enabled(LogLevel::Error) {
            self.sink.error(message, data);
        }
    }
}

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

/// Credential headers that retain a key suffix for identification.
const KEY_HEADERS: [&str; 3] = ["authorization", "proxy-authorization", "x-api-key"];

/// Headers whose values are redacted in full.
const OPAQUE_HEADERS: [&str; 2] = ["cookie", "set-cookie"];

/// Mask a key, preserving its scheme and the last four characters of secrets longer than eight.
fn redact_key(value: &str) -> String {
    let (scheme, secret) = if value.contains(' ') {
        let mut parts = value.split_whitespace();
        (parts.next(), parts.next())
    } else {
        (None, Some(value))
    };
    let tail = match secret {
        Some(secret) if secret.chars().count() > 8 => {
            let characters: Vec<char> = secret.chars().collect();
            characters[characters.len() - 4..].iter().collect::<String>()
        }
        _ => String::new(),
    };
    match scheme {
        Some(scheme) => format!("{scheme} ***{tail}"),
        None => format!("***{tail}"),
    }
}

fn redact(name: &str, value: &str) -> String {
    let lower = name.to_lowercase();
    if KEY_HEADERS.contains(&lower.as_str()) {
        redact_key(value)
    } else if OPAQUE_HEADERS.contains(&lower.as_str()) {
        "***".to_string()
    } else {
        value.to_string()
    }
}

/// Copy headers with known credential values redacted.
pub fn redact_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers.iter().map(|(name, value)| (name.clone(), redact(name, value))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn parses_levels() {
        assert_eq!(parse_log_level("debug", "test").unwrap(), LogLevel::Debug);
        assert_eq!(parse_log_level("off", "test").unwrap(), LogLevel::Off);
        assert_eq!(
            parse_log_level("loud", "TYPESAFE_LOG_LEVEL").unwrap_err().to_string(),
            "Invalid log level \"loud\" from TYPESAFE_LOG_LEVEL. Expected one of: debug, info, warn, error, off."
        );
    }

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl Logger for Recorder {
        fn debug(&self, message: &str, _: Option<&Value>) {
            self.0.lock().unwrap().push(format!("debug {message}"));
        }
        fn info(&self, message: &str, _: Option<&Value>) {
            self.0.lock().unwrap().push(format!("info {message}"));
        }
        fn warn(&self, message: &str, _: Option<&Value>) {
            self.0.lock().unwrap().push(format!("warn {message}"));
        }
        fn error(&self, message: &str, _: Option<&Value>) {
            self.0.lock().unwrap().push(format!("error {message}"));
        }
    }

    #[test]
    fn filters_to_the_level_and_above() {
        let recorder = Arc::new(Recorder::default());
        let logger = with_level(recorder.clone(), LogLevel::Warn);
        logger.debug("a", None);
        logger.info("b", None);
        logger.warn("c", None);
        logger.error("d", None);
        assert_eq!(*recorder.0.lock().unwrap(), ["warn c", "error d"]);

        let silent = Arc::new(Recorder::default());
        with_level(silent.clone(), LogLevel::Off).error("x", None);
        assert!(silent.0.lock().unwrap().is_empty());
    }

    #[test]
    fn redacts_credentials() {
        let pair = |name: &str, value: &str| (name.to_string(), value.to_string());
        let redacted = redact_headers(&[
            pair("Authorization", "Bearer sk-1234567890abcd"),
            pair("x-api-key", "short"),
            pair("X-API-Key", "longer-secret-wxyz"),
            pair("Cookie", "session=1"),
            pair("Accept", "application/json"),
        ]);
        assert_eq!(redacted[0].1, "Bearer ***abcd");
        assert_eq!(redacted[1].1, "***");
        assert_eq!(redacted[2].1, "***wxyz");
        assert_eq!(redacted[3].1, "***");
        assert_eq!(redacted[4].1, "application/json");
    }
}
