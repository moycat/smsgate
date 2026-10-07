//! Bounded PDN diagnostics retain evidence without implying registration state.

use smsgate::modem::{ModemDiagnostics, PdnEvent};

#[test]
fn pdn_event_parses_context_initiator_and_optional_reason() {
    for (line, expected, display) in [
        (
            "+CGEV: ME PDN ACT 8,1",
            PdnEvent {
                cid: 8,
                reason: Some(1),
                network_initiated: false,
            },
            "ME cid=8 reason=1",
        ),
        (
            "+CGEV: NW PDN DEACT 1",
            PdnEvent {
                cid: 1,
                reason: None,
                network_initiated: true,
            },
            "NW cid=1",
        ),
        (
            " +CGEV: ME PDN DEACT 8, 0 ",
            PdnEvent {
                cid: 8,
                reason: None,
                network_initiated: false,
            },
            "ME cid=8",
        ),
        (
            "+CGEV: NW PDN ACT 2,3",
            PdnEvent {
                cid: 2,
                reason: None,
                network_initiated: true,
            },
            "NW cid=2",
        ),
    ] {
        let event = PdnEvent::parse(line).unwrap();

        assert_eq!(event, expected);
        assert_eq!(event.to_string(), display);
    }
}

#[test]
fn only_me_pdn_activation_interprets_the_optional_number_as_a_reason() {
    for reason in [0, 1, 2] {
        let event = PdnEvent::parse(&format!("+CGEV: ME PDN ACT 8,{reason}")).unwrap();

        assert_eq!(event.cid, 8);
        assert_eq!(event.reason, Some(reason));
        assert!(!event.network_initiated);
        assert_eq!(event.to_string(), format!("ME cid=8 reason={reason}"));
    }
}

#[test]
fn other_pdn_events_never_mislabel_optional_numbers_as_reasons() {
    for (kind, network_initiated, display) in [
        ("NW PDN ACT", true, "NW cid=8"),
        ("NW PDN DEACT", true, "NW cid=8"),
        ("ME PDN DEACT", false, "ME cid=8"),
    ] {
        for optional in [0, 1, 3] {
            let event = PdnEvent::parse(&format!("+CGEV: {kind} 8,{optional}")).unwrap();

            assert_eq!(event.cid, 8);
            assert_eq!(event.reason, None, "{kind} optional field={optional}");
            assert_eq!(event.network_initiated, network_initiated);
            assert_eq!(event.to_string(), display);
        }
    }
}

#[test]
fn malformed_pdn_event_does_not_fabricate_a_context_id() {
    for line in [
        "",
        "+CREG: 0,1",
        "+CGEV: ME PDN ACT",
        "+CGEV: ME PDN ACT ,1",
        "+CGEV: ME PDN ACT invalid,1",
        "+CGEV: ME PDN ACT 256,1",
        "+CGEV: ME PDN DEACT -1",
        "+CGEV: PDN ACT 8,1",
        "+CGEV: XX PDN ACT 8,1",
        "+CGEV: ME DETACH",
    ] {
        assert_eq!(PdnEvent::parse(line), None, "unexpected event from {line}");
    }
}

#[test]
fn invalid_optional_reason_keeps_real_context_without_fabricating_reason() {
    for reason in ["", "invalid", "256"] {
        let event = PdnEvent::parse(&format!("+CGEV: ME PDN ACT 8,{reason}")).unwrap();

        assert_eq!(event.cid, 8);
        assert_eq!(event.reason, None);
        assert_eq!(event.to_string(), "ME cid=8");
    }
}

