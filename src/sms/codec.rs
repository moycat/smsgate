//! SMS PDU encode/decode — pure functions, no hardware dependencies.
//!
//! Supports:
//!   - SMS-DELIVER parsing (AT+CMGR / AT+CMGL PDU mode)
//!   - SMS-SUBMIT encoding (GSM-7 and UCS-2, single-part and multi-part)
//!   - SMS-STATUS-REPORT parsing (+CDS URC)
//!   - CLIP URC parsing
//!   - Phone number formatting / normalisation

use super::SmsError;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Parsed SMS-DELIVER PDU.
#[derive(Debug, Clone, Default)]
pub struct SmsPdu {
    pub sender: String,
    /// "yy/MM/dd,HH:mm:ss+zz" — same shape accepted by timestamp helpers.
    pub timestamp: String,
    pub content: String,
    pub is_concatenated: bool,
    pub concat_ref: u16,
    pub concat_total: u8,
    pub concat_part: u8,
}

/// A single SMS-SUBMIT PDU (ready to hand to AT+CMGS).
#[derive(Debug, Clone)]
pub struct SmsSubmitPdu {
    /// Full PDU as uppercase hex (SCA + TPDU).
    pub hex: String,
    /// TPDU byte count passed as the argument to AT+CMGS=<n>.
    pub tpdu_len: u8,
}

/// Parsed SMS-STATUS-REPORT PDU (+CDS URC).
#[derive(Debug, Clone)]
pub struct StatusReport {
    pub message_ref: u8,
    pub recipient: String,
    pub sc_timestamp: String,
    pub discharge_time: String,
    pub status: u8,
    pub delivered: bool,
    pub status_text: String,
}

// ---------------------------------------------------------------------------
// Phone number helpers
// ---------------------------------------------------------------------------

/// Format a phone number for display in forwarded messages.
/// `xxxxxxxxxxx` (11 digits) → `+86 xxx-xxxx-xxxx`
pub fn human_readable_phone(number: &str) -> String {
    if number.len() == 11 && number.chars().all(|c| c.is_ascii_digit()) {
        return format!("+86 {}-{}-{}", &number[..3], &number[3..7], &number[7..]);
    }
    if number.len() == 14 && number.starts_with("+86") {
        return format!("+86 {}-{}-{}", &number[3..6], &number[6..10], &number[10..]);
    }
    number.to_string()
}

/// Strip formatting characters; convert leading "00" to "+".
pub fn normalize_phone(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c == '+' || c.is_ascii_digit() {
            out.push(c);
        }
    }
    if out.starts_with("00") {
        out = format!("+{}", &out[2..]);
    }
    out
}

/// Convert a CMGR/PDU timestamp to RFC 3339.
/// `gmt_offset_minutes` is e.g. 480 for UTC+8, -300 for UTC-5.
pub fn timestamp_to_rfc3339(ts: &str, gmt_offset_minutes: i32) -> String {
    if ts.len() < 17 {
        return String::new();
    }
    let abs_mins = gmt_offset_minutes.unsigned_abs();
    let abs_h = abs_mins / 60;
    let abs_m = abs_mins % 60;
    let sign = if gmt_offset_minutes < 0 { '-' } else { '+' };
    format!(
        "20{year}-{mo}-{dd}T{hh}:{mm}:{ss}{sign}{ah:02}:{am:02}",
        year = &ts[0..2],
        mo = &ts[3..5],
        dd = &ts[6..8],
        hh = &ts[9..11],
        mm = &ts[12..14],
        ss = &ts[15..17],
        sign = sign,
        ah = abs_h,
        am = abs_m,
    )
}

