//! Mock implementations of modem and message-sink boundaries.

use crate::im::{InlineKeyboard, MessageFormat, MessageId, MessageSink, MessengerError};
use crate::modem::a76xx::at::UartPort;
use crate::modem::{AtResponse, AtTransport, ModemError, ModemPort};
use std::collections::VecDeque;
use std::time::Duration;

// ---------------------------------------------------------------------------
// MockUart
// ---------------------------------------------------------------------------

/// Byte-level UART mock for testing `AtPort<MockUart>` without hardware.
///
/// ## Timing model
///
/// On real hardware the modem only sends its response *after* it receives a
/// command.  `drain_urcs()` in `AtPort::send_at` drains the UART FIFO
/// *before* writing the command — so response bytes must not be visible yet.
///
/// This mock mirrors that model:
/// - `feed` / `feed_line` → bytes visible **immediately** (pre-command URCs).
/// - `queue_response` / `queue_response_line` → bytes injected into `rx`
///   on the **next `write_all`** call (one batch per call, FIFO order).
///
/// For `send_at_connect_payload` (two writes: command + payload) call
/// `queue_response_line` once per expected write to build two batches.
pub struct MockUart {
    rx: VecDeque<u8>,
    /// Pending responses: each inner Vec is flushed into `rx` on one `write_all`.
    write_responses: VecDeque<Vec<u8>>,
    /// Accumulator for the response being built by the current `queue_response*` calls.
    pending_response: Vec<u8>,
    pub tx: Vec<u8>,
}

impl MockUart {
    pub fn new() -> Self {
        MockUart {
            rx: VecDeque::new(),
            write_responses: VecDeque::new(),
            pending_response: Vec::new(),
            tx: Vec::new(),
        }
    }

    /// Push bytes that are immediately available (pre-command URCs in FIFO).
    pub fn feed(&mut self, data: &[u8]) {
        self.rx.extend(data.iter().copied());
    }

    /// Push a line with `\r\n` that is immediately available.
    pub fn feed_line(&mut self, line: &str) {
        self.feed(line.as_bytes());
        self.feed(b"\r\n");
    }

    /// Append bytes to the *current* queued response batch.
    pub fn queue_response(&mut self, data: &[u8]) {
        self.pending_response.extend_from_slice(data);
    }

    /// Append a line (with `\r\n`) to the current queued response batch.
    pub fn queue_response_line(&mut self, line: &str) {
        self.queue_response(line.as_bytes());
        self.queue_response(b"\r\n");
    }

    /// Finalise the current response batch so the *next* `queue_response*`
    /// calls start building the batch for the write after that.
    ///
    /// Called automatically on `write_all` — only needed explicitly when you
    /// want two separate write-triggered response batches (e.g. for
    /// `send_at_connect_payload`'s two-write sequence).
    pub fn finish_response(&mut self) {
        let batch = std::mem::take(&mut self.pending_response);
        if !batch.is_empty() {
            self.write_responses.push_back(batch);
        }
    }

    /// View what has been transmitted as UTF-8 (panics if non-UTF-8).
    pub fn sent_str(&self) -> &str {
        std::str::from_utf8(&self.tx).expect("tx is valid UTF-8")
    }
}

impl UartPort for MockUart {
    fn read_byte(&mut self, _ticks: u32) -> Option<u8> {
        self.rx.pop_front()
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), ModemError> {
        self.tx.extend_from_slice(data);
        // Finalise current pending batch, then inject the oldest queued response.
        self.finish_response();
        if let Some(batch) = self.write_responses.pop_front() {
            self.rx.extend(batch.iter().copied());
        }
        Ok(())
    }
}

impl Default for MockUart {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ScriptedModem
// ---------------------------------------------------------------------------

/// A scripted AT command/response pair.
pub struct AtScript {
    pub command_suffix: String, // what comes after "AT" (without CRLF)
    pub response_body: String,
    pub ok: bool,
}

/// Programmable modem mock.
///
/// Feed it a script of (command, response) pairs.
/// Unconsumed script steps fail the test via `check_consumed()`.
pub struct ScriptedModem {
    script: VecDeque<AtScript>,
    urc_queue: VecDeque<String>,
    pub sent_pdus: Vec<(String, u8)>, // (hex, tpdu_len)
    pub hang_up_count: usize,
}

impl ScriptedModem {
    pub fn new() -> Self {
        ScriptedModem {
            script: VecDeque::new(),
            urc_queue: VecDeque::new(),
            sent_pdus: Vec::new(),
            hang_up_count: 0,
        }
    }

