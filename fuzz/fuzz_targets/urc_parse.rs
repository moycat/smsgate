#![no_main]

use libfuzzer_sys::fuzz_target;
use smsgate::modem::{
    creg_registration_status, urc, AtResponse, RegistrationDomain, RegistrationQuery,
};
use smsgate::sms::codec::{parse_clip_line, parse_sms_pdu};
use std::time::Duration;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = parse_clip_line(s);
        let _ = urc::is_urc(s);
        let _ = urc::parse_urc(s);
        let _ = creg_registration_status(s);
        for domain in [RegistrationDomain::Circuit, RegistrationDomain::Eps] {
            let query = RegistrationQuery::from_response(
                domain,
                Ok(AtResponse {
                    body: s.to_owned(),
                    ok: true,
                }),
                Duration::ZERO,
            );
            let _ = query.registered();
            let _ = query.diagnostic();
        }
    }
    // Also fuzz PDU decode with raw bytes converted to hex
    let hex: String = data.iter().map(|b| format!("{:02X}", b)).collect();
    let _ = parse_sms_pdu(&hex);
});