/// Convert a PDU timestamp to a UTC Unix timestamp (seconds since epoch).
/// Returns 0 on parse error.
pub fn pdu_timestamp_to_unix(ts: &str) -> i64 {
    if ts.len() < 17 {
        return 0;
    }
    let parse2 = |s: &str| -> i64 { s.parse().unwrap_or(0) };
    let yy = parse2(&ts[0..2]);
    let mo = parse2(&ts[3..5]);
    let dd = parse2(&ts[6..8]);
    let hh = parse2(&ts[9..11]);
    let mm = parse2(&ts[12..14]);
    let ss = parse2(&ts[15..17]);

    if !(1..=12).contains(&mo) || !(1..=31).contains(&dd) || hh > 23 || mm > 59 || ss > 60 {
        return 0;
    }

    let year = 2000 + yy;
    let month_days = [0i64, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let is_leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let leaps =
        (year - 1) / 4 - (year - 1) / 100 + (year - 1) / 400 - (1969 / 4 - 1969 / 100 + 1969 / 400);
    let doy = month_days[(mo - 1) as usize] + dd - 1 + if mo > 2 && is_leap { 1 } else { 0 };
    let days = (year - 1970) * 365 + leaps + doy;
    let local = days * 86400 + hh * 3600 + mm * 60 + ss;

    let tz_offset_min: i64 = if ts.len() >= 18 {
        let sign = if ts.as_bytes()[17] == b'-' {
            -1i64
        } else {
            1i64
        };
        let raw: i64 = ts[18..].parse().unwrap_or(0);
        sign * raw * 15
    } else {
        0
    };

    local - tz_offset_min * 60
}

/// Parse a +CLIP URC line. Returns the caller number (may be empty for withheld).
pub fn parse_clip_line(line: &str) -> Option<String> {
    if !line.starts_with("+CLIP:") {
        return None;
    }
    let q1 = line.find('"')?;
    let rest = &line[q1 + 1..];
    let q2 = rest.find('"')?;
    let number = &rest[..q2];
    // Require comma after closing quote
    rest[q2 + 1..].starts_with(',').then(|| number.to_string())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    // Strip whitespace
    let clean: Vec<u8> = hex
        .bytes()
        .filter(|&b| !matches!(b, b' ' | b'\r' | b'\n' | b'\t'))
        .collect();
    if !clean.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(clean.len() / 2);
    let mut i = 0;
    while i + 1 < clean.len() {
        let hi = hex_nibble(clean[i])?;
        let lo = hex_nibble(clean[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Some(out)
}

fn bytes_to_hex(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 2);
    for &b in data {
        out.push(
            char::from_digit((b >> 4) as u32, 16)
                .unwrap()
                .to_ascii_uppercase(),
        );
        out.push(
            char::from_digit((b & 0xF) as u32, 16)
                .unwrap()
                .to_ascii_uppercase(),
        );
    }
    out
}

// GSM-7 default alphabet (septet index → UTF-8)
static GSM7: [&str; 128] = [
    "@", "£", "$", "¥", "è", "é", "ù", "ì", "ò", "Ç", "\n", "Ø", "ø", "\r", "Å", "å", "Δ", "_",
    "Φ", "Γ", "Λ", "Ω", "Π", "Ψ", "Σ", "Θ", "Ξ", "\x1b", "Æ", "æ", "ß", "É", " ", "!", "\"", "#",
    "¤", "%", "&", "'", "(", ")", "*", "+", ",", "-", ".", "/", "0", "1", "2", "3", "4", "5", "6",
    "7", "8", "9", ":", ";", "<", "=", ">", "?", "¡", "A", "B", "C", "D", "E", "F", "G", "H", "I",
    "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z", "Ä", "Ö",
    "Ñ", "Ü", "§", "¿", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o",
    "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z", "ä", "ö", "ñ", "ü", "à",
];

fn gsm7_extension(c: u8) -> &'static str {
    match c {
        0x0A => "\x0C",
        0x14 => "^",
        0x28 => "{",
        0x29 => "}",
        0x2F => "\\",
        0x3C => "[",
        0x3D => "~",
        0x3E => "]",
        0x40 => "|",
        0x65 => "€",
        _ => " ",
    }
}

fn unpack_septets(data: &[u8], num_septets: usize, bit_offset: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(num_septets);
    for i in 0..num_septets {
        let bit = bit_offset + i * 7;
        let byte = bit / 8;
        let shift = bit % 8;
        if byte >= data.len() {
            break;
        }
        let v = data[byte] as u16 | ((data.get(byte + 1).copied().unwrap_or(0) as u16) * 256);
        out.push(((v >> shift) & 0x7F) as u8);
    }
    out
}

fn gsm7_septets_to_utf8(septets: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < septets.len() {
        let s = septets[i];
        if s == 0x1B {
            i += 1;
            if i < septets.len() {
                out.push_str(gsm7_extension(septets[i]));
            }
        } else if (s as usize) < 128 {
            out.push_str(GSM7[s as usize]);
        }
        i += 1;
    }
    out
}

fn ucs2_bytes_to_utf8(data: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i + 1 < data.len() {
        let code = (data[i] as u16) << 8 | data[i + 1] as u16;
        i += 2;
        // High surrogate?
        if (0xD800..=0xDBFF).contains(&code) && i + 1 < data.len() {
            let low = (data[i] as u16) << 8 | data[i + 1] as u16;
            if (0xDC00..=0xDFFF).contains(&low) {
                i += 2;
                let cp = ((code as u32 - 0xD800) << 10) + (low as u32 - 0xDC00) + 0x10000;
                if let Some(c) = char::from_u32(cp) {
                    out.push(c);
                }
                continue;
            }
        }
        if let Some(c) = char::from_u32(code as u32) {
            out.push(c);
        }
    }
    out
}

fn decode_bcd_address(data: &[u8], byte_len: usize, digit_len: usize) -> String {
    let mut out = String::with_capacity(digit_len);
    let mut i = 0;
    while i < byte_len && out.len() < digit_len {
        let lo = data[i] & 0x0F;
        let hi = (data[i] >> 4) & 0x0F;
        if lo <= 9 {
            out.push((b'0' + lo) as char);
        } else {
            break;
        }
        if out.len() >= digit_len {
            break;
        }
        if hi <= 9 {
            out.push((b'0' + hi) as char);
        } else {
            break;
        }
        i += 1;
    }
    out
}

fn decode_scts(data: &[u8]) -> String {
    if data.len() < 7 {
        return String::new();
    }
    let swap = |b: u8| -> u8 { (b & 0x0F) * 10 + ((b >> 4) & 0x0F) };
    let tz_raw = data[6];
    let tz_hi = (tz_raw >> 4) & 0x0F;
    let negative = (tz_hi & 0x08) != 0;
    let tz_hi2 = tz_hi & 0x07;
    let tz_lo = tz_raw & 0x0F;
    let tz_q = tz_lo * 10 + tz_hi2;
    format!(
        "{:02}/{:02}/{:02},{:02}:{:02}:{:02}{}{:02}",
        swap(data[0]),
        swap(data[1]),
        swap(data[2]),
        swap(data[3]),
        swap(data[4]),
        swap(data[5]),
        if negative { '-' } else { '+' },
        tz_q
    )
}

// ---------------------------------------------------------------------------
// SMS-DELIVER parser
// ---------------------------------------------------------------------------

/// Parse a hex-encoded SMS-DELIVER PDU.
pub fn parse_sms_pdu(hex_pdu: &str) -> Result<SmsPdu, SmsError> {
    let buf = hex_to_bytes(hex_pdu).ok_or(SmsError::MalformedPdu("invalid hex"))?;
    let n = buf.len();
    let mut p = 0usize;

    if p >= n {
        return Err(SmsError::MalformedPdu("truncated before SCA"));
    }
    let sca_len = buf[p] as usize;
    p += 1;
    if p + sca_len > n {
        return Err(SmsError::MalformedPdu("SCA too long"));
    }
    p += sca_len;

    if p >= n {
        return Err(SmsError::MalformedPdu("truncated before first octet"));
    }
    let first_octet = buf[p];
    p += 1;
    let udhi = (first_octet & 0x40) != 0;

    // TP-OA
    if p >= n {
        return Err(SmsError::MalformedPdu("truncated before OA"));
    }
    let oa_digits = buf[p] as usize;
    p += 1;
    if p >= n {
        return Err(SmsError::MalformedPdu("truncated before OA TOA"));
    }
    let oa_toa = buf[p];
    p += 1;
    let oa_bytes = oa_digits.div_ceil(2);
    if p + oa_bytes > n {
        return Err(SmsError::MalformedPdu("OA bytes truncated"));
    }

    let sender = if (oa_toa & 0x70) == 0x50 {
        // Alphanumeric sender
        let scount = (oa_bytes * 8) / 7;
        let septets = unpack_septets(&buf[p..p + oa_bytes], scount, 0);
        gsm7_septets_to_utf8(&septets)
    } else {
        let mut s = decode_bcd_address(&buf[p..], oa_bytes, oa_digits);
        if (oa_toa & 0x70) == 0x10 {
            s = format!("+{}", s);
        }
        s
    };
    p += oa_bytes;

    // TP-PID
    if p >= n {
        return Err(SmsError::MalformedPdu("truncated before PID"));
    }
    p += 1;

    // TP-DCS
    if p >= n {
        return Err(SmsError::MalformedPdu("truncated before DCS"));
    }
    let dcs = buf[p];
    p += 1;

    #[derive(PartialEq)]
    enum Alpha {
        Gsm7,
        Eight,
        Ucs2,
    }
    let alpha = if (dcs & 0xC0) == 0x00 {
        match (dcs >> 2) & 0x03 {
            0 => Alpha::Gsm7,
            1 => Alpha::Eight,
            2 => Alpha::Ucs2,
            _ => Alpha::Gsm7,
        }
    } else if (dcs & 0xF0) == 0xF0 {
        if (dcs & 0x04) != 0 {
            Alpha::Eight
        } else {
            Alpha::Gsm7
        }
    } else if (dcs & 0xF0) == 0xE0 {
        Alpha::Ucs2
    } else {
        Alpha::Gsm7
    };

    // TP-SCTS
    if p + 7 > n {
        return Err(SmsError::MalformedPdu("truncated before SCTS"));
    }
    let timestamp = decode_scts(&buf[p..p + 7]);
    p += 7;

    // TP-UDL
    if p >= n {
        return Err(SmsError::MalformedPdu("truncated before UDL"));
    }
    let udl = buf[p] as usize;
    p += 1;

    let ud_remaining = n - p;
    let ud_bytes = if alpha != Alpha::Gsm7 {
        if udl > ud_remaining {
            return Err(SmsError::MalformedPdu("UDL > remaining"));
        }
        udl
    } else {
        ud_remaining
    };

    // UDH
    let mut udh_total = 0usize;
    let mut pdu = SmsPdu {
        sender,
        timestamp,
        ..Default::default()
    };

    if udhi {
        if ud_bytes < 1 {
            return Err(SmsError::MalformedPdu("UDH missing UDHL"));
        }
        let udhl = buf[p] as usize;
        udh_total = udhl + 1;
        if udh_total > ud_bytes {
            return Err(SmsError::MalformedPdu("UDH too long"));
        }

        let mut ie = p + 1;
        let ie_end = p + udh_total;
        while ie + 1 < ie_end {
            let iei = buf[ie];
            ie += 1;
            let iedl = buf[ie] as usize;
            ie += 1;
            if ie + iedl > ie_end {
                break;
            }
            match (iei, iedl) {
                (0x00, 3) => {
                    pdu.is_concatenated = true;
                    pdu.concat_ref = buf[ie] as u16;
                    pdu.concat_total = buf[ie + 1];
                    pdu.concat_part = buf[ie + 2];
                }
                (0x08, 4) => {
                    pdu.is_concatenated = true;
                    pdu.concat_ref = ((buf[ie] as u16) << 8) | buf[ie + 1] as u16;
                    pdu.concat_total = buf[ie + 2];
                    pdu.concat_part = buf[ie + 3];
                }
                _ => {}
            }
            ie += iedl;
        }
    }

    // Decode content
    pdu.content = match alpha {
        Alpha::Gsm7 => {
            if udhi {
                let udh_bits = udh_total * 8;
                let fill = (7 - (udh_bits % 7)) % 7;
                let header_septets = (udh_bits + fill) / 7;
                if header_septets > udl {
                    return Err(SmsError::MalformedPdu("UDH header septets > UDL"));
                }
                let body_septets = udl - header_septets;
                let bit_offset = udh_bits + fill;
                let septets = unpack_septets(&buf[p..p + ud_bytes], body_septets, bit_offset);
                gsm7_septets_to_utf8(&septets)
            } else {
                let septets = unpack_septets(&buf[p..p + ud_bytes], udl, 0);
                gsm7_septets_to_utf8(&septets)
            }
        }
        Alpha::Ucs2 => {
            let body_start = if udhi { udh_total } else { 0 };
            if body_start > ud_bytes {
                return Err(SmsError::MalformedPdu("UDH > UCS2 body"));
            }
            ucs2_bytes_to_utf8(&buf[p + body_start..p + ud_bytes])
        }
        Alpha::Eight => {
            let body_start = if udhi { udh_total } else { 0 };
            if body_start > ud_bytes {
                return Err(SmsError::MalformedPdu("UDH > 8bit body"));
            }
            buf[p + body_start..p + ud_bytes]
                .iter()
                .map(|&b| b as char)
                .collect()
        }
    };

    Ok(pdu)
}

// ---------------------------------------------------------------------------
// SMS-SUBMIT encoder
// ---------------------------------------------------------------------------

/// Encode unicode code points from UTF-8.
fn utf8_to_codepoints(s: &str) -> Vec<u32> {
    s.chars().map(|c| c as u32).collect()
}

/// Try to encode a Unicode code point as GSM-7 septets.
/// Returns 0, 1 or 2 septets.
fn encode_gsm7_char(cp: u32, out: &mut [u8]) -> usize {
    match cp {
        // Characters with non-identity ASCII mapping
        0x24 => {
            out[0] = 0x02;
            1
        }
        0x40 => {
            out[0] = 0x00;
            1
        }
        0x5F => {
            out[0] = 0x11;
            1
        }
        0x60 => 0, // backtick not in GSM-7
        // Extension table (2 septets: ESC + code)
        0x5B => {
            out[0] = 0x1B;
            out[1] = 0x3C;
            2
        } // [
        0x5C => {
            out[0] = 0x1B;
            out[1] = 0x2F;
            2
        } // backslash
        0x5D => {
            out[0] = 0x1B;
            out[1] = 0x3E;
            2
        } // ]
        0x5E => {
            out[0] = 0x1B;
            out[1] = 0x14;
            2
        } // ^
        0x7B => {
            out[0] = 0x1B;
            out[1] = 0x28;
            2
        } // {
        0x7C => {
            out[0] = 0x1B;
            out[1] = 0x40;
            2
        } // |
        0x7D => {
            out[0] = 0x1B;
            out[1] = 0x29;
            2
        } // }
        0x7E => {
            out[0] = 0x1B;
            out[1] = 0x3D;
            2
        } // ~
        // Direct ASCII range (after special cases above)
        0x20..=0x7E => {
            out[0] = cp as u8;
            1
        }
        0x0A => {
            out[0] = 0x0A;
            1
        } // LF
        0x0D => {
            out[0] = 0x0D;
            1
        } // CR
        0x0C => {
            out[0] = 0x1B;
            out[1] = 0x0A;
            2
        } // form feed
        // Non-ASCII GSM-7 basic table
        0x00A1 => {
            out[0] = 0x40;
            1
        }
        0x00A3 => {
            out[0] = 0x01;
            1
        }
        0x00A4 => {
            out[0] = 0x24;
            1
        }
        0x00A5 => {
            out[0] = 0x03;
            1
        }
        0x00A7 => {
            out[0] = 0x5F;
            1
        }
        0x00BF => {
            out[0] = 0x60;
            1
        }
        0x00C4 => {
            out[0] = 0x5B;
            1
        }
        0x00C5 => {
            out[0] = 0x0E;
            1
        }
        0x00C6 => {
            out[0] = 0x1C;
            1
        }
        0x00C7 => {
            out[0] = 0x09;
            1
        }
        0x00C9 => {
            out[0] = 0x1F;
            1
        }
        0x00D1 => {
            out[0] = 0x5D;
            1
        }
        0x00D6 => {
            out[0] = 0x5C;
            1
        }
        0x00D8 => {
            out[0] = 0x0B;
            1
        }
        0x00DC => {
            out[0] = 0x5E;
            1
        }
        0x00DF => {
            out[0] = 0x1E;
            1
        }
        0x00E0 => {
            out[0] = 0x7F;
            1
        }
        0x00E4 => {
            out[0] = 0x7B;
            1
        }
        0x00E5 => {
            out[0] = 0x0F;
            1
        }
        0x00E6 => {
            out[0] = 0x1D;
            1
        }
        0x00E8 => {
            out[0] = 0x04;
            1
        }
        0x00E9 => {
            out[0] = 0x05;
            1
        }
        0x00EC => {
            out[0] = 0x07;
            1
        }
        0x00F1 => {
            out[0] = 0x7D;
            1
        }
        0x00F2 => {
            out[0] = 0x08;
            1
        }
        0x00F6 => {
            out[0] = 0x7C;
            1
        }
        0x00F8 => {
            out[0] = 0x0C;
            1
        }
        0x00F9 => {
            out[0] = 0x06;
            1
        }
        0x00FC => {
            out[0] = 0x7E;
            1
        }
        // Greek (basic table)
        0x0393 => {
            out[0] = 0x13;
            1
        }
        0x0394 => {
            out[0] = 0x10;
            1
        }
        0x0398 => {
            out[0] = 0x19;
            1
        }
        0x039B => {
            out[0] = 0x14;
            1
        }
        0x039E => {
            out[0] = 0x1A;
            1
        }
        0x03A0 => {
            out[0] = 0x16;
            1
        }
        0x03A3 => {
            out[0] = 0x18;
            1
        }
        0x03A6 => {
            out[0] = 0x12;
            1
        }
        0x03A8 => {
            out[0] = 0x17;
            1
        }
        0x03A9 => {
            out[0] = 0x15;
            1
        }
        // Euro (extension)
        0x20AC => {
            out[0] = 0x1B;
            out[1] = 0x65;
            2
        }
        _ => 0,
    }
}

/// Pack septets into bytes with optional bit offset (for UDH alignment).
pub fn pack_septets(septets: &[u8], bit_offset: usize) -> Vec<u8> {
    if septets.is_empty() {
        return vec![0u8; bit_offset.div_ceil(8)];
    }
    let num_bytes = (bit_offset + septets.len() * 7).div_ceil(8);
    let mut out = vec![0u8; num_bytes];
    for (i, &s) in septets.iter().enumerate() {
        let val = (s & 0x7F) as u16;
        let bit_pos = bit_offset + i * 7;
        let byte_idx = bit_pos / 8;
        let bit_off = bit_pos % 8;
        let shifted = val << bit_off;
        out[byte_idx] |= (shifted & 0xFF) as u8;
        if byte_idx + 1 < num_bytes {
            out[byte_idx + 1] |= (shifted >> 8) as u8;
        }
    }
    out
}

/// Returns true iff every code point in `s` can be represented in GSM-7.
pub fn is_gsm7_compatible(s: &str) -> bool {
    let mut buf = [0u8; 2];
    for cp in utf8_to_codepoints(s) {
        if encode_gsm7_char(cp, &mut buf) == 0 {
            return false;
        }
    }
    true
}

fn code_points_to_utf16be(cps: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(cps.len() * 2);
    for &cp in cps {
        if cp <= 0xFFFF {
            out.push((cp >> 8) as u8);
            out.push((cp & 0xFF) as u8);
        } else if cp <= 0x10FFFF {
            let adj = cp - 0x10000;
            let hi = (0xD800 + (adj >> 10)) as u16;
            let lo = (0xDC00 + (adj & 0x3FF)) as u16;
            out.push((hi >> 8) as u8);
            out.push((hi & 0xFF) as u8);
            out.push((lo >> 8) as u8);
            out.push((lo & 0xFF) as u8);
        }
    }
    out
}

fn encode_bcd_phone(phone: &str, out: &mut Vec<u8>) {
    let mut digits = String::new();
    let mut international = false;
    for c in phone.chars() {
        if c == '+' {
            international = true;
        } else if c.is_ascii_digit() {
            digits.push(c);
        }
    }
    out.push(digits.len() as u8);
    out.push(if international { 0x91 } else { 0x81 });
    let dbytes: Vec<u8> = digits.bytes().collect();
    let mut i = 0;
    while i < dbytes.len() {
        let lo = dbytes[i] - b'0';
        let hi = if i + 1 < dbytes.len() {
            dbytes[i + 1] - b'0'
        } else {
            0x0F
        };
        out.push((hi << 4) | lo);
        i += 2;
    }
}

/// Build SMS-SUBMIT PDU(s) for the given phone and UTF-8 body.
/// Returns empty vec on error (empty inputs or body exceeds `max_parts`).
pub fn build_sms_submit_pdus(
    phone: &str,
    body: &str,
    max_parts: usize,
    request_status_report: bool,
) -> Vec<SmsSubmitPdu> {
    if phone.is_empty() || body.is_empty() {
        return vec![];
    }

    let cps = utf8_to_codepoints(body);

    // Try GSM-7
    let mut gsm7: Vec<u8> = Vec::new();
    let mut buf = [0u8; 2];
    let mut can_gsm7 = true;
    for &cp in &cps {
        let n = encode_gsm7_char(cp, &mut buf);
        if n == 0 {
            can_gsm7 = false;
            break;
        }
        gsm7.extend_from_slice(&buf[..n]);
    }

    if can_gsm7 {
        return build_gsm7_pdus(phone, &gsm7, max_parts, request_status_report);
    }

    // UCS-2
    let ucs2 = code_points_to_utf16be(&cps);
    build_ucs2_pdus(phone, &ucs2, max_parts, request_status_report)
}

fn build_gsm7_pdus(phone: &str, septets: &[u8], max_parts: usize, srr: bool) -> Vec<SmsSubmitPdu> {
    if septets.len() <= 160 {
        // Single-part
        let packed = pack_septets(septets, 0);
        let mut pdu = vec![0x00u8]; // SCA
        pdu.push(if srr { 0x21 } else { 0x01 }); // first octet
        pdu.push(0x00); // TP-MR
        encode_bcd_phone(phone, &mut pdu);
        pdu.push(0x00); // PID
        pdu.push(0x00); // DCS GSM7
        pdu.push(septets.len() as u8);
        pdu.extend_from_slice(&packed);
        return vec![SmsSubmitPdu {
            tpdu_len: (pdu.len() - 1) as u8,
            hex: bytes_to_hex(&pdu),
        }];
    }

    // Multi-part: split into 153-septet chunks (ESC-safe)
    let mut slices: Vec<(usize, usize)> = vec![];
    let mut pos = 0;
    while pos < septets.len() {
        let remaining = septets.len() - pos;
        let mut chunk = remaining.min(153);
        if chunk == 153 && remaining > 153 {
            if septets[pos + 152] == 0x1B {
                chunk = 152;
            } else if septets[pos + 151] == 0x1B {
                chunk = 151;
            }
        }
        slices.push((pos, pos + chunk));
        pos += chunk;
    }

    let total = slices.len();
    if total > max_parts {
        return vec![];
    }

    static REF: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    let concat_ref = REF.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let mut result = Vec::with_capacity(total);
    for (part_num, (start, end)) in slices.iter().enumerate() {
        let chunk = &septets[*start..*end];
        // UDH: 6 bytes; first septet at bit 49
        let mut packed = pack_septets(chunk, 49);
        packed[0] = 0x05;
        packed[1] = 0x00;
        packed[2] = 0x03;
        packed[3] = concat_ref;
        packed[4] = total as u8;
        packed[5] = (part_num + 1) as u8;

        let udl = (7 + chunk.len()) as u8; // 7 header septets + body

        let mut pdu = vec![0x00u8]; // SCA
        pdu.push(if srr { 0x61 } else { 0x41 }); // UDHI set
        pdu.push(0x00);
        encode_bcd_phone(phone, &mut pdu);
        pdu.push(0x00);
        pdu.push(0x00);
        pdu.push(udl);
        pdu.extend_from_slice(&packed);
        result.push(SmsSubmitPdu {
            tpdu_len: (pdu.len() - 1) as u8,
            hex: bytes_to_hex(&pdu),
        });
    }
    result
}

fn build_ucs2_pdus(phone: &str, ucs2: &[u8], max_parts: usize, srr: bool) -> Vec<SmsSubmitPdu> {
    if ucs2.len() <= 140 {
        let mut pdu = vec![0x00u8];
        pdu.push(if srr { 0x21 } else { 0x01 });
        pdu.push(0x00);
        encode_bcd_phone(phone, &mut pdu);
        pdu.push(0x00);
        pdu.push(0x08); // DCS UCS2
        pdu.push(ucs2.len() as u8);
        pdu.extend_from_slice(ucs2);
        return vec![SmsSubmitPdu {
            tpdu_len: (pdu.len() - 1) as u8,
            hex: bytes_to_hex(&pdu),
        }];
    }

    // Multi-part: 134-byte chunks (surrogate-safe)
    let mut slices: Vec<(usize, usize)> = vec![];
    let mut pos = 0;
    while pos < ucs2.len() {
        let remaining = ucs2.len() - pos;
        let mut chunk = remaining.min(134);
        if chunk == 134 && remaining > 134 {
            let hi = (ucs2[pos + 132] as u16) << 8 | ucs2[pos + 133] as u16;
            if (0xD800..=0xDBFF).contains(&hi) {
                chunk = 132;
            }
        }
        slices.push((pos, pos + chunk));
        pos += chunk;
    }

    let total = slices.len();
    if total > max_parts {
        return vec![];
    }

    static REF2: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    let concat_ref = REF2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let mut result = Vec::with_capacity(total);
    for (part_num, (start, end)) in slices.iter().enumerate() {
        let chunk = &ucs2[*start..*end];
        let udl = (6 + chunk.len()) as u8;
        let mut ud = vec![
            0x05u8,
            0x00,
            0x03,
            concat_ref,
            total as u8,
            (part_num + 1) as u8,
        ];
        ud.extend_from_slice(chunk);

        let mut pdu = vec![0x00u8];
        pdu.push(if srr { 0x61 } else { 0x41 });
        pdu.push(0x00);
        encode_bcd_phone(phone, &mut pdu);
        pdu.push(0x00);
        pdu.push(0x08);
        pdu.push(udl);
        pdu.extend_from_slice(&ud);
        result.push(SmsSubmitPdu {
            tpdu_len: (pdu.len() - 1) as u8,
            hex: bytes_to_hex(&pdu),
        });
    }
    result
}

/// Return the number of SMS parts needed for `body` (0 on error).
pub fn count_sms_parts(body: &str, max_parts: usize) -> usize {
    build_sms_submit_pdus("+1", body, max_parts, false).len()
}

// ---------------------------------------------------------------------------
// SMS-STATUS-REPORT parser
// ---------------------------------------------------------------------------

/// Parse a hex-encoded SMS-STATUS-REPORT PDU (+CDS URC).
pub fn parse_status_report(hex_pdu: &str) -> Result<StatusReport, SmsError> {
    let raw =
        hex_to_bytes(hex_pdu).ok_or(SmsError::MalformedPdu("invalid hex in status report"))?;
    let n = raw.len();
    let mut pos = 0usize;

    if pos >= n {
        return Err(SmsError::MalformedPdu("empty status report"));
    }
    let sca_len = raw[pos] as usize;
    pos += 1 + sca_len;

    if pos >= n {
        return Err(SmsError::MalformedPdu("truncated SR first octet"));
    }
    let first = raw[pos];
    pos += 1;
    if (first & 0x03) != 0x02 {
        return Err(SmsError::MalformedPdu("not a STATUS-REPORT"));
    }

    if pos >= n {
        return Err(SmsError::MalformedPdu("truncated SR MR"));
    }
    let message_ref = raw[pos];
    pos += 1;

    if pos + 1 >= n {
        return Err(SmsError::MalformedPdu("truncated SR RA"));
    }
    let ra_digits = raw[pos] as usize;
    pos += 1;
    let ra_toa = raw[pos];
    pos += 1;
    let ra_bytes = ra_digits.div_ceil(2);
    if pos + ra_bytes > n {
        return Err(SmsError::MalformedPdu("SR RA bytes truncated"));
    }

    let mut recipient = if (ra_toa & 0x70) == 0x10 {
        "+".to_string()
    } else {
        String::new()
    };
    for b in 0..ra_bytes {
        let byte = raw[pos + b];
        let d1 = byte & 0x0F;
        let d2 = (byte >> 4) & 0x0F;
        recipient.push((b'0' + d1) as char);
        if b * 2 + 1 < ra_digits {
            recipient.push((b'0' + d2) as char);
        }
    }
    pos += ra_bytes;

    if pos + 14 > n {
        return Err(SmsError::MalformedPdu("SR timestamps truncated"));
    }
    let sc_timestamp = decode_scts(&raw[pos..pos + 7]);
    pos += 7;
    let discharge_time = decode_scts(&raw[pos..pos + 7]);
    pos += 7;

    if pos >= n {
        return Err(SmsError::MalformedPdu("SR status byte missing"));
    }
    let status = raw[pos];
    let delivered = status == 0x00;
    let status_text = sr_status_text(status);

    Ok(StatusReport {
        message_ref,
        recipient,
        sc_timestamp,
        discharge_time,
        status,
        delivered,
        status_text,
    })
}

fn sr_status_text(st: u8) -> String {
    match st {
        0x00 => "delivered".into(),
        0x01 => "forwarded, unconfirmed".into(),
        0x02 => "replaced".into(),
        0x20..=0x2F => match st {
            0x20 => "temporary failure, still trying (congestion)".into(),
            0x21 => "temporary failure, still trying (SME busy)".into(),
            _ => format!("temporary failure, still trying ({:#04x})", st),
        },
        0x40..=0x4F => match st {
            0x40 => "permanent failure (remote procedure error)".into(),
            0x41 => "permanent failure (incompatible destination)".into(),
            _ => format!("permanent failure ({:#04x})", st),
        },
        0x60..=0x6F => match st {
            0x60 => "temporary failure, stopped trying (congestion)".into(),
            _ => format!("temporary failure, stopped trying ({:#04x})", st),
        },
        _ => format!("unknown status {:#04x}", st),
    }
}
