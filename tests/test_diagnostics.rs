use smsgate::diagnostics;

#[test]
fn rust_backtrace_env_is_enabled_for_firmware_diagnostics() {
    assert_eq!(diagnostics::rust_backtrace_env(), ("RUST_BACKTRACE", "1"));
}

#[test]
fn firmware_diagnostic_env_keeps_panic_backtraces_but_disables_error_backtraces() {
    assert_eq!(
        diagnostics::firmware_diagnostic_env(),
        [("RUST_BACKTRACE", "1"), ("RUST_LIB_BACKTRACE", "0")]
    );
}
