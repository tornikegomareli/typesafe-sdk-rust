//! Runtime description for the `X-TypeSafe-Runtime` header.

/// Runtime name and platform, as in `rust (macos; aarch64)`.
pub fn describe_runtime() -> String {
    format!("rust ({}; {})", std::env::consts::OS, std::env::consts::ARCH)
}
