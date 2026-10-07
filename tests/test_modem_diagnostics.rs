//! Bounded modem receive-fault diagnostics.

use smsgate::modem::ModemDiagnostics;

#[test]
fn diagnostic_counters_saturate_without_losing_receive_faults() {
    let mut diagnostics = ModemDiagnostics {
        dropped_urcs: u32::MAX - 1,
        dropped_response_lines: u32::MAX - 1,
        overlong_lines: u32::MAX - 1,
    };

    diagnostics.accumulate(ModemDiagnostics {
        dropped_urcs: 2,
        dropped_response_lines: 2,
        overlong_lines: 2,
    });

    assert_eq!(diagnostics.dropped_urcs, u32::MAX);
    assert_eq!(diagnostics.dropped_response_lines, u32::MAX);
    assert_eq!(diagnostics.overlong_lines, u32::MAX);
    assert!(!diagnostics.is_empty());
}
