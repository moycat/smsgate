//! URC classifier and parser tests.

use smsgate::modem::urc::{is_urc, parse_urc, Urc};

// ---------------------------------------------------------------------------
// is_urc
// ---------------------------------------------------------------------------

#[test]
fn cmti_is_urc() {
    assert!(is_urc("+CMTI: \"SM\",3"));
}

#[test]
fn cmt_is_urc() {
    assert!(is_urc("+CMT: \"+8613800138000\",18"));
}

#[test]
fn clip_is_urc() {
    assert!(is_urc("+CLIP: \"+8613800138000\",145"));
}

#[test]
fn ring_is_urc() {
    assert!(is_urc("RING"));
}

#[test]
fn no_carrier_is_urc() {
    assert!(is_urc("NO CARRIER"));
}

// +CREG / +CGREG / +CEREG are intentionally NOT URCs (see is_urc doc).
// They appear only as AT+CREG? responses with AT+CREG=0 (default).
#[test]
fn creg_is_not_urc() {
    assert!(!is_urc("+CREG: 1"));
    assert!(!is_urc("+CGREG: 1"));
    assert!(!is_urc("+CEREG: 1"));
}

#[test]
fn cds_is_urc() {
    assert!(is_urc("+CDS: ..."));
    assert!(is_urc("+CDSI: 1"));
}

#[test]
fn cusd_is_urc() {
    assert!(is_urc("+CUSD: 0,\"balance\",15"));
}

#[test]
fn ok_response_is_not_urc() {
    assert!(!is_urc("OK"));
}

#[test]
fn at_response_body_is_not_urc() {
    assert!(!is_urc("+CSQ: 20,0"));
    assert!(!is_urc("+COPS: 0,0,\"China Mobile\""));
}

// ---------------------------------------------------------------------------
// parse_urc
// ---------------------------------------------------------------------------

#[test]
fn parse_cmti_extracts_index() {
    let urc = parse_urc("+CMTI: \"SM\",3");
    assert!(matches!(urc, Urc::NewSms { index: 3, .. }));
}

#[test]
fn parse_cmti_extracts_mem_sm() {
    if let Urc::NewSms { mem, index } = parse_urc("+CMTI: \"SM\",3") {
        assert_eq!(mem, "SM");
        assert_eq!(index, 3);
    } else {
        panic!("expected Urc::NewSms");
    }
}

#[test]
fn parse_cmti_me_memory() {
    if let Urc::NewSms { mem, index } = parse_urc("+CMTI: \"ME\",7") {
        assert_eq!(mem, "ME");
        assert_eq!(index, 7);
    } else {
        panic!("expected Urc::NewSms");
    }
}

#[test]
fn parse_cmt_is_delivery() {
    let urc = parse_urc("+CMT: \"+8613800138000\",18");
    assert!(matches!(urc, Urc::SmsDelivery));
}

#[test]
fn parse_ring() {
    let urc = parse_urc("RING");
    assert!(matches!(urc, Urc::Ring));
}

#[test]
fn parse_clip_extracts_number() {
    let urc = parse_urc("+CLIP: \"+8613800138000\",145,\"\",,,\"\"");
    if let Urc::Clip(n) = urc {
        assert_eq!(n, "+8613800138000");
    } else {
        panic!("expected Urc::Clip");
    }
}

#[test]
fn parse_cds_is_status_report() {
    let urc = parse_urc("+CDS: ...");
    assert!(matches!(urc, Urc::StatusReport));
}

#[test]
fn parse_cdsi_is_status_report() {
    let urc = parse_urc("+CDSI: 1");
    assert!(matches!(urc, Urc::StatusReport));
}

#[test]
fn parse_unknown_is_other() {
    let urc = parse_urc("+UNKNOWNCMD: data");
    assert!(matches!(urc, Urc::Other(_)));
}

#[test]
fn parse_cmti_bad_index_defaults_zero() {
    let urc = parse_urc("+CMTI: \"SM\",abc");
    assert!(matches!(urc, Urc::NewSms { index: 0, .. }));
}

// Some A76xx firmware variants omit the space after the colon.
#[test]
fn parse_cmti_no_space_after_colon() {
    if let Urc::NewSms { mem, index } = parse_urc("+CMTI:\"ME\",1") {
        assert_eq!(mem, "ME");
        assert_eq!(index, 1);
    } else {
        panic!("expected Urc::NewSms for +CMTI without space");
    }
}

#[test]
fn parse_clip_no_space_after_colon() {
    let urc = parse_urc("+CLIP:\"+8613800138000\",145,\"\",,,\"\"");
    assert!(matches!(urc, Urc::Clip(_)));
}
