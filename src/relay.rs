//! Talking to the relay: HTTP for reading history and sending (the same `/query` and `/events` endpoints the `buzz` CLI and the
//! Buzz app use, signed per request with NIP-98), and a WebSocket for live events (see `live.rs`).

use crate::model::{Channel, Ev, KIND_GROUP_MEMBERS, KIND_GROUP_METADATA, KIND_PROFILE, KIND_WINDOW_BOUNDS, MESSAGE_KINDS};

/// Messages per page of a channel, and the most messages loaded for one thread.
pub const PAGE: u32 = 50;
pub const THREAD_LIMIT: u32 = 200;
/// Most messages counted for one channel's unread badge on startup; a busier channel reads as this many.
pub const UNREAD_LIMIT: u32 = 200;
use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use nostr::nips::nip19::ToBech32;
use nostr::{EventBuilder, JsonUtil, Keys, Kind, Tag};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;

/// `wss://host`, `ws://host:3000/` or `https://host` -> `https://host` / `http://host:3000`.
pub fn http_base(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    if let Some(rest) = u.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = u.strip_prefix("ws://") {
        format!("http://{rest}")
    } else if u.starts_with("http://") || u.starts_with("https://") {
        u.to_owned()
    } else {
        format!("https://{u}")
    }
}

/// The WebSocket URL of the same relay.
pub fn ws_url(url: &str) -> String {
    let b = http_base(url);
    if let Some(rest) = b.strip_prefix("https://") {
        format!("wss://{rest}")
    } else {
        format!("ws://{}", b.trim_start_matches("http://"))
    }
}

pub struct Relay {
    http: reqwest::Client,
    base: String,
    ws: String,
    keys: Keys,
    /// NIP-OA authorisation tag, set when this identity is an agent acting for an owner (`BUZZ_AUTH_TAG`).
    auth_tag: Option<Tag>,
    auth_tag_json: Option<String>,
}

/// One page of a channel's timeline from the relay's channel window.
pub struct Window {
    /// Rows, thread summaries and the aux closure (reactions, edits, deletions), unsorted.
    pub events: Vec<Ev>,
    pub has_more: bool,
    pub next_cursor: Option<(u64, String)>,
}

/// What a `kind:0` profile says about one person.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub about: String,
}

impl Profile {
    /// Read a profile event; `display_name` wins over `name`, as in the Buzz apps.
    fn from_event(e: &Ev) -> Profile {
        let v: Value = serde_json::from_str(&e.content).unwrap_or(Value::Null);
        let field = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_owned();
        let name = ["display_name", "name"].iter().map(|k| field(k)).find(|n| !n.is_empty()).unwrap_or_default();
        Profile { name, about: field("about") }
    }
}

/// The hex pubkey when `who` is one: hex, or an `npub1…`, either bare or as a `nostr:` URI.
pub fn as_pubkey(who: &str) -> Option<String> {
    let w = who.trim();
    let w = w.strip_prefix("nostr:").unwrap_or(w);
    if w.len() == 64 && w.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(w.to_lowercase());
    }
    if w.starts_with("npub1") {
        return nostr::PublicKey::parse(w).ok().map(|p| p.to_hex());
    }
    None
}

/// `npub1…` for a hex pubkey, when it is one.
pub fn npub_of(pubkey: &str) -> Option<String> {
    nostr::PublicKey::from_hex(pubkey).ok().and_then(|p| p.to_bech32().ok())
}

/// What a channel accumulated after a read position, for the badges at startup: messages from other people, and how many of
/// those name you in a `p` tag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Unread {
    pub messages: u32,
    pub mentions: u32,
}