    /// Push an expected (command_suffix, body, ok) interaction.
    pub fn expect(mut self, cmd: &str, body: &str, ok: bool) -> Self {
        self.script.push_back(AtScript {
            command_suffix: cmd.to_string(),
            response_body: body.to_string(),
            ok,
        });
        self
    }

    /// Push a URC that will be returned by the next `poll_urc()`.
    pub fn push_urc(mut self, urc: &str) -> Self {
        self.urc_queue.push_back(urc.to_string());
        self
    }

    /// Inject a URC at runtime (after construction).
    pub fn inject_urc(&mut self, urc: &str) {
        self.urc_queue.push_back(urc.to_string());
    }

    /// Assert all scripted steps were consumed. Panics if any remain.
    pub fn check_consumed(&self) {
        if !self.script.is_empty() {
            let remaining: Vec<_> = self.script.iter().map(|s| &s.command_suffix).collect();
            panic!(
                "ScriptedModem: {} unconsumed script steps: {:?}",
                self.script.len(),
                remaining
            );
        }
    }
}

impl AtTransport for ScriptedModem {
    fn send_at(&mut self, cmd: &str) -> Result<AtResponse, ModemError> {
        let Some(step) = self.script.pop_front() else {
            return Err(ModemError::AtError(format!(
                "unexpected AT command: AT{}",
                cmd
            )));
        };
        if step.command_suffix != cmd {
            panic!(
                "ScriptedModem: expected AT{} but got AT{}",
                step.command_suffix, cmd
            );
        }
        Ok(AtResponse {
            body: step.response_body,
            ok: step.ok,
        })
    }

    fn poll_urc(&mut self) -> Option<String> {
        self.urc_queue.pop_front()
    }

    fn write_raw(&mut self, _data: &[u8]) -> Result<(), ModemError> {
        // ScriptedModem overrides send_pdu_sms, so write_raw is never reached.
        unreachable!("ScriptedModem: write_raw should not be called directly")
    }

    fn wait_for_prompt(&mut self, _prompt: u8, _timeout: Duration) -> bool {
        // ScriptedModem overrides send_pdu_sms, so this is never reached.
        unreachable!("ScriptedModem: wait_for_prompt should not be called directly")
    }
}

impl ModemPort for ScriptedModem {
    fn send_pdu_sms(&mut self, hex: &str, tpdu_len: u8) -> Result<u8, ModemError> {
        self.sent_pdus.push((hex.to_string(), tpdu_len));
        Ok(1) // fake MR = 1
    }

    fn hang_up(&mut self) -> Result<(), ModemError> {
        self.hang_up_count += 1;
        Ok(())
    }
}

impl Default for ScriptedModem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// RecordingMessenger
// ---------------------------------------------------------------------------

/// Captured outbound message.
#[derive(Debug, Clone)]
pub struct SentMessage {
    pub text: String,
    pub id: MessageId,
    pub keyboard: Option<InlineKeyboard>,
    pub format: MessageFormat,
}

/// Captured edited message.
#[derive(Debug, Clone)]
pub struct EditedMessage {
    pub text: String,
    pub id: MessageId,
    pub keyboard: Option<InlineKeyboard>,
    pub format: MessageFormat,
}

/// Records sent messages.
pub struct RecordingMessenger {
    pub sent: Vec<SentMessage>,
    edited: Vec<EditedMessage>,
    answered_callbacks: Vec<String>,
    next_id: i64,
}

impl RecordingMessenger {
    pub fn new() -> Self {
        RecordingMessenger {
            sent: Vec::new(),
            edited: Vec::new(),
            answered_callbacks: Vec::new(),
            next_id: 1000,
        }
    }

