//! Declarative Scenario DSL for end-to-end tests.

use crate::bridge::{forwarder::forward_sms, reply_router::ReplyRouter};
use crate::log_ring::LogRing;
use crate::persist::mem::MemStore;
use crate::sms::{codec::parse_sms_pdu, SmsMessage};
use crate::testing::mocks::{RecordingMessenger, ScriptedModem};

/// Assertion to check after the scenario runs.
enum Assertion {
    ImSentContains(String),
    ImSentCount(usize),
    ImSentNone,
    PdusSentCount(usize),
    HangUpCount(usize),
}

/// End-to-end test scenario.
pub struct Scenario {
    name: String,
    modem: ScriptedModem,
    messenger: RecordingMessenger,
    store: MemStore,
    assertions: Vec<Assertion>,
    sms_pdu_inputs: Vec<String>,
}

impl Scenario {
    pub fn new(name: &str) -> Self {
        Scenario {
            name: name.to_string(),
            modem: ScriptedModem::new(),
            messenger: RecordingMessenger::new(),
            store: MemStore::new(),
            assertions: Vec::new(),
            sms_pdu_inputs: Vec::new(),
        }
    }

    /// Inject a URC that will be returned by the modem.
    pub fn modem_urc(mut self, urc: &str) -> Self {
        self.modem = self.modem.push_urc(urc);
        self
    }

    /// Feed a raw PDU hex string for forwarding.
    pub fn with_pdu(mut self, hex: &str) -> Self {
        self.sms_pdu_inputs.push(hex.to_string());
        self
    }

    /// Assert at least one sent IM message contains `substr`.
    pub fn expect_im_sent_contains(mut self, substr: &str) -> Self {
        self.assertions
            .push(Assertion::ImSentContains(substr.to_string()));
        self
    }

    /// Assert exactly `n` IM messages were sent.
    pub fn expect_im_sent_count(mut self, n: usize) -> Self {
        self.assertions.push(Assertion::ImSentCount(n));
        self
    }

    /// Assert no IM messages were sent.
    pub fn expect_im_sent_none(mut self) -> Self {
        self.assertions.push(Assertion::ImSentNone);
        self
    }

    /// Assert exactly `n` PDUs were sent via the modem.
    pub fn expect_pdus_sent(mut self, n: usize) -> Self {
        self.assertions.push(Assertion::PdusSentCount(n));
        self
    }

    /// Assert hang_up was called `n` times.
    pub fn expect_hangup_count(mut self, n: usize) -> Self {
        self.assertions.push(Assertion::HangUpCount(n));
        self
    }

    /// Run the scenario and check all assertions. Panics on failure.
    pub fn run(mut self) {
        let mut router = ReplyRouter::new();
        let mut log = LogRing::new();

        // Process PDU inputs
        let pdu_inputs = std::mem::take(&mut self.sms_pdu_inputs);
        for hex in &pdu_inputs {
            let pdu = parse_sms_pdu(hex).expect("invalid PDU in scenario");
            let sms = SmsMessage {
                sender: pdu.sender,
                body: pdu.content,
                timestamp: pdu.timestamp,
                slot: 0,
            };
            forward_sms(
                &sms,
                &mut self.messenger,
                &mut router,
                &mut log,
                &mut self.store,
            );
        }

        // Check assertions
        for assertion in &self.assertions {
            match assertion {
                Assertion::ImSentContains(s) => {
                    assert!(
                        self.messenger.contains_sent(s),
                        "[{}] expected IM message containing {:?}, sent messages: {:?}",
                        self.name,
                        s,
                        self.messenger
                            .sent
                            .iter()
                            .map(|m| &m.text)
                            .collect::<Vec<_>>()
                    );
                }
                Assertion::ImSentCount(n) => {
                    assert_eq!(
                        self.messenger.sent_count(),
                        *n,
                        "[{}] expected {} IM messages, got {}",
                        self.name,
                        n,
                        self.messenger.sent_count()
                    );
                }
                Assertion::ImSentNone => {
                    assert_eq!(
                        self.messenger.sent_count(),
                        0,
                        "[{}] expected no IM messages, got {} (first: {:?})",
                        self.name,
                        self.messenger.sent_count(),
                        self.messenger.last_sent()
                    );
                }
                Assertion::PdusSentCount(n) => {
                    assert_eq!(
                        self.modem.sent_pdus.len(),
                        *n,
                        "[{}] expected {} PDUs sent, got {}",
                        self.name,
                        n,
                        self.modem.sent_pdus.len()
                    );
                }
                Assertion::HangUpCount(n) => {
                    assert_eq!(
                        self.modem.hang_up_count, *n,
                        "[{}] expected {} hang_ups, got {}",
                        self.name, n, self.modem.hang_up_count
                    );
                }
            }
        }

        // Verify all scripted AT steps were consumed
        self.modem.check_consumed();
    }
}
