//! Runtime diagnostic configuration.

pub const RUST_BACKTRACE_ENV_KEY: &str = "RUST_BACKTRACE";
pub const RUST_BACKTRACE_ENV_VALUE: &str = "1";
pub const RUST_LIB_BACKTRACE_ENV_KEY: &str = "RUST_LIB_BACKTRACE";
pub const RUST_LIB_BACKTRACE_ENV_VALUE: &str = "0";

pub fn rust_backtrace_env() -> (&'static str, &'static str) {
    (RUST_BACKTRACE_ENV_KEY, RUST_BACKTRACE_ENV_VALUE)
}

pub fn firmware_diagnostic_env() -> [(&'static str, &'static str); 2] {
    [
        rust_backtrace_env(),
        (RUST_LIB_BACKTRACE_ENV_KEY, RUST_LIB_BACKTRACE_ENV_VALUE),
    ]
}
