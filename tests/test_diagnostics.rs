use smsgate::diagnostics;

#[test]
fn rust_backtrace_env_is_enabled_for_firmware_diagnostics() {
    assert_eq!(diagnostics::rust_backtrace_env(), ("RUST_BACKTRACE", "1"));
}
