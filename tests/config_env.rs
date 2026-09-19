//! Port of the configuration tests of `test/client.test.ts` that read environment variables.
//!
//! Environment variables are global to the process. These tests are in their own test binary, and
//! each test holds `ENV_LOCK`, so that they do not run in parallel with each other.

mod common;

use std::sync::{Mutex, MutexGuard};

use common::*;
use serde_json::Value;
use typesafe::{
    env, Error, LogLevel, RetryPolicy, TypeSafeClient, TypeSafeClientConfig, DEFAULT_BASE_URL, DEFAULT_MODEL, DEFAULT_TIMEOUT_MS,
    LOG_LEVELS,
};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer};

static ENV_LOCK: Mutex<()> = Mutex::new(());

const NAMES: [&str; 4] = [env::API_KEY, env::BASE_URL, env::DEFAULT_MODEL, env::LOG_LEVEL];

/// Holds the lock and restores the environment variables when it is dropped.
struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
    _lock: MutexGuard<'static, ()>,
}

/// Take the lock and remove all TypeSafe environment variables.
fn clean_env() -> EnvGuard {
    let lock = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let saved = NAMES.iter().map(|name| (*name, std::env::var(name).ok())).collect();
    for name in NAMES {
        std::env::remove_var(name);
    }
    EnvGuard { saved, _lock: lock }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in &self.saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

fn with_key() -> TypeSafeClientConfig {
    TypeSafeClientConfig {
        api_key: Some("k".into()),
        ..Default::default()
    }
}

#[test]
fn uses_the_defaults_when_config_and_environment_are_empty() {
    let _env = clean_env();
    let client = TypeSafeClient::new(with_key()).unwrap();
    assert_eq!(client.base_url(), DEFAULT_BASE_URL);
    assert_eq!(client.default_model(), DEFAULT_MODEL);
    assert_eq!(client.default_model(), "jev-latest");
    assert_eq!(client.log_level(), LogLevel::Warn);
    assert_eq!(*client.retry(), RetryPolicy::default());
    assert_eq!(client.timeout_ms(), DEFAULT_TIMEOUT_MS);
    assert_eq!(client.timeout_ms(), 10_000);
}

#[test]
fn missing_api_key_error_names_the_environment_variable() {
    let _env = clean_env();
    let error = TypeSafeClient::new(Default::default()).unwrap_err();
    assert!(matches!(error, Error::TypeSafe(_)));
    assert_eq!(
        error.to_string(),
        "No API key was provided. Pass `api_key` to the TypeSafeClient constructor or set the TYPESAFE_API_KEY environment variable."
    );
}

#[tokio::test]
async fn reads_every_setting_from_the_environment() {
    let _env = clean_env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(json_response(200, system_one_response()))
        .mount(&server)
        .await;
    std::env::set_var(env::API_KEY, "env-key");
    std::env::set_var(env::BASE_URL, server.uri());
    std::env::set_var(env::DEFAULT_MODEL, "env-model");
    std::env::set_var(env::LOG_LEVEL, "error");

    let client = TypeSafeClient::new(Default::default()).unwrap();
    assert_eq!(client.base_url(), server.uri());
    assert_eq!(client.default_model(), "env-model");
    assert_eq!(client.log_level(), LogLevel::Error);

    client.system_one(request(), Default::default()).await.unwrap();
    let requests = received(&server).await;
    assert_eq!(header(&requests[0], "authorization"), Some("Bearer env-key"));
    assert_eq!(requests[0].body_json::<Value>().unwrap()["model"], "env-model");
}

#[tokio::test]
async fn explicit_config_wins_over_the_environment() {
    let _env = clean_env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(json_response(200, system_one_response()))
        .mount(&server)
        .await;
    std::env::set_var(env::API_KEY, "env-key");
    std::env::set_var(env::BASE_URL, "https://env.test");
    std::env::set_var(env::DEFAULT_MODEL, "env-model");
    std::env::set_var(env::LOG_LEVEL, "debug");

    let client = TypeSafeClient::new(TypeSafeClientConfig {
        api_key: Some("code-key".into()),
        base_url: Some(server.uri()),
        default_model: Some("code-model".into()),
        log_level: Some(LogLevel::Error),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(client.base_url(), server.uri());
    assert_eq!(client.default_model(), "code-model");
    assert_eq!(client.log_level(), LogLevel::Error);

    client.system_one(request(), Default::default()).await.unwrap();
    let requests = received(&server).await;
    assert_eq!(header(&requests[0], "authorization"), Some("Bearer code-key"));
    assert_eq!(requests[0].body_json::<Value>().unwrap()["model"], "code-model");
}

#[test]
fn blank_environment_values_are_ignored() {
    let _env = clean_env();
    std::env::set_var(env::API_KEY, "k");
    std::env::set_var(env::BASE_URL, "   ");
    std::env::set_var(env::DEFAULT_MODEL, "");
    std::env::set_var(env::LOG_LEVEL, "");
    let client = TypeSafeClient::new(Default::default()).unwrap();
    assert_eq!(client.base_url(), DEFAULT_BASE_URL);
    assert_eq!(client.default_model(), DEFAULT_MODEL);
    assert_eq!(client.log_level(), LogLevel::Warn);

    std::env::set_var(env::API_KEY, "  ");
    assert!(matches!(TypeSafeClient::new(Default::default()), Err(Error::TypeSafe(_))));
}

#[test]
fn removes_trailing_slashes_from_the_base_url_of_either_source() {
    let _env = clean_env();
    std::env::set_var(env::BASE_URL, "https://example.test///");
    assert_eq!(TypeSafeClient::new(with_key()).unwrap().base_url(), "https://example.test");

    let explicit = TypeSafeClientConfig {
        base_url: Some("https://x.test/".into()),
        ..with_key()
    };
    assert_eq!(TypeSafeClient::new(explicit).unwrap().base_url(), "https://x.test");
}

#[test]
fn accepts_each_log_level_from_config_and_environment() {
    let _env = clean_env();
    for level in LOG_LEVELS {
        let explicit = TypeSafeClientConfig {
            log_level: Some(level),
            ..with_key()
        };
        assert_eq!(TypeSafeClient::new(explicit).unwrap().log_level(), level);

        std::env::set_var(env::LOG_LEVEL, level.as_str());
        assert_eq!(TypeSafeClient::new(with_key()).unwrap().log_level(), level);
    }
}

#[test]
fn rejects_an_invalid_log_level_from_the_environment() {
    let _env = clean_env();
    std::env::set_var(env::LOG_LEVEL, "loud");
    let error = TypeSafeClient::new(with_key()).unwrap_err();
    assert!(matches!(error, Error::TypeSafe(_)));
    assert_eq!(
        error.to_string(),
        "Invalid log level \"loud\" from TYPESAFE_LOG_LEVEL. Expected one of: debug, info, warn, error, off."
    );

    // An explicit level wins, so the invalid environment value is not read.
    let explicit = TypeSafeClientConfig {
        log_level: Some(LogLevel::Info),
        ..with_key()
    };
    assert_eq!(TypeSafeClient::new(explicit).unwrap().log_level(), LogLevel::Info);
}
