use smsgate::log_ring::{FlashLogRing, LogEvent, MemFlashLogStorage, FLASH_LOG_RECORD_SIZE};
use smsgate::modem::{
    cnmi_store_notifications_enabled, creg_registered, creg_registration_status, AtResponse,
    ModemError, ModemPort, RegistrationDomain, RegistrationQuery, CSQ_UNKNOWN,
};
use smsgate::testing::mocks::ScriptedModem;
use std::time::Duration;

#[test]
fn registration_requires_creg_stat_field() {
    assert!(creg_registered("+CREG: 0,1"));
    assert!(creg_registered("+CREG: 2,5,\"abcd\""));
    assert!(!creg_registered("+CGEV: ME PDN ACT 8,1"));
    assert!(!creg_registered("+CREG: 0,0\n+CGEV: ME PDN ACT 8,1"));
    assert_eq!(creg_registration_status("+CREG: 0,0"), Some(false));
    assert_eq!(creg_registration_status("+CREG: 0,1"), Some(true));
    assert_eq!(creg_registration_status("+CREG: broken"), None);
}

#[test]
fn cnmi_health_requires_store_notification_configuration() {
    assert!(cnmi_store_notifications_enabled("+CNMI: 2,1,0,0,0"));
    assert!(cnmi_store_notifications_enabled(
        "+CGEV: ME PDN ACT 8,0\n+CNMI: 2, 1, 0, 0, 0"
    ));
    assert!(!cnmi_store_notifications_enabled("+CNMI: 2,2,0,0,0"));
    assert!(!cnmi_store_notifications_enabled("+CGEV: ME PDN ACT 8,0"));
}

#[test]
fn status_parses_expected_lines_after_unrelated_modem_events() {
    let mut modem = ScriptedModem::new()
        .expect("+CSQ", "+CGEV: ME PDN ACT 8,0\n+CSQ: 13,0", true)
        .expect(
            "+COPS?",
            "+CGEV: ME PDN DEACT 8\n+COPS: 0,2,\"310260\",7",
            true,
        )
        .expect("+CREG?", "+CREG: 0,1", true);

    let status = modem.update_status();
    assert_eq!(status.csq, 13);
    assert_eq!(status.csq_error, None);
    assert_eq!(status.operator, "310260");
    assert_eq!(status.registration.registered(), Some(true));
    assert_eq!(status.registration.stat, Some(1));
    assert_eq!(status.registration.response, "+CREG: 0,1");
    assert_eq!(status.registration.error, None);
    modem.check_consumed();
}

#[test]
fn malformed_signal_remains_unknown() {
    let mut modem = ScriptedModem::new()
        .expect("+CSQ", "+CSQ: invalid,0", true)
        .expect("+COPS?", "", true)
        .expect("+CREG?", "+CREG: 0,0", true);
    let status = modem.update_status();
    assert_eq!(status.csq, CSQ_UNKNOWN);
    assert_eq!(status.csq_error.as_deref(), Some("invalid +CSQ response"));
    assert_eq!(status.registration.registered(), Some(false));
    modem.check_consumed();
}

#[test]
fn malformed_registration_is_unknown_not_unregistered() {
    let mut modem = ScriptedModem::new()
        .expect("+CSQ", "+CSQ: 13,0", true)
        .expect("+COPS?", "", true)
        .expect("+CREG?", "garbled", true);
    let status = modem.update_status();
    assert_eq!(status.registration.registered(), None);
    assert_eq!(
        status.registration.error.as_deref(),
        Some("invalid +CREG response")
    );
    modem.check_consumed();
}

#[test]
fn sms_only_registration_keeps_home_and_roaming_stat_values() {
    for stat in [6, 7] {
        let response = format!("+CREG: 0,{stat}");
        let mut modem = ScriptedModem::new()
            .expect("+CSQ", "+CSQ: 13,0", true)
            .expect("+COPS?", "+COPS: 0,2,\"310260\",7", true)
            .expect("+CREG?", &response, true);

        let status = modem.update_status();

        assert_eq!(status.registration.stat, Some(stat));
        assert_eq!(status.registration.response, response);
        assert_eq!(status.registration.registered(), Some(true));
        assert!(status.registration.sms_only());
        assert_eq!(status.registration.error, None);
        assert!(creg_registered(&response));
        modem.check_consumed();
    }
}

