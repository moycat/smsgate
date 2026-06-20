//! Dedicated Telegram send worker.
//!
//! The polling thread owns its own Telegram client for `getUpdates`; this worker
//! owns the outbound client for notifications and command replies.

use super::{http::TelegramHttpClient, TelegramMessenger};
use crate::im::{MessageId, MessageSink, MessengerError};
use crate::modem::ModemPort;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{sync_channel, SyncSender},
    Arc, Mutex,
};

const OUTBOUND_QUEUE_DEPTH: usize = 8;

enum Request {
    Send {
        text: String,
        reply: SyncSender<Result<MessageId, MessengerError>>,
    },
    RegisterCommands {
        commands: Vec<(String, String)>,
        reply: SyncSender<Result<(), MessengerError>>,
    },
}

/// Synchronous `MessageSink` facade backed by a dedicated Telegram worker task.
pub struct TelegramSendWorker {
    tx: SyncSender<Request>,
}

impl TelegramSendWorker {
    pub fn spawn(
        modem: Arc<Mutex<dyn ModemPort + Send>>,
        token: String,
        chat_id: i64,
        transport_cellular: Arc<AtomicBool>,
    ) -> Self {
        let (tx, rx) = sync_channel::<Request>(OUTBOUND_QUEUE_DEPTH);
        std::thread::Builder::new()
            .name("tg-send".into())
            .stack_size(16 * 1024)
            .spawn(move || {
                let mut current_cellular = transport_cellular.load(Ordering::SeqCst);
                let mut messenger =
                    build_worker_messenger(current_cellular, modem.clone(), token.clone(), chat_id)
                        .ok();

                while let Ok(req) = rx.recv() {
                    let desired_cellular = transport_cellular.load(Ordering::SeqCst);
                    if desired_cellular != current_cellular || messenger.is_none() {
                        messenger = build_worker_messenger(
                            desired_cellular,
                            modem.clone(),
                            token.clone(),
                            chat_id,
                        )
                        .ok();
                        current_cellular = desired_cellular;
                    }

                    match req {
                        Request::Send { text, reply } => {
                            let result = match messenger.as_mut() {
                                Some(m) => m.send_message(&text),
                                None => Err(MessengerError::Disconnected),
                            };
                            let _ = reply.send(result);
                        }
                        Request::RegisterCommands { commands, reply } => {
                            let result = match messenger.as_mut() {
                                Some(m) => {
                                    let refs: Vec<(&str, &str)> = commands
                                        .iter()
                                        .map(|(name, desc)| (name.as_str(), desc.as_str()))
                                        .collect();
                                    m.register_commands(&refs)
                                }
                                None => Err(MessengerError::Disconnected),
                            };
                            let _ = reply.send(result);
                        }
                    }
                }
            })
            .expect("failed to spawn tg-send thread");

        TelegramSendWorker { tx }
    }

    pub fn register_commands(&mut self, commands: &[(&str, &str)]) -> Result<(), MessengerError> {
        let (reply, rx) = sync_channel(1);
        let commands = commands
            .iter()
            .map(|(name, desc)| ((*name).to_string(), (*desc).to_string()))
            .collect();
        self.tx
            .send(Request::RegisterCommands { commands, reply })
            .map_err(|_| MessengerError::Disconnected)?;
        rx.recv().map_err(|_| MessengerError::Disconnected)?
    }
}

impl MessageSink for TelegramSendWorker {
    fn send_message(&mut self, text: &str) -> Result<MessageId, MessengerError> {
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(Request::Send {
                text: text.to_string(),
                reply,
            })
            .map_err(|_| MessengerError::Disconnected)?;
        rx.recv().map_err(|_| MessengerError::Disconnected)?
    }
}

fn build_worker_messenger(
    use_cellular: bool,
    modem: Arc<Mutex<dyn ModemPort + Send>>,
    token: String,
    chat_id: i64,
) -> anyhow::Result<TelegramMessenger> {
    if use_cellular {
        Ok(TelegramMessenger::new_modem(modem, token, chat_id))
    } else {
        Ok(TelegramMessenger::new_wifi(
            TelegramHttpClient::new(None)?,
            token,
            chat_id,
        ))
    }
}
