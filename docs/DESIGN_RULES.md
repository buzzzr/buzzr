# Design rules: keeping buzzr's shape

`AGENTS.md` says what to do and `ARCHITECTURE.md` says how the program fits together. This file names the **antipatterns** that
grow a codebase like this one into a mess, why each one is tempting, and what to do instead. Read it before adding a feature,
and again in review when a diff touches many files for one idea. Each rule points at the code that already follows it.

The rules come from a review of the code at 0.2.0-rc.3 (god-file, repeated shapes, logic in the binary, stringly errors). They are
here so the next feature does not bring those back.

## 1. One file, one reason to change

**Antipattern:** the file everything lands in. `app.rs` reached 4.2k lines because every new overlay, command and flow was
"just another method on `App`".

**Instead:**
- State lives where it is defined (`App` in `app.rs`). Behaviour lives in a module for the *feature*: `app/palette.rs`,
  `app/people.rs`, `app/plugins.rs`, `app/reactions.rs`, `app/search.rs`. Each is a further `impl App` block with a header saying what it
  covers. A method that is only used inside its module stays private to it; one used elsewhere is `pub(super)`.
- A new feature gets a new module (`app/<feature>.rs`) from the start, not a place in the biggest file. If most of a module's code touches
  one piece of state, give that state its own struct (see `Store`, `Editor`, `Registry`) with its own tests, rather than another field on `App`.
- Rough limits that should trigger a split: a file over about 1,500 lines, an `impl` block over about 800, a function over about
  100. These are prompts to look, not a hard rule; `ui.rs` (1.7k) is the next candidate.
- Tests move with the code: pure logic gets unit tests beside it; behaviour through `App::handle` goes in `app/tests.rs`.

## 2. Say a thing once

**Antipattern:** the same three lines pasted at every call site, so that a fix or a new case must find them all.
It was `t.first().map(String::as_str) == Some("p") && t.get(1)…` in four files, `now()` in four, and twenty
`tokio::spawn(async { … tx.send(AppEvent::X) })` arms.

**Instead:**
- **Tags:** read them through `Ev::tag`, `tag_values`, `tags_named`, `has_tag` (and `root_tag`/`reply_tag` for threads). Never compare
  `t.first()` or index into a tag yourself; a new kind of lookup is a new method on `Ev`.
- **Time:** `clock::now_secs()` is the only wall-clock read. Do not define another `now()`.
- **Commands that call the relay:** an arm of `run` in `main.rs` is one `request(tx, <future>, <AppEvent variant>)`. If you are
  writing `tokio::spawn` and `tx.send` by hand there, you are adding the 21st copy; extend `request` instead. (A spawn that sends
  nothing back, like `Typing`, or a count that fails silently, like `LoadUnread`, is the exception, and says why.)
- **Event builders:** `relay.rs` builders go through `channel_uuid`, `event_id` and `sdk_err`. Event formats come from `buzz-sdk`; do not
  assemble tags by hand when it has a builder.
- Before you write a helper, `grep` for one. After you write a third copy of anything, stop and extract it.
- Do not extract at two copies when they only *look* alike: wait until the third shows what actually varies.

## 3. The binary is a shell; the library has the logic

**Antipattern:** `create_and_publish` and `delete_and_unpublish` sat in `main.rs`, where no test and no plugin crate could reach them.

**Instead:**
- `main.rs` parses arguments, sets up the terminal, runs the loop and maps `Cmd` to calls. It contains no rules (ordering, validation,
  "relay first, then disk"). Those go in the module that owns the subject (`agents.rs`, `relay.rs`, `config.rs`) as functions that
  take their dependencies as arguments (`&Relay`, `&Paths`), so they can be tested without a terminal.
- `lib.rs` is the public surface plugin crates depend on. Anything a second binary or plugin would need belongs in the library.

## 4. Never block the loop, and keep writes ordered

**Antipattern:** `Cmd::Save*` wrote files on the loop thread because a spawned write could land after a newer one.

**Instead:**
- File writes go through `Disk` in `main.rs`: one queue, one task, executed in order on the blocking pool. Do not call `std::fs` from
  the loop or from `App`. On exit the loop awaits `Disk::flush`; anything that must survive quitting is queued before that.
- The same goes for any new "write the whole state each time" file: queue it, do not spawn a write per change.
- Reconnect and retry loops add jitter; a fixed delay makes every client retry together.

## 5. Errors say what went wrong, in a type

**Antipattern:** `Result<_, String>` everywhere, so callers cannot tell a duplicate name from a failed disk write and `format!("{e:#}")`
everything just to get through the signature.

**Instead:**
- In library code with several distinct failures a caller may care about, use an error enum with `thiserror`
  (`agents::Error` is the pattern: one variant per cause, `Display` is the user-facing text, `From<Error> for String` for the
  places that only show it). Keep the underlying `source` (the `io::Error`) instead of formatting it away.
- Glue and async code that just passes failures up uses `anyhow` with `.context(…)`.
- A `String` error is for the edge where it is only displayed: the `result` inside an `AppEvent` is a `String` on purpose (`request`
  makes it with `{err:#}`), because the UI shows it and `AppEvent` stays `Clone` and comparable. Do not use it for new internal APIs.
- Tests match on a variant (`matches!(err, Error::Duplicate(_))`) where they can, not on message text.

## 6. State that means something gets its own variant

**Antipattern:** `Live::Down` carried relay notices, so a NOTICE showed as "offline".

**Instead:** if two things are handled differently by the receiver, they are two variants, even if the sender has one channel for
both. Do not smuggle a case into a string, a magic empty value, or a flag next to an `Option`. Adding the variant makes every
`match` say what it does with it.

## 7. Trust boundaries are explicit

**Antipattern:** data from the network, a plugin, or another author's message treated as instructions or as safe text.

**Instead:** the rules in `AGENTS.md` principle 3 (message text and plugin output are hostile) apply to everything new. Also:
- Anything that acts on behalf of the user or an agent (running a tool, sending a message, deleting) is decided by *who asked*, and the
  author of an event is a fact to check, not a field to trust.
- A new permission or capability is default-deny and has a test that shows it refused.
- Secrets: see principle 4. A key stored in plain text is a stated trade-off in the file's header comment, not a quiet default; when
  a second place stores a key, use the same protection or say why not.

## 8. Reviewing for these

When reviewing or writing a change, ask:

1. Does it add to the biggest file, or does the feature have a module? (Rule 1)
2. Is any piece of it a copy of something nearby? (Rule 2)
3. Is there a rule in `main.rs` that a test cannot reach? (Rule 3)
4. Does the loop wait on anything, or can two writes race? (Rule 4)
5. Could a caller tell the failures apart if it needed to? (Rule 5)
6. Is there a case hiding in a string, a flag or a reused variant? (Rule 6)
7. Who is allowed to cause this, and is that checked? (Rule 7)

Known debts, not excuses: `app.rs` still holds the largest `impl App` block (navigation, layout, unread); `ui.rs` is large;
`Cmd` and `AppEvent` are still mirrored by hand; and `agents.json` keeps agent keys as plain text (mode 600, see the header of
`agents.rs`). Fix these when you are in the area; do not add to them.
