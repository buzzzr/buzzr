//! `buzzr --selftest`: a read-only check of everything that talks to the relay, for a machine without a terminal (or for a smoke
//! test from a container that already has an identity). It sends nothing.

use crate::live::{self, Live};
use crate::model::{Ev, Store, KIND_TYPING};
use crate::relay::Relay;
use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

pub async fn run(relay: Arc<Relay>, listen_secs: u64, probe_typing: Option<String>, probe_search: Option<String>) -> Result<()> {
    let info = relay.info().await?;
    println!("relay: {} {} ({})", info["name"].as_str().unwrap_or("?"), info["version"].as_str().unwrap_or("?"), info["software"].as_str().unwrap_or("?"));
    println!("identity: {}", &relay.me()[..8]);

    let channels = relay.channels().await?;
    println!("channels: {} ({})", channels.len(), channels.iter().take(8).map(|c| c.name.as_str()).collect::<Vec<_>>().join(", "));
    if channels.is_empty() {
        bail!("no channels visible");
    }

    // the busiest-looking channel: the first one that has messages
    let mut chosen = None;
    for c in &channels {
        let w = relay.window(&c.id, None, 30).await?;
        if w.events.iter().any(|e| e.kind == 9) {
            chosen = Some((c, w));
            break;
        }
    }
    let Some((chan, w)) = chosen else { bail!("no channel has any message") };
    let mut store = Store::default();
    for e in &w.events {
        store.apply(e);
    }
    let timeline = store.timeline();
    let with_replies = timeline.iter().filter(|m| store.replies_of(m).0 > 0).count();
    println!(
        "#{}: window of {} events -> {} timeline rows, {} with replies, {} summaries, has_more={}, next_cursor={}",
        chan.name,
        w.events.len(),
        timeline.len(),
        with_replies,
        store.summaries.len(),
        w.has_more,
        w.next_cursor.is_some()
    );
    if timeline.iter().any(|m| m.parent().is_some() && store.msgs.contains_key(m.parent().unwrap_or(""))) {
        bail!("a reply to a loaded message was returned as a top-level row");
    }
    if let Some(m) = timeline.first() {
        let (text, _) = store.content_of(m);
        println!("oldest row: {:?}", text.chars().take(60).collect::<String>());
    }

    if let (true, Some((at, id))) = (w.has_more, w.next_cursor.clone()) {
        let older = relay.window(&chan.id, Some((at, &id)), 30).await?;
        let rows = older.events.iter().filter(|e| e.kind == 9).count();
        println!("older page: {} rows, has_more={}", rows, older.has_more);
        if older.events.iter().any(|e| w.events.iter().any(|f| f.id == e.id && e.kind == 9)) {
            bail!("the older page repeated rows of the first page");
        }
    }

    if let Some(root) = timeline.iter().find(|m| store.replies_of(m).0 > 0) {
        let thread = relay.thread(&chan.id, &root.id, 100).await?;
        let mut t = Store::default();
        for e in &thread {
            t.apply(e);
        }
        println!("thread of {}: {} messages loaded (relay says {} replies)", &root.id[..8], t.thread(&root.id).len(), store.replies_of(root).0);
    }

    let members = relay.members(&chan.id).await?;
    let pubkeys: Vec<String> = members.iter().map(|(p, _)| p.clone()).collect();
    let names = relay.names(&pubkeys).await?;
    println!("members: {} ({} named), roles: {:?}", members.len(), names.len(), members.iter().map(|(_, r)| r.as_str()).collect::<std::collections::BTreeSet<_>>());

    if let Some(q) = probe_search {
        let hits = relay.search(&q, 10).await?;
        println!("search {q:?}: {} hits", hits.len());
        for h in hits.iter().take(3) {
            let channel = channels.iter().find(|c| Some(c.id.as_str()) == h.channel()).map(|c| c.name.as_str()).unwrap_or("?");
            println!("  #{channel}: {:?}", h.content.chars().take(60).collect::<String>());
        }
        if hits.iter().any(|h| h.channel().is_none()) {
            bail!("a search hit without a channel");
        }
    }
    println!("listening for live events for {listen_secs}s ...");
    let mut handle = live::spawn(relay.clone(), channels.iter().map(|c| c.id.clone()).collect());
    // Optionally prove that typing indicators reach a subscriber: publish one (an ephemeral event that is never stored; people in
    // that channel see "<this identity> is typing" for a few seconds) over a second connection and expect to receive it.
    let mut expect_typing = None;
    if let Some(name) = probe_typing {
        let Some(chan) = channels.iter().find(|c| c.name == name) else { bail!("no channel called {name}") };
        let (relay, id) = (relay.clone(), chan.id.clone());
        expect_typing = Some(relay.me());
        println!("probe: publishing one typing indicator in #{name} in 2 s");
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            // through Relay::submit: the same signed HTTP request a chat message uses
            match crate::relay::typing_builder(&id, None) {
                Ok(b) => match relay.submit(b).await {
                    Ok(id) => println!("probe: submitted {}", &id[..8]),
                    Err(e) => println!("probe: submit failed: {e}"),
                },
                Err(e) => println!("probe: {e}"),
            }
        });
    }
    let mut got_typing = false;
    let mut kinds: BTreeMap<u64, usize> = BTreeMap::new();
    let mut up = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(listen_secs);
    loop {
        match tokio::time::timeout_at(deadline, handle.rx.recv()).await {
            Err(_) => break,
            Ok(None) => bail!("the live task ended"),
            Ok(Some(Live::Up)) => {
                up = true;
                println!("live: connected and subscribed");
            }
            Ok(Some(Live::Down(why))) => println!("live: down ({why})"),
            Ok(Some(Live::Notice(message))) => println!("live: relay notice ({message})"),
            Ok(Some(Live::Event(e))) => {
                *kinds.entry(e.kind).or_default() += 1;
                if e.kind == KIND_TYPING {
                    println!("live: typing indicator from {} in {}", &e.pubkey[..8], e.channel().unwrap_or("?"));
                    got_typing |= expect_typing.as_deref() == Some(e.pubkey.as_str());
                }
                describe(&e);
            }
        }
    }
    println!("live events by kind: {kinds:?}");
    if !up {
        bail!("never got a live connection");
    }
    if expect_typing.is_some() && !got_typing {
        bail!("the typing indicator we published did not come back through the subscription");
    }
    println!("selftest ok");
    Ok(())
}

