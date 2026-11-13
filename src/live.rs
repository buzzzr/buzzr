//! The live connection: one authenticated WebSocket that pushes new messages, edits, deletions, reactions, thread summaries and
//! typing indicators for every channel, reconnecting by itself.

use crate::model::{Ev, KIND_DELETE_MESSAGE, KIND_DELETION, KIND_EDIT, KIND_REACTION, KIND_THREAD_SUMMARY, KIND_TYPING, MESSAGE_KINDS};
use crate::relay::Relay;
use anyhow::{anyhow, bail, Result};
use buzz_ws_client::{build_auth_event, parse_relay_message, RelayMessage};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug)]
pub enum Live {
    /// Connected, authenticated and subscribed. Whatever happened while we were away is missing: refetch.
    Up,
    /// The connection is gone (or could not be made); a retry follows.
    Down(String),
    /// The relay said something in a NOTICE frame. The connection is fine, so this is not `Down`.
    Notice(String),
    Event(Ev),
}

pub struct LiveHandle {
    pub rx: mpsc::UnboundedReceiver<Live>,
    /// Send the ids of the channels to follow; the subscription is replaced when they change.
    pub channels: watch::Sender<Vec<String>>,
}

pub fn spawn(relay: Arc<Relay>, channels: Vec<String>) -> LiveHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let (chan_tx, chan_rx) = watch::channel(channels);
    tokio::spawn(async move {
        let mut wait = 1u64;
        loop {
            let started = Instant::now();
            let why = match session(&relay, &tx, chan_rx.clone()).await {
                Ok(()) => "connection closed".to_owned(),
                Err(e) => e.to_string(),
            };
            if tx.send(Live::Down(why)).is_err() {
                return; // the app is gone
            }
            if started.elapsed() > Duration::from_secs(30) {
                wait = 1; // it was a healthy connection that dropped: retry quickly
            }
            // Jitter, so a relay that restarts is not met by every client in the same second.
            tokio::time::sleep(Duration::from_secs(wait).mul_f64(1.0 + 0.25 * rand::random::<f64>())).await;
            wait = (wait * 2).min(20);
        }
    });
    LiveHandle { rx, channels: chan_tx }
}

fn req(channels: &[String], since: u64) -> Value {
    let mut kinds: Vec<u64> = MESSAGE_KINDS.to_vec();
    kinds.extend([KIND_EDIT, KIND_DELETION, KIND_DELETE_MESSAGE, KIND_REACTION, KIND_THREAD_SUMMARY, KIND_TYPING]);
    json!(["REQ", "live", {"kinds": kinds, "#h": channels, "since": since}])
}


async fn session(relay: &Relay, tx: &mpsc::UnboundedSender<Live>, mut chan_rx: watch::Receiver<Vec<String>>) -> Result<()> {
    let (ws, _) = tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async(relay.ws_url()))
        .await
        .map_err(|_| anyhow!("timed out connecting"))??;
    let (mut sink, mut stream) = ws.split();

    // NIP-42: the relay sends a challenge, we answer with a signed event
    let challenge = loop {
        let frame = tokio::time::timeout(Duration::from_secs(15), stream.next()).await.map_err(|_| anyhow!("no authentication challenge"))?;
        match frame {
            Some(Ok(Message::Text(t))) => {
                if let Ok(RelayMessage::Auth { challenge }) = parse_relay_message(t.as_str()) {
                    break challenge;
                }
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(e.into()),
            None => bail!("closed before authenticating"),
        }
    };
    let auth = build_auth_event(&challenge, relay.ws_url(), relay.keys(), relay.auth_tag()).map_err(|e| anyhow!("{e}"))?;
    let auth_id = auth.id.to_hex();
    sink.send(Message::Text(json!(["AUTH", auth]).to_string().into())).await?;
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(15), stream.next()).await.map_err(|_| anyhow!("no answer to authentication"))?;
        match frame {
            Some(Ok(Message::Text(t))) => {
                if let Ok(RelayMessage::Ok(ok)) = parse_relay_message(t.as_str()) {
                    if ok.event_id == auth_id {
                        if ok.accepted {
                            break;
                        }
                        bail!("authentication refused: {}", ok.message);
                    }
                }
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(e.into()),
            None => bail!("closed during authentication"),
        }
    }

    let since = crate::clock::now_secs().saturating_sub(2);
    let channels = chan_rx.borrow_and_update().clone();
    if !channels.is_empty() {
        sink.send(Message::Text(req(&channels, since).to_string().into())).await?;
    }
    let _ = tx.send(Live::Up);

    let mut ping = tokio::time::interval(Duration::from_secs(25));
    ping.tick().await;
    loop {
        tokio::select! {
            frame = stream.next() => match frame {
                Some(Ok(Message::Text(t))) => match parse_relay_message(t.as_str()) {
                    Ok(RelayMessage::Event { event, .. }) => {
                        if let Some(ev) = serde_json::to_value(&*event).ok().as_ref().and_then(Ev::from_value) {
                            if tx.send(Live::Event(ev)).is_err() {
                                return Ok(());
                            }
                        }
                    }
                    Ok(RelayMessage::Closed { message, .. }) => bail!("subscription closed by the relay: {message}"),
                    Ok(RelayMessage::Notice { message }) => { let _ = tx.send(Live::Notice(message)); }
                    _ => {}
                },
                Some(Ok(Message::Ping(p))) => sink.send(Message::Pong(p)).await?,
                Some(Ok(Message::Close(_))) | None => return Ok(()),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
            },
            changed = chan_rx.changed() => {
                if changed.is_err() { return Ok(()); }
                let channels = chan_rx.borrow_and_update().clone();
                sink.send(Message::Text(json!(["CLOSE", "live"]).to_string().into())).await?;
                if !channels.is_empty() {
                    sink.send(Message::Text(req(&channels, crate::clock::now_secs().saturating_sub(2)).to_string().into())).await?;
                }
            }
            _ = ping.tick() => sink.send(Message::Ping(Vec::new().into())).await?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_subscription_asks_for_everything_we_display_and_only_for_new_events() {
        let r = req(&["c1".into(), "c2".into()], 1000);
        assert_eq!(r[0], "REQ");
        assert_eq!(r[2]["#h"], json!(["c1", "c2"]));
        assert_eq!(r[2]["since"], 1000);
        let kinds: Vec<u64> = serde_json::from_value(r[2]["kinds"].clone()).unwrap();
        for k in [9, 40003, 5, 7, 39005, 20002] {
            assert!(kinds.contains(&k), "kind {k} missing");
        }
    }
}