#[test]
fn registration_states_distinguish_unknown_from_deregistration() {
    for (stat, registered) in [
        (0, Some(false)),
        (1, Some(true)),
        (2, Some(false)),
        (3, Some(false)),
        (4, None),
        (5, Some(true)),
        (6, Some(true)),
        (7, Some(true)),
        (8, None),
        (9, None),
        (10, None),
        (11, Some(false)),
        (255, None),
    ] {
        let response = format!("+CREG: 0,{stat}");
        let query = RegistrationQuery::from_response(
            RegistrationDomain::Circuit,
            Ok(AtResponse {
                body: response.clone(),
                ok: true,
            }),
            Duration::from_millis(42),
        );

        assert_eq!(query.stat, Some(stat));
        assert_eq!(query.registered(), registered, "stat={stat}");
        assert_eq!(creg_registration_status(&response), registered);
        assert_eq!(query.sms_only(), matches!(stat, 6 | 7));
        assert_eq!(query.error, None);
    }
}

#[test]
fn registration_timeout_is_query_failure_not_deregistration() {
    let mut modem = ScriptedModem::new()
        .expect_error("+CREG?", ModemError::Timeout)
        .expect("+CREG?", "+CREG: 0,0", true);

    let timeout = modem.query_registration(RegistrationDomain::Circuit);
    let deregistered = modem.query_registration(RegistrationDomain::Circuit);

    assert_eq!(timeout.stat, None);
    assert_eq!(timeout.registered(), None);
    assert_eq!(timeout.response, "");
    assert_eq!(
        timeout.error.as_deref(),
        Some("timeout waiting for response")
    );
    assert!(!timeout.sms_only());
    assert_eq!(deregistered.stat, Some(0));
    assert_eq!(deregistered.registered(), Some(false));
    assert_eq!(deregistered.response, "+CREG: 0,0");
    assert_eq!(deregistered.error, None);
    modem.check_consumed();
}

#[test]
fn circuit_sms_only_and_eps_registration_remain_independent() {
    let mut modem = ScriptedModem::new()
        .expect("+CREG?", "+CEREG: 0,1\n+CREG: 0,6", true)
        .expect("+CEREG?", "+CREG: 0,0\n+CEREG: 0,1", true);

    let circuit = modem.query_registration(RegistrationDomain::Circuit);
    let eps = modem.query_registration(RegistrationDomain::Eps);

    assert_eq!(circuit.stat, Some(6));
    assert_eq!(circuit.response, "+CREG: 0,6");
    assert_eq!(circuit.registered(), Some(true));
    assert!(circuit.sms_only());
    assert_eq!(eps.stat, Some(1));
    assert_eq!(eps.response, "+CEREG: 0,1");
    assert_eq!(eps.registered(), Some(true));
    assert!(!eps.sms_only());
    modem.check_consumed();
}

#[test]
fn eps_sms_only_codes_remain_unknown_without_changing_circuit_semantics() {
    for stat in [6, 7] {
        let mut modem = ScriptedModem::new()
            .expect("+CREG?", &format!("+CREG: 0,{stat}"), true)
            .expect("+CEREG?", &format!("+CEREG: 0,{stat}"), true);

        let circuit = modem.query_registration(RegistrationDomain::Circuit);
        let eps = modem.query_registration(RegistrationDomain::Eps);

        assert_eq!(circuit.stat, Some(stat));
        assert_eq!(circuit.registered(), Some(true));
        assert!(circuit.sms_only());
        assert_eq!(eps.stat, Some(stat));
        assert_eq!(eps.response, format!("+CEREG: 0,{stat}"));
        assert_eq!(eps.registered(), None);
        assert!(!eps.sms_only());
        assert_eq!(eps.error, None);
        assert!(eps.diagnostic().contains(&format!("stat={stat}")));
        modem.check_consumed();
    }
}

#[test]
fn registration_evidence_isolates_domain_line_and_keeps_elapsed_metadata() {
    let query = RegistrationQuery::from_response(
        RegistrationDomain::Eps,
        Ok(AtResponse {
            body: "AT+CEREG?\r\n+CGEV: ME PDN ACT 8,1\r\n +CEREG: 2,1,\"abcd\" \r\n+CREG: 0,0"
                .into(),
            ok: true,
        }),
        Duration::from_micros(42_999),
    );

    assert_eq!(query.stat, Some(1));
    assert_eq!(query.response, "+CEREG: 2,1,\"abcd\"");
    assert_eq!(query.elapsed_ms, 42);
    assert_eq!(
        query.diagnostic(),
        "+CEREG? stat=1 42ms: +CEREG: 2,1,\"abcd\""
    );
}

#[test]
fn registration_evidence_bounds_utf8_raw_line_without_losing_stat() {
    let raw_line = format!("+CREG: 2,1,\"{}\"", "界".repeat(100));
    let query = RegistrationQuery::from_response(
        RegistrationDomain::Circuit,
        Ok(AtResponse {
            body: format!("+CGEV: ME PDN ACT 8,1\n{raw_line}\n+CSQ: 13,0"),
            ok: true,
        }),
        Duration::from_millis(123),
    );

    assert_eq!(query.stat, Some(1));
    assert_eq!(query.registered(), Some(true));
    assert!(query.response.len() <= 128);
    assert!(query.response.len() > 125);
    assert!(raw_line.starts_with(&query.response));
    assert!(!query.response.contains(['\r', '\n']));
    assert_eq!(query.elapsed_ms, 123);
}

