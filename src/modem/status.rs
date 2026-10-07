//! Bounded evidence for registration queries and PDN context events.

use super::{AtResponse, ModemError};
use std::time::Duration;

const MAX_REGISTRATION_REPLY_BYTES: usize = 128;
// Two snippets plus the transition summary fit in one flash-backed log row.
const MAX_REGISTRATION_DIAGNOSTIC_BYTES: usize = 76;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationDomain {
    Circuit,
    Eps,
}

impl RegistrationDomain {
    pub fn command(self) -> &'static str {
        match self {
            Self::Circuit => "+CREG?",
            Self::Eps => "+CEREG?",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Circuit => "+CREG:",
            Self::Eps => "+CEREG:",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RegistrationQuery {
    domain: RegistrationDomain,
    pub stat: Option<u8>,
    pub response: String,
    pub elapsed_ms: u32,
    pub error: Option<String>,
}

impl Default for RegistrationQuery {
    fn default() -> Self {
        Self {
            domain: RegistrationDomain::Circuit,
            stat: None,
            response: String::new(),
            elapsed_ms: 0,
            error: Some("not queried".into()),
        }
    }
}

impl RegistrationQuery {
    pub fn from_response(
        domain: RegistrationDomain,
        response: Result<AtResponse, ModemError>,
        elapsed: Duration,
    ) -> Self {
        let mut query = Self {
            domain,
            elapsed_ms: elapsed_ms(elapsed),
            ..Self::default()
        };
        match response {
            Ok(response) => {
                // Retain only the relevant line; unrelated URCs must not replace
                // registration evidence, and a malformed modem cannot grow logs.
                let line = response
                    .body
                    .lines()
                    .find(|line| line.trim().starts_with(domain.prefix()))
                    .unwrap_or(response.body.trim());
                query.response = bounded_reply(line.trim());
                if response.ok {
                    query.stat = parse_registration_stat(&response.body, domain.prefix());
                    query.error = query.stat.is_none().then(|| {
                        format!(
                            "invalid {} response",
                            domain.command().trim_end_matches('?')
                        )
                    });
                } else {
                    query.error = Some(format!("AT{} rejected", domain.command()));
                }
            }
            Err(error) => query.error = Some(error.to_string()),
        }
        query
    }

    /// Unknown and unsupported modem states are not evidence of deregistration.
    pub fn registered(&self) -> Option<bool> {
        if self.error.is_some()
            || (self.domain == RegistrationDomain::Eps && matches!(self.stat, Some(6 | 7)))
        {
            None
        } else {
            self.stat.and_then(registered_for_stat)
        }
    }

    pub fn sms_only(&self) -> bool {
        self.domain == RegistrationDomain::Circuit
            && self.error.is_none()
            && matches!(self.stat, Some(6 | 7))
    }

    pub fn diagnostic(&self) -> String {
        let reply = if self.response.is_empty() {
            "no response"
        } else {
            &self.response
        };
        let stat = self
            .stat
            .map_or_else(|| "?".into(), |stat| stat.to_string());
        let detail = match &self.error {
            Some(error) => format!("{error}; {reply}"),
            None => reply.to_owned(),
        };
        // Put parsed state and timing first. Normalize characters that expand
        // during flash escaping so a long reply cannot hide the other domain.
        let mut diagnostic = format!(
            "{} stat={} {}ms: {}",
            self.domain.command(),
            stat,
            self.elapsed_ms,
            detail
        )
        .replace(['\\', '\t', '\r', '\n'], " ");
        if diagnostic.len() > MAX_REGISTRATION_DIAGNOSTIC_BYTES {
            diagnostic
                .truncate(diagnostic.floor_char_boundary(MAX_REGISTRATION_DIAGNOSTIC_BYTES - 3));
            diagnostic.push_str("...");
        }
        diagnostic
    }
}

fn bounded_reply(text: &str) -> String {
    let end = text.floor_char_boundary(text.len().min(MAX_REGISTRATION_REPLY_BYTES));
    text[..end].replace(['\r', '\n'], " ")
}

pub(super) fn elapsed_ms(elapsed: Duration) -> u32 {
    elapsed.as_millis().min(u128::from(u32::MAX)) as u32
}

pub(super) fn parse_registration_stat(body: &str, prefix: &str) -> Option<u8> {
    body.lines()
        .find_map(|line| line.trim().strip_prefix(prefix))
        .and_then(|fields| fields.split(',').nth(1))
        .and_then(|stat| stat.trim().parse().ok())
}

pub(super) fn registered_for_stat(stat: u8) -> Option<bool> {
    match stat {
        1 | 5 | 6 | 7 => Some(true),
        0 | 2 | 3 | 11 => Some(false),
        _ => None,
    }
}

/// The most recent event of each kind; counters still include all contexts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdnEvent {
    pub cid: u8,
    /// Defined only for ME PDN ACT; NW's optional field is WLAN offload.
    pub reason: Option<u8>,
    pub network_initiated: bool,
}

impl PdnEvent {
    pub fn parse(line: &str) -> Option<Self> {
        let event = line.trim().strip_prefix("+CGEV:")?.trim();
        let (network_initiated, event) = if let Some(event) = event.strip_prefix("ME ") {
            (false, event)
        } else {
            (true, event.strip_prefix("NW ")?)
        };
        let (activation, fields) = if let Some(fields) = event.strip_prefix("PDN ACT ") {
            (true, fields)
        } else {
            (false, event.strip_prefix("PDN DEACT ")?)
        };
        let mut fields = fields.split(',');
        Some(Self {
            cid: fields.next()?.trim().parse().ok()?,
            reason: if activation && !network_initiated {
                fields.next().and_then(|field| field.trim().parse().ok())
            } else {
                None
            },
            network_initiated,
        })
    }
}

impl std::fmt::Display for PdnEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} cid={}",
            if self.network_initiated { "NW" } else { "ME" },
            self.cid
        )?;
        if let Some(reason) = self.reason {
            write!(f, " reason={reason}")?;
        }
        Ok(())
    }
}