impl Relay {
    pub fn new(relay_url: &str, keys: Keys, auth_tag_json: Option<String>) -> Result<Relay> {
        let auth_tag = match &auth_tag_json {
            Some(j) => {
                let parts: Vec<String> = serde_json::from_str(j).context("BUZZ_AUTH_TAG is not a JSON array of strings")?;
                Some(Tag::parse(parts).map_err(|e| anyhow!("BUZZ_AUTH_TAG: {e}"))?)
            }
            None => None,
        };
        let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).connect_timeout(Duration::from_secs(15)).build()?;
        Ok(Relay { http, base: http_base(relay_url), ws: ws_url(relay_url), keys, auth_tag, auth_tag_json })
    }

    pub fn keys(&self) -> &Keys {
        &self.keys
    }

    pub fn ws_url(&self) -> &str {
        &self.ws
    }

    pub fn auth_tag(&self) -> Option<&Tag> {
        self.auth_tag.as_ref()
    }

    pub fn me(&self) -> String {
        self.keys.public_key().to_hex()
    }

    /// `Authorization` header value: a NIP-98 event for this request (fresh nonce every time, so a retry is not a replay).
    fn nip98(&self, method: &str, url: &str, body: Option<&[u8]>) -> Result<String> {
        let mut tags = vec![
            Tag::parse(["u", url]).map_err(|e| anyhow!("{e}"))?,
            Tag::parse(["method", method]).map_err(|e| anyhow!("{e}"))?,
            Tag::parse(["nonce", &uuid::Uuid::new_v4().to_string()]).map_err(|e| anyhow!("{e}"))?,
        ];
        if let Some(b) = body {
            tags.push(Tag::parse(["payload", &hex::encode(Sha256::digest(b))]).map_err(|e| anyhow!("{e}"))?);
        }
        let event = EventBuilder::new(Kind::Custom(27235), "").tags(tags).sign_with_keys(&self.keys).map_err(|e| anyhow!("NIP-98 signing failed: {e}"))?;
        Ok(format!("Nostr {}", B64.encode(event.as_json().as_bytes())))
    }

    /// POST JSON to the relay with up to three attempts for transient failures.
    async fn post(&self, path: &str, body: Vec<u8>) -> Result<String> {
        let url = format!("{}{}", self.base, path);
        let mut last = anyhow!("no attempt made");
        for attempt in 0..3u64 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500 * attempt * attempt + 300)).await;
            }
            let mut req = self
                .http
                .post(&url)
                .header("Authorization", self.nip98("POST", &url, Some(&body))?)
                .header("Content-Type", "application/json")
                .body(body.clone());
            if let Some(j) = &self.auth_tag_json {
                req = req.header("x-auth-tag", j);
            }
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    if status.is_success() {
                        return Ok(text);
                    }
                    last = anyhow!("relay returned {status}: {}", relay_message(&text));
                    if !matches!(status.as_u16(), 429 | 502 | 503 | 504) {
                        break;
                    }
                }
                Err(e) => last = anyhow!("could not reach the relay: {e}"),
            }
        }
        Err(last)
    }

    /// One-shot Nostr query (the relay ORs the filters).
    pub async fn query(&self, filters: &[Value]) -> Result<Vec<Ev>> {
        let text = self.post("/query", serde_json::to_vec(filters)?).await?;
        let v: Value = serde_json::from_str(&text).context("the relay's answer was not JSON")?;
        Ok(v.as_array().map(|a| a.iter().filter_map(Ev::from_value).collect()).unwrap_or_default())
    }

    /// Sign an event (adding the NIP-OA tag when this is an agent) and hand it to the relay. Returns the event id.
    pub async fn submit(&self, builder: EventBuilder) -> Result<String> {
        let builder = match &self.auth_tag {
            Some(t) => builder.tags([t.clone()]),
            None => builder,
        };
        let event = builder.sign_with_keys(&self.keys).map_err(|e| anyhow!("signing failed: {e}"))?;
        let text = self.post("/events", serde_json::to_vec(&event)?).await?;
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            if v.get("accepted") == Some(&Value::Bool(false)) {
                bail!("the relay refused it: {}", v.get("message").and_then(Value::as_str).unwrap_or("no reason given"));
            }
        }
        Ok(event.id.to_hex())
    }

    /// NIP-11 information document (name, software, version), without authentication.
    pub async fn info(&self) -> Result<Value> {
        let resp = self.http.get(&self.base).header("Accept", "application/nostr+json").send().await?;
        Ok(resp.json().await?)
    }

    pub async fn channels(&self) -> Result<Vec<Channel>> {
        let evs = self.query(&[json!({"kinds": [KIND_GROUP_METADATA], "limit": 500})]).await?;
        Ok(evs.iter().filter_map(Channel::from_event).collect())
    }

    /// `(pubkey, role)` of a channel's members.
    pub async fn members(&self, channel: &str) -> Result<Vec<(String, String)>> {
        let evs = self.query(&[json!({"kinds": [KIND_GROUP_MEMBERS], "#d": [channel], "limit": 1})]).await?;
        Ok(evs
            .first()
            .map(|e| {
                e.tags
                    .iter()
                    .filter(|t| t.first().map(String::as_str) == Some("p"))
                    .filter_map(|t| {
                        let role = t.get(3).filter(|r| !r.is_empty()).cloned().unwrap_or_else(|| "member".into());
                        t.get(1).map(|p| (p.clone(), role))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// The newest `kind:0` profile per pubkey: the name to show and what the person says about themselves.
    pub async fn profiles(&self, pubkeys: &[String]) -> Result<HashMap<String, Profile>> {
        let mut out = HashMap::new();
        for chunk in pubkeys.chunks(100) {
            let evs = self.query(&[json!({"kinds": [KIND_PROFILE], "authors": chunk, "limit": 500})]).await?;
            let mut newest: HashMap<&str, &Ev> = HashMap::new();
            for e in &evs {
                if newest.get(e.pubkey.as_str()).map(|n| n.created_at <= e.created_at).unwrap_or(true) {
                    newest.insert(&e.pubkey, e);
                }
            }
            for (pk, e) in newest {
                out.insert(pk.to_owned(), Profile::from_event(e));
            }
        }
        Ok(out)
    }

    /// Display names for pubkeys (`display_name`, else `name`); someone who never wrote a name is left out.
    pub async fn names(&self, pubkeys: &[String]) -> Result<HashMap<String, String>> {
        Ok(self.profiles(pubkeys).await?.into_iter().filter(|(_, p)| !p.name.is_empty()).map(|(pk, p)| (pk, p.name)).collect())
    }

    /// One person's profile; someone who never wrote one reads as an empty profile, which is not an error.
    pub async fn profile(&self, pubkey: &str) -> Result<Profile> {
        Ok(self.profiles(&[pubkey.to_owned()]).await?.remove(pubkey).unwrap_or_default())
    }

    /// Full-text search over the messages you can read, most relevant first (NIP-50).
    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<Ev>> {
        self.query(&[json!({"kinds": [9, 40002, 45001, 45003], "search": query, "limit": limit})]).await
    }

    /// What a channel accumulated after `since`: messages someone else wrote, and how many of those mention you. Counts at
    /// most `UNREAD_LIMIT`, so a very busy channel reads as that many rather than making one huge query.
    pub async fn unread(&self, channel: &str, since: u64) -> Result<Unread> {
        let me = self.me();
        let evs = self.query(&[json!({"kinds": MESSAGE_KINDS, "#h": [channel], "since": since, "limit": UNREAD_LIMIT})]).await?;
        let mut out = Unread::default();
        for e in evs.iter().filter(|e| MESSAGE_KINDS.contains(&e.kind) && e.pubkey != me) {
            out.messages += 1;
            if e.mentions(&me) {
                out.mentions += 1;
            }
        }
        Ok(out)
    }

    /// A page of the channel timeline, the way the Buzz app asks for it: top-level messages only, with thread summaries and the
    /// reactions/edits/deletions of those rows (NIP-CW channel window).
    pub async fn window(&self, channel: &str, cursor: Option<(u64, &str)>, limit: u32) -> Result<Window> {
        let mut filter = json!({
            "kinds": MESSAGE_KINDS, "#h": [channel], "limit": limit,
            "top_level": true, "include_summaries": true, "include_aux": true,
        });
        let suffix = match cursor {
            Some((at, id)) => {
                filter["until"] = json!(at);
                filter["before_id"] = json!(id);
                format!("{at}:{id}")
            }
            None => "head".to_owned(),
        };
        let all = self.query(&[filter]).await?;
        let (bounds, events): (Vec<Ev>, Vec<Ev>) = all.into_iter().partition(|e| e.kind == KIND_WINDOW_BOUNDS);
        let rows = events.iter().filter(|e| MESSAGE_KINDS.contains(&e.kind)).count();
        // The bounds say whether there is more; they must answer *this* request (the `d` tag ends with the cursor we sent).
        let own = bounds.iter().find(|b| b.tag("d").map(|d| d.ends_with(&suffix)).unwrap_or(false));
        let (has_more, next_cursor) = match own.and_then(|b| serde_json::from_str::<Value>(&b.content).ok()) {
            Some(v) => (
                v.get("has_more").and_then(Value::as_bool).unwrap_or(false),
                v.get("next_cursor").and_then(|c| Some((c.get("created_at")?.as_u64()?, c.get("id")?.as_str()?.to_owned()))),
            ),
            None => {
                // an extension-unaware relay: fall back to the newest row's position
                let oldest = events.iter().filter(|e| MESSAGE_KINDS.contains(&e.kind)).min_by_key(|e| (e.created_at, e.id.clone()));
                (rows >= limit as usize, oldest.map(|e| (e.created_at, e.id.clone())))
            }
        };
        Ok(Window { events, has_more, next_cursor })
    }

    /// A thread: its root and every reply, plus the reactions/edits/deletions of those messages.
    pub async fn thread(&self, channel: &str, root: &str, limit: u32) -> Result<Vec<Ev>> {
        let mut evs = self
            .query(&[
                json!({"kinds": [9, 40002, 40003, 40008, 45003], "#h": [channel], "#e": [root], "limit": limit}),
                json!({"ids": [root], "#h": [channel], "limit": 1}),
            ])
            .await?;
        let ids: Vec<&str> = evs.iter().filter(|e| MESSAGE_KINDS.contains(&e.kind)).map(|e| e.id.as_str()).take(200).collect();
        if !ids.is_empty() {
            let aux = self.query(&[json!({"kinds": [7, 5, 9005], "#e": ids, "limit": 500})]).await?;
            evs.extend(aux);
        }
        Ok(evs)
    }
}

/// A chat message for a channel; `root` makes it a reply in that thread (a direct reply to the root, which is how the Buzz app
/// threads), `mentions` are pubkeys to notify.
pub fn message_builder(channel: &str, text: &str, root: Option<&str>, mentions: &[String]) -> Result<EventBuilder> {
    let channel_id = uuid::Uuid::parse_str(channel).context("not a channel id")?;
    let thread = match root {
        Some(r) => {
            let id = nostr::EventId::from_hex(r).context("not a message id")?;
            Some(buzz_sdk::ThreadRef { root_event_id: id, parent_event_id: id })
        }
        None => None,
    };
    let mention_refs: Vec<&str> = mentions.iter().map(String::as_str).collect();
    buzz_sdk::build_message(channel_id, text, thread.as_ref(), &mention_refs, false, &[], &[]).map_err(|e| anyhow!("{e}"))
}

/// A typing indicator (`kind:20002`, ephemeral: delivered to subscribers, never stored). In a thread it names the thread root the way
/// the Buzz agents do, so that other clients can show it under the right thread.
pub fn typing_builder(channel: &str, thread_root: Option<&str>) -> Result<EventBuilder> {
    let mut tags = vec![Tag::parse(["h", channel]).map_err(|e| anyhow!("{e}"))?];
    if let Some(root) = thread_root {
        tags.push(Tag::parse(["e", root, "", "reply"]).map_err(|e| anyhow!("{e}"))?);
    }
    Ok(EventBuilder::new(Kind::Custom(20002), "").tags(tags))
}

/// Replace the text of one of your messages (`kind:40003`).
pub fn edit_builder(channel: &str, target: &str, text: &str) -> Result<EventBuilder> {
    let channel_id = uuid::Uuid::parse_str(channel).context("not a channel id")?;
    let target = nostr::EventId::from_hex(target).context("not a message id")?;
    buzz_sdk::build_edit(channel_id, target, text).map_err(|e| anyhow!("{e}"))
}

/// Delete a message (`kind:9005`, what `buzz messages delete` sends).
pub fn delete_builder(channel: &str, target: &str) -> Result<EventBuilder> {
    let channel_id = uuid::Uuid::parse_str(channel).context("not a channel id")?;
    let target = nostr::EventId::from_hex(target).context("not a message id")?;
    buzz_sdk::build_delete_message(channel_id, target).map_err(|e| anyhow!("{e}"))
}

/// A new stream channel (`kind:9007`, what `buzz channels create` sends). Returns the builder and the new channel's id.
pub fn create_channel_builder(name: &str, private: bool, about: Option<&str>) -> Result<(EventBuilder, String)> {
    let id = uuid::Uuid::new_v4();
    let visibility = if private { buzz_sdk::Visibility::Private } else { buzz_sdk::Visibility::Open };
    let builder = buzz_sdk::build_create_channel(id, name, Some(visibility), Some(buzz_sdk::ChannelKind::Stream), about, None, false).map_err(|e| anyhow!("{e}"))?;
    Ok((builder, id.to_string()))
}

/// Delete a whole channel (`kind:9008`); used by the write probe to clean up its test channel.
pub fn delete_channel_builder(channel: &str) -> Result<EventBuilder> {
    let channel_id = uuid::Uuid::parse_str(channel).context("not a channel id")?;
    buzz_sdk::build_delete_channel(channel_id).map_err(|e| anyhow!("{e}"))
}

/// Open a direct message with `peer` (`kind:41010`, what `/dm` sends). The relay creates the conversation if it does not exist
/// and reuses it if it does, so this is safe to send for someone you already have a DM with; it answers with no channel id, so
/// the client finds the conversation in the channel list afterwards.
pub fn dm_open_builder(peer: &str) -> Result<EventBuilder> {
    buzz_sdk::build_dm_open(&[peer]).map_err(|e| anyhow!("{e}"))
}

/// The human-readable part of a relay error body.
fn relay_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("message").or_else(|| v.get("error")).and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| body.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_normalised_both_ways() {
        assert_eq!(http_base("wss://chat.example.com"), "https://chat.example.com");
        assert_eq!(http_base("ws://192.0.2.10:3000/"), "http://192.0.2.10:3000");
        assert_eq!(http_base("https://x.y/"), "https://x.y");
        assert_eq!(http_base("buzz.example"), "https://buzz.example");
        assert_eq!(ws_url("https://chat.example.com"), "wss://chat.example.com");
        assert_eq!(ws_url("http://192.0.2.10:3000"), "ws://192.0.2.10:3000");
        assert_eq!(ws_url("wss://a.b"), "wss://a.b");
    }

    #[test]
    fn relay_errors_are_readable() {
        assert_eq!(relay_message(r#"{"message":"rate limited"}"#), "rate limited");
        assert_eq!(relay_message(r#"{"error":"nope"}"#), "nope");
        assert_eq!(relay_message("plain text"), "plain text");
    }

    #[test]
    fn nip98_header_is_a_signed_event_for_this_request() {
        let relay = Relay::new("https://r.example", Keys::generate(), None).unwrap();
        let header = relay.nip98("POST", "https://r.example/query", Some(b"[]")).unwrap();
        let json = String::from_utf8(B64.decode(header.strip_prefix("Nostr ").unwrap()).unwrap()).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["kind"], 27235);
        let tags = v["tags"].to_string();
        assert!(tags.contains("https://r.example/query") && tags.contains("POST") && tags.contains(&hex::encode(Sha256::digest(b"[]"))));
        assert_ne!(header, relay.nip98("POST", "https://r.example/query", Some(b"[]")).unwrap(), "each request gets a fresh nonce");
    }

    #[test]
    fn messages_are_built_with_channel_thread_and_mention_tags() {
        let keys = Keys::generate();
        let channel = "11111111-1111-4111-8111-111111111111";
        let root = "ab".repeat(32);
        let friend = "cd".repeat(32);
        let plain = message_builder(channel, "hello", None, &[]).unwrap().sign_with_keys(&keys).unwrap();
        assert_eq!(plain.kind.as_u16(), 9);
        assert_eq!(plain.content, "hello");
        assert!(plain.tags.iter().any(|t| t.as_slice() == ["h", channel]));
        assert!(!plain.tags.iter().any(|t| t.as_slice().first().map(String::as_str) == Some("e")));
        let reply = message_builder(channel, "hi @friend", Some(&root), &[friend.clone()]).unwrap().sign_with_keys(&keys).unwrap();
        assert!(reply.tags.iter().any(|t| t.as_slice() == ["e", root.as_str(), "", "reply"]), "a direct reply to the thread root: {:?}", reply.tags);
        assert!(reply.tags.iter().any(|t| t.as_slice() == ["p", friend.as_str()]));
        assert!(message_builder("not-a-uuid", "x", None, &[]).is_err());
        assert!(message_builder(channel, "x", Some("zz"), &[]).is_err());
        let typing = typing_builder(channel, None).unwrap().sign_with_keys(&keys).unwrap();
        assert_eq!(typing.kind.as_u16(), 20002);
        assert!(typing.tags.iter().any(|t| t.as_slice() == ["h", channel]));
        let in_thread = typing_builder(channel, Some(&root)).unwrap().sign_with_keys(&keys).unwrap();
        assert!(in_thread.tags.iter().any(|t| t.as_slice() == ["e", root.as_str(), "", "reply"]), "a typing indicator in a thread names the thread");
    }

    #[test]
    fn edits_deletes_and_new_channels_are_built_like_the_cli_builds_them() {
        let keys = Keys::generate();
        let channel = "11111111-1111-4111-8111-111111111111";
        let target = "ab".repeat(32);
        let edit = edit_builder(channel, &target, "new text").unwrap().sign_with_keys(&keys).unwrap();
        assert_eq!((edit.kind.as_u16(), edit.content.as_str()), (40003, "new text"));
        assert!(edit.tags.iter().any(|t| t.as_slice() == ["e", target.as_str()]) && edit.tags.iter().any(|t| t.as_slice() == ["h", channel]));
        let del = delete_builder(channel, &target).unwrap().sign_with_keys(&keys).unwrap();
        assert_eq!(del.kind.as_u16(), 9005);
        assert!(del.tags.iter().any(|t| t.as_slice() == ["e", target.as_str()]));
        let (b, id) = create_channel_builder("Team Room", true, Some("what it is for")).unwrap();
        let ev = b.sign_with_keys(&keys).unwrap();
        assert_eq!(ev.kind.as_u16(), 9007);
        assert!(ev.tags.iter().any(|t| t.as_slice() == ["h", id.as_str()]));
        assert!(ev.tags.iter().any(|t| t.as_slice() == ["visibility", "private"]) && ev.tags.iter().any(|t| t.as_slice() == ["about", "what it is for"]));
        assert!(ev.tags.iter().any(|t| t.as_slice().first().map(String::as_str) == Some("name")));
        assert!(create_channel_builder("   ", false, None).is_err(), "a channel needs a name");
        assert!(edit_builder("nope", &target, "x").is_err() && delete_builder(channel, "zz").is_err());
    }

    #[test]
    fn a_profile_gives_the_name_to_show_and_the_about_line() {
        let profile = |at: u64, content: &str| Ev { id: format!("p{at}"), pubkey: "alice".into(), created_at: at, kind: KIND_PROFILE, content: content.into(), tags: Vec::new() };
        assert_eq!(
            Profile::from_event(&profile(5, r#"{"name":"alice","display_name":"Alice Smith","about":"  I sail  "}"#)),
            Profile { name: "Alice Smith".into(), about: "I sail".into() },
            "display_name wins, and the fields are trimmed"
        );
        assert_eq!(Profile::from_event(&profile(6, r#"{"name":"alice"}"#)), Profile { name: "alice".into(), about: String::new() }, "no about line");
        assert_eq!(Profile::from_event(&profile(7, "not json")), Profile::default(), "a broken profile is an empty one, not an error");
    }

    #[test]
    fn a_person_is_recognised_by_hex_npub_or_neither() {
        let keys = Keys::generate();
        let hex = keys.public_key().to_hex();
        let npub = keys.public_key().to_bech32().unwrap();
        assert_eq!(as_pubkey(&hex).as_deref(), Some(hex.as_str()));
        assert_eq!(as_pubkey(&hex.to_uppercase()).as_deref(), Some(hex.as_str()));
        assert_eq!(as_pubkey(&npub).as_deref(), Some(hex.as_str()));
        assert_eq!(as_pubkey(&format!("nostr:{npub}")).as_deref(), Some(hex.as_str()));
        assert_eq!(npub_of(&hex).as_deref(), Some(npub.as_str()));
        assert_eq!(as_pubkey("alice"), None, "a display name is not a key");
        assert_eq!(as_pubkey("npub1nope"), None);
        assert_eq!(npub_of("alice"), None);
    }

    #[test]
    fn a_dm_open_names_the_other_person() {
        let keys = Keys::generate();
        let peer = "cd".repeat(32);
        let ev = dm_open_builder(&peer).unwrap().sign_with_keys(&keys).unwrap();
        assert_eq!(ev.kind.as_u16(), 41010);
        assert_eq!(ev.content, "");
        assert!(ev.tags.iter().any(|t| t.as_slice() == ["p", peer.as_str()]), "{:?}", ev.tags);
        assert!(dm_open_builder("alice").is_err(), "a pubkey is required");
    }

    #[test]
    fn auth_tag_must_be_a_json_array() {
        assert!(Relay::new("https://r", Keys::generate(), Some("not json".into())).is_err());
        let keys = Keys::generate();
        let tag = json!(["auth", keys.public_key().to_hex(), "", "00".repeat(64)]).to_string();
        assert!(Relay::new("https://r", Keys::generate(), Some(tag)).unwrap().auth_tag().is_some());
    }
}
