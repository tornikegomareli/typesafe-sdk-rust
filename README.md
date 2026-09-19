# TypeSafe AI Rust SDK

Community Rust SDK for [TypeSafe AI](https://typesafe.ai). It is a port of the official JavaScript SDK,
[`@typesafe-ai/sdk`](https://github.com/typesafe-ai/typesafe-sdk-js) 0.6.0, with the same defaults, the
same retry rules, and the same error messages.

## Quickstart

```toml
[dependencies]
typesafe-sdk-rust = { git = "https://github.com/tornikegomareli/typesafe-sdk-rust" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Set `TYPESAFE_API_KEY` in your environment, then create and use the client. The library name is `typesafe`.

```rust
use typesafe::{choice, noul, Questions, SystemOneRequest, TypeSafeClient};

#[tokio::main]
async fn main() -> typesafe::Result<()> {
    let client = TypeSafeClient::new(Default::default())?;

    let mut questions = Questions::new();
    questions.insert("category".into(), choice("What is this ticket about?", [("billing", ()), ("technical", ()), ("other", ())]));
    questions.insert("urgent".into(), noul("Does the customer convey urgency?"));

    let result = client
        .system_one(SystemOneRequest::new("I was charged twice. Please fix this ASAP.", questions), Default::default())
        .await?;

    println!("{}", result.choice("category").unwrap().choice);
    println!("{}", result.noul("urgent").unwrap().noul);
    Ok(())
}
```

All questions in one request run in parallel and cannot see one another's answers. The model sees the
options of a choice in the order you give them, and the order can change the probabilities. The SDK keeps
that order.

`cargo run --example ticket` runs all three question types against the API.

## What maps to what

| JavaScript | Rust |
| --- | --- |
| `new TypeSafeClient(config)` | `TypeSafeClient::new(TypeSafeClientConfig { .. })`. A clone shares the connection pool. |
| `noul()`, `choice()`, `score()` | `noul()`, `noul_with_criteria()`, `choice()`, `score()` |
| `await client.systemOne(request, options)` | `client.system_one(request, options).await?` |
| `.withResponse()`, `.asResponse()` | `.with_response().await?`, `.as_response().await?` |
| `client.models.list()` | `client.models().list(options).await?` |
| answers typed by the question | `result.noul(name)`, `result.choice(name)`, `result.score(name)` |
| `AbortSignal` | `CancellationToken` in `RequestOptions::signal` |
| the error classes | the `Error` enum: `TypeSafe`, `Api`, `Connection`, `Timeout`, `UserAbort`. `ApiError::kind()` gives the class of the status. |
| `retry: Partial<RetryPolicy>` | `RetryOverrides` |
| `logger`, `logLevel` | the `Logger` trait, `LogLevel` |
| `fetch` | `TypeSafeClientConfig::http_client`, a `reqwest::Client` |

Two things differ. A request starts when you await it, not when you create it, because a Rust future
does no work before that. The browser guard has no meaning in Rust, so `dangerouslyAllowBrowser` is gone.

## Configuration

Explicit options take precedence over environment variables, then SDK defaults.

| Option | Environment variable | Default |
| --- | --- | --- |
| `api_key` | `TYPESAFE_API_KEY` | required |
| `base_url` | `TYPESAFE_BASE_URL` | `https://api.typesafe.ai` |
| `default_model` | `TYPESAFE_DEFAULT_MODEL` | `jev-latest` |
| `log_level` | `TYPESAFE_LOG_LEVEL` | `warn` |
| `timeout_ms` | | 10000, for each attempt |
| `retry` | | 2 retries for 408, 429, 5xx, connection errors, and timeouts. Backoff from 500 ms to 5 s with jitter. `Retry-After` is honored up to 60 s. |

`info` logs request summaries. `debug` adds headers and bodies. Known credential headers are redacted.
Bodies are not.

## Documentation

Learn what TypeSafe can do in the [TypeSafe docs](https://docs.typesafe.ai/).

## License

MIT. See [LICENSE](LICENSE). This SDK is not an official TypeSafe product.