    pub fn sent_count(&self) -> usize {
        self.sent.len()
    }
    pub fn last_sent(&self) -> Option<&str> {
        self.sent.last().map(|m| m.text.as_str())
    }
    pub fn last_sent_keyboard(&self) -> Option<&InlineKeyboard> {
        self.sent.last().and_then(|m| m.keyboard.as_ref())
    }
    pub fn contains_sent(&self, substr: &str) -> bool {
        self.sent.iter().any(|m| m.text.contains(substr))
    }
    pub fn edited_count(&self) -> usize {
        self.edited.len()
    }
    pub fn last_edited(&self) -> Option<&EditedMessage> {
        self.edited.last()
    }
    pub fn answered_callback_count(&self) -> usize {
        self.answered_callbacks.len()
    }
}

impl MessageSink for RecordingMessenger {
    fn send_message(&mut self, text: &str) -> Result<MessageId, MessengerError> {
        let id = self.next_id;
        self.next_id += 1;
        self.sent.push(SentMessage {
            text: text.to_string(),
            id,
            keyboard: None,
            format: MessageFormat::Plain,
        });
        Ok(id)
    }

    fn send_message_with_format(
        &mut self,
        text: &str,
        format: MessageFormat,
    ) -> Result<MessageId, MessengerError> {
        let id = self.next_id;
        self.next_id += 1;
        self.sent.push(SentMessage {
            text: text.to_string(),
            id,
            keyboard: None,
            format,
        });
        Ok(id)
    }

    fn send_message_with_keyboard(
        &mut self,
        text: &str,
        keyboard: &InlineKeyboard,
    ) -> Result<MessageId, MessengerError> {
        let id = self.next_id;
        self.next_id += 1;
        self.sent.push(SentMessage {
            text: text.to_string(),
            id,
            keyboard: Some(keyboard.clone()),
            format: MessageFormat::Plain,
        });
        Ok(id)
    }

    fn send_message_with_keyboard_and_format(
        &mut self,
        text: &str,
        keyboard: &InlineKeyboard,
        format: MessageFormat,
    ) -> Result<MessageId, MessengerError> {
        let id = self.next_id;
        self.next_id += 1;
        self.sent.push(SentMessage {
            text: text.to_string(),
            id,
            keyboard: Some(keyboard.clone()),
            format,
        });
        Ok(id)
    }

    fn edit_message(&mut self, message_id: MessageId, text: &str) -> Result<(), MessengerError> {
        self.edited.push(EditedMessage {
            text: text.to_string(),
            id: message_id,
            keyboard: None,
            format: MessageFormat::Plain,
        });
        Ok(())
    }

    fn edit_message_with_format(
        &mut self,
        message_id: MessageId,
        text: &str,
        format: MessageFormat,
    ) -> Result<(), MessengerError> {
        self.edited.push(EditedMessage {
            text: text.to_string(),
            id: message_id,
            keyboard: None,
            format,
        });
        Ok(())
    }

    fn edit_message_with_keyboard(
        &mut self,
        message_id: MessageId,
        text: &str,
        keyboard: &InlineKeyboard,
    ) -> Result<(), MessengerError> {
        self.edited.push(EditedMessage {
            text: text.to_string(),
            id: message_id,
            keyboard: Some(keyboard.clone()),
            format: MessageFormat::Plain,
        });
        Ok(())
    }

    fn edit_message_with_keyboard_and_format(
        &mut self,
        message_id: MessageId,
        text: &str,
        keyboard: &InlineKeyboard,
        format: MessageFormat,
    ) -> Result<(), MessengerError> {
        self.edited.push(EditedMessage {
            text: text.to_string(),
            id: message_id,
            keyboard: Some(keyboard.clone()),
            format,
        });
        Ok(())
    }

    fn answer_callback_query(
        &mut self,
        callback_query_id: &str,
        _text: Option<&str>,
    ) -> Result<(), MessengerError> {
        self.answered_callbacks.push(callback_query_id.to_string());
        Ok(())
    }
}

impl Default for RecordingMessenger {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// FailingMessenger
// ---------------------------------------------------------------------------

/// A messenger that always returns an HTTP error on `send_message`.
pub struct FailingMessenger;

impl MessageSink for FailingMessenger {
    fn send_message(&mut self, _text: &str) -> Result<MessageId, MessengerError> {
        Err(MessengerError::Http("simulated failure".into()))
    }
}