fn describe(e: &Ev) {
    if e.kind == 9 {
        println!("live: message from {} in {}: {:?}", &e.pubkey[..8], e.channel().unwrap_or("?"), e.content.chars().take(40).collect::<String>());
    }
}

/// `--probe-write`: the one check that writes. It creates a PRIVATE throwaway channel (only this identity can see it), posts a
/// message in it, edits it, deletes it, and deletes the channel, checking after each step that the relay shows the result. Run it with
/// an identity you are happy to see doing that.
pub async fn write_probe(relay: Arc<Relay>) -> Result<()> {
    use crate::relay::{create_channel_builder, delete_channel_builder};
    let name = format!("buzzr-selftest-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let (builder, channel) = create_channel_builder(&name, true, Some("temporary: created and deleted by buzzr --probe-write"))?;
    relay.submit(builder).await?;
    println!("write probe: created private channel {name}");
    let result = write_probe_steps(&relay, &channel).await;
    // always try to clean up
    let cleanup = relay.submit(delete_channel_builder(&channel)?).await;
    match &cleanup {
        Ok(_) => println!("write probe: deleted the channel"),
        Err(e) => println!("write probe: COULD NOT DELETE the test channel {name} ({channel}): {e}"),
    }
    result?;
    cleanup?;
    println!("write probe ok");
    Ok(())
}

async fn load(relay: &Relay, channel: &str) -> Result<Store> {
    let w = relay.window(channel, None, 20).await?;
    let mut store = Store::default();
    for e in &w.events {
        store.apply(e);
    }
    Ok(store)
}

async fn write_probe_steps(relay: &Relay, channel: &str) -> Result<()> {
    use crate::relay::{delete_builder, edit_builder, message_builder};
    // the relay may need a moment before it lists the new channel
    let mut listed = false;
    for _ in 0..10 {
        if relay.channels().await?.iter().any(|c| c.id == channel) {
            listed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if !listed {
        bail!("the new channel never appeared in the channel list");
    }
    println!("write probe: the channel is listed");
    let id = relay.submit(message_builder(channel, "hello from the write probe", None, &[])?).await?;
    let store = load(relay, channel).await?;
    if !store.msgs.contains_key(&id) {
        bail!("the posted message did not show up in the channel");
    }
    println!("write probe: message posted and read back");
    relay.submit(edit_builder(channel, &id, "edited by the write probe")?).await?;
    let store = load(relay, channel).await?;
    let (text, edited) = store.content_of(&store.msgs[&id]);
    if !edited || text != "edited by the write probe" {
        bail!("the edit did not show up (shows {text:?})");
    }
    println!("write probe: edit applied");
    relay.submit(delete_builder(channel, &id)?).await?;
    let store = load(relay, channel).await?;
    if store.timeline().iter().any(|m| m.id == id) {
        bail!("the deleted message is still listed");
    }
    println!("write probe: message deleted");
    Ok(())
}
