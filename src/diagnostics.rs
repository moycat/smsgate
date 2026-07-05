//! Runtime diagnostic configuration.

pub const RUST_BACKTRACE_ENV_KEY: &str = "RUST_BACKTRACE";
pub const RUST_BACKTRACE_ENV_VALUE: &str = "1";

pub fn rust_backtrace_env() -> (&'static str, &'static str) {
    (RUST_BACKTRACE_ENV_KEY, RUST_BACKTRACE_ENV_VALUE)
}