#[test]
fn registration_elapsed_metadata_saturates_without_wrapping() {
    for elapsed in [u64::from(u32::MAX), u64::from(u32::MAX) + 1] {
        let query = RegistrationQuery::from_response(
            RegistrationDomain::Circuit,
            Err(ModemError::Timeout),
            Duration::from_millis(elapsed),
        );

        assert_eq!(query.elapsed_ms, u32::MAX);
        assert!(query.error.is_some());
        assert_eq!(query.registered(), None);
    }
}

#[test]
fn registration_diagnostics_preserve_both_domains_after_flash_remount() {
    let elapsed = Duration::from_millis(u64::from(u32::MAX));
    let circuit = RegistrationQuery::from_response(
        RegistrationDomain::Circuit,
        Ok(AtResponse {
            body: format!("+CREG: 2,255,\\\t{}", "界".repeat(100)),
            ok: true,
        }),
        elapsed,
    );
    let eps = RegistrationQuery::from_response(
        RegistrationDomain::Eps,
        Ok(AtResponse {
            body: format!("+CEREG: 2,255,{}", "long response ".repeat(100)),
            ok: true,
        }),
        elapsed,
    );
    let failed_query = |domain| {
        RegistrationQuery::from_response(
            domain,
            Err(ModemError::AtError("long\\\tfault界 ".repeat(100))),
            elapsed,
        )
    };
    let cases = [
        (circuit.clone(), eps.clone()),
        (failed_query(RegistrationDomain::Circuit), eps),
        (circuit, failed_query(RegistrationDomain::Eps)),
        (
            failed_query(RegistrationDomain::Circuit),
            failed_query(RegistrationDomain::Eps),
        ),
    ];
    let storage = MemFlashLogStorage::new(FLASH_LOG_RECORD_SIZE * 8, FLASH_LOG_RECORD_SIZE * 2);
    let mut ring = FlashLogRing::mount(storage).unwrap();
    let mut expected_bodies = Vec::new();

    for (circuit, eps) in &cases {
        let circuit_detail = circuit.diagnostic();
        let eps_detail = eps.diagnostic();
        for detail in [&circuit_detail, &eps_detail] {
            assert!(detail.len() <= 76);
            assert!(detail.ends_with("..."));
            assert!(!detail.contains(['\\', '\t', '\r', '\n']));
        }
        // Use the longest transition label even for failed/unknown queries,
        // so both domain snippets exercise the worst-case flash row budget.
        let body = format!("network registration unavailable; {circuit_detail}; {eps_detail}");
        let entry = LogEvent::network("registration", &body, false).at("2026-10-07 12:00:00-07:00");
        assert_eq!(entry.body_preview, body);
        ring.append(&entry).unwrap();
        expected_bodies.push(body);
    }

    let mut ring = FlashLogRing::mount(ring.into_storage()).unwrap();
    let entries = ring.last_n(cases.len()).unwrap();
    assert_eq!(entries.len(), cases.len());
    for ((entry, expected), (circuit, eps)) in entries.iter().zip(expected_bodies).zip(&cases) {
        assert_eq!(entry.body_preview, expected);
        for (command, query) in [("+CREG?", circuit), ("+CEREG?", eps)] {
            let stat = query
                .stat
                .map_or_else(|| "?".into(), |stat| stat.to_string());
            assert!(entry
                .body_preview
                .contains(&format!("{command} stat={stat} {}ms:", u32::MAX)));
        }
    }
}

#[test]
fn rejected_registration_response_cannot_claim_registration() {
    let query = RegistrationQuery::from_response(
        RegistrationDomain::Circuit,
        Ok(AtResponse {
            body: "+CREG: 0,6".into(),
            ok: false,
        }),
        Duration::from_millis(50),
    );

    assert_eq!(query.response, "+CREG: 0,6");
    assert_eq!(query.stat, None);
    assert_eq!(query.registered(), None);
    assert!(!query.sms_only());
    assert_eq!(query.error.as_deref(), Some("AT+CREG? rejected"));
    assert_eq!(query.elapsed_ms, 50);
}

#[test]
fn explicit_unknown_signal_differs_from_query_failure() {
    let mut modem = ScriptedModem::new()
        .expect("+CSQ", "+CSQ: 99,99", true)
        .expect("+COPS?", "", true)
        .expect("+CREG?", "+CREG: 0,1", true);
    let status = modem.update_status();
    assert_eq!(status.csq, CSQ_UNKNOWN);
    assert_eq!(status.csq_error, None);
    modem.check_consumed();
}