#[test]
fn diagnostic_accumulation_retains_latest_activation_and_deactivation_independently() {
    let activation = PdnEvent::parse("+CGEV: ME PDN ACT 1,0").unwrap();
    let deactivation = PdnEvent::parse("+CGEV: NW PDN DEACT 8").unwrap();
    let latest_activation = PdnEvent::parse("+CGEV: ME PDN ACT 8,1").unwrap();
    let mut diagnostics = ModemDiagnostics::default();

    diagnostics.accumulate(ModemDiagnostics {
        pdn_activations: 1,
        last_pdn_activation: Some(activation),
        ..Default::default()
    });
    diagnostics.accumulate(ModemDiagnostics {
        pdn_deactivations: 2,
        last_pdn_deactivation: Some(deactivation),
        ..Default::default()
    });
    diagnostics.accumulate(ModemDiagnostics {
        pdn_activations: 3,
        last_pdn_activation: Some(latest_activation),
        ..Default::default()
    });
    diagnostics.accumulate(ModemDiagnostics::default());

    assert_eq!(diagnostics.pdn_activations, 4);
    assert_eq!(diagnostics.pdn_deactivations, 2);
    assert_eq!(diagnostics.last_pdn_activation, Some(latest_activation));
    assert_eq!(diagnostics.last_pdn_deactivation, Some(deactivation));
    assert!(!diagnostics.pdn_events_empty());
    assert!(diagnostics.receive_faults_empty());
    assert!(!diagnostics.is_empty());
}

#[test]
fn malformed_latest_event_clears_stale_evidence_for_only_its_event_kind() {
    let previous_activation = PdnEvent::parse("+CGEV: ME PDN ACT 8,1").unwrap();
    let previous_deactivation = PdnEvent::parse("+CGEV: NW PDN DEACT 1").unwrap();
    let mut diagnostics = ModemDiagnostics {
        pdn_activations: 1,
        pdn_deactivations: 1,
        last_pdn_activation: Some(previous_activation),
        last_pdn_deactivation: Some(previous_deactivation),
        ..Default::default()
    };

    diagnostics.accumulate(ModemDiagnostics {
        pdn_activations: 1,
        last_pdn_activation: PdnEvent::parse("+CGEV: ME PDN ACT invalid,1"),
        ..Default::default()
    });

    assert_eq!(diagnostics.pdn_activations, 2);
    assert_eq!(diagnostics.last_pdn_activation, None);
    assert_eq!(
        diagnostics.last_pdn_deactivation,
        Some(previous_deactivation)
    );
    assert_eq!(diagnostics.pdn_deactivations, 1);
}

#[test]
fn diagnostic_counters_saturate_and_latest_event_evidence_still_updates() {
    let previous_activation = PdnEvent::parse("+CGEV: ME PDN ACT 1,0").unwrap();
    let previous_deactivation = PdnEvent::parse("+CGEV: ME PDN DEACT 1").unwrap();
    let latest_activation = PdnEvent::parse("+CGEV: ME PDN ACT 8,1").unwrap();
    let latest_deactivation = PdnEvent::parse("+CGEV: NW PDN DEACT 8").unwrap();
    let mut diagnostics = ModemDiagnostics {
        dropped_urcs: u32::MAX - 1,
        dropped_response_lines: u32::MAX - 1,
        overlong_lines: u32::MAX - 1,
        pdn_activations: u32::MAX - 1,
        pdn_deactivations: u32::MAX - 1,
        last_pdn_activation: Some(previous_activation),
        last_pdn_deactivation: Some(previous_deactivation),
    };

    diagnostics.accumulate(ModemDiagnostics {
        dropped_urcs: 2,
        dropped_response_lines: 2,
        overlong_lines: 2,
        pdn_activations: 2,
        pdn_deactivations: 2,
        last_pdn_activation: Some(latest_activation),
        last_pdn_deactivation: Some(latest_deactivation),
    });

    assert_eq!(diagnostics.dropped_urcs, u32::MAX);
    assert_eq!(diagnostics.dropped_response_lines, u32::MAX);
    assert_eq!(diagnostics.overlong_lines, u32::MAX);
    assert_eq!(diagnostics.pdn_activations, u32::MAX);
    assert_eq!(diagnostics.pdn_deactivations, u32::MAX);
    assert_eq!(diagnostics.last_pdn_activation, Some(latest_activation));
    assert_eq!(diagnostics.last_pdn_deactivation, Some(latest_deactivation));
    assert!(!diagnostics.receive_faults_empty());
}
