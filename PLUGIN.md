# Writing a buzzr plugin

A plugin is a **separate process** that adds commands, status items, badges and panels to buzzr. It never draws and never touches
the network: it speaks newline-delimited JSON on stdin/stdout, and buzzr owns every cell of the terminal. That is what makes hot
reload (a process swap), crash isolation and any language possible.

A plugin can put things on these surfaces and nothing else:

| Contribution | Where it shows |
|---|---|
| `commands` | `/commands` in the input box |
| `status` | one item on the status line (left of the built-in text) |
| `header` | a badge on the right of the header |
| `messageMarks` | a short tag in front of one message (by event id) |
| `channelMarks` | a short suffix after one channel (by channel id) |
| `pane` | a small block under the channel list, always on screen |
| `panel` | an overlay with selectable rows (sent on demand) |

Everything in and out is plain data: buzzr strips control characters, measures with `unicode_width`, clips to the space left and
maps a small set of tone names through the theme. **A plugin can never emit an escape sequence.**

## Where plugins live

`~/.config/buzzr/plugins/<id>/` — or `$BUZZR_PLUGINS_DIR`, or `--plugins-dir DIR`. Each plugin is one folder whose name equals the
manifest `id`. Put a file named `DISABLED` in the folder to turn it off without deleting anything.

See `examples/plugins/hello` for a complete, tiny plugin, and `examples/plugins/clock` for one that keeps a pane on screen and
redraws it from a heartbeat.

## The manifest: `plugin.json`

```json
{
  "id": "hello",
  "name": "Hello",
  "apiVersion": 1,
  "description": "A tiny example plugin.",
  "entry": ["sh", "main.sh"],
  "permissions": ["read:visible"]
}
```

- `id` — lowercase letters, digits, `-`, `_` or `.`, at most 64 characters. It must match the folder name.
- `apiVersion` — the contract version. This buzzr speaks `1`; a different value refuses the plugin (see the protocol below).
- `entry` — the command line to run, with the plugin folder as the working directory. At most 16 arguments, 4096 characters.
- `permissions` — what the plugin may do. Unknown names are dropped and shown, so a manifest never claims more than buzzr enforces.
  In this build: `read:visible` (get the screen projection) and `messages:write` (send messages).
- `tickMs` — an **opt-in heartbeat**, in milliseconds. When it is set, buzzr sends the plugin a `tick` this often so it can refresh
  something time-based (a clock, a build) without running a timer of its own. Absent or `0` means never — most plugins only react to
  commands and snapshots and are never woken. A value is clamped to `250`–`60000`.
- The process runs with a **clean environment** (only `PATH`, `HOME`, `LANG`, `LC_ALL`, `TZ`, `TMPDIR` and the `BUZZR_PLUGIN_*`
  variables are passed on), so no key or relay credential in buzzr's environment can leak into it.

## Lifecycle

discovered → starting → ready → stopped/failed. buzzr sends `hello`, the plugin must answer `ready` within **2 seconds** or it is
refused. buzzr polls the plugin folder every 2 s and, after a 300 ms debounce, **hot reloads** on any edit: it starts the new
revision, waits for its `ready`, and only then swaps the contributions and kills the old process. If the new revision fails, the
old one keeps running and buzzr flashes "kept previous". A crash drops the plugin's contributions and flashes; there is no
auto-restart (a crash loop is worse than a missing plugin). Plugin state is lost on reload — a plugin that needs to remember
something keeps its own file.

## The protocol

One JSON object per line, both ways. **Unknown message types and unknown fields are ignored** (and logged), so a newer plugin can
talk to an older buzzr. `apiVersion` in `hello` is the hard contract: an unknown value means the plugin stops.

### buzzr → plugin

```json
{"type":"hello","protocol":1,"apiVersion":1,"host":"buzzr/0.2.0","plugin":"hello"}
{"type":"snapshot","seq":7,"projection":{ /* see below */ }}
{"type":"tick","seq":3}
{"type":"invoke","id":11,"command":"hello","args":"the text after /hello"}
{"type":"panel_action","id":"hello","row":1,"value":"pick"}
{"type":"bye"}
```

`snapshot` is a bounded projection of what is on screen — debounced and sent only when it changes:

```json
{
  "me": "<your pubkey>",
  "connection": "live | connecting | offline: <why>",
  "channel": { "id": "…", "label": "dev-bazz" },
  "view": "channel | thread",
  "selected": "<message id>",
  "unread": 3,
  "messages": [
    { "id": "…", "author": "Bob", "pubkey": "…", "at": 1700000000, "text": "…", "replies": 2, "selected": false }
  ]
}
```

Only the rows on screen are included. The private key, the relay address, filters and other channels' history are **never** sent.

### plugin → buzzr

```json
{"type":"ready","contributions":{ /* see below */ }}
{"type":"status","text":"2 open PRs"}
{"type":"status","segments":[{"text":"2 open PRs","tone":"accent"}]}
{"type":"pane","title":"Clock","rows":[{"text":"12:34","tone":"accent"}]}
{"type":"panel","id":"prs","title":"Pull requests","rows":[{"text":"#42 fix the anchor","action":"open:42"}]}
{"type":"flash","text":"done"}
{"type":"draft","text":"text to put in the input box"}
{"type":"send","channel":"<channel id, optional>","text":"a message"}
{"type":"log","level":"error","text":"something went wrong"}
```

`contributions` (in `ready`):

```json
{
  "commands":     [{"id":"prs","title":"Pull requests","usage":"/prs [open|closed]"}],
  "status":       [{"text":"2 open","tone":"ok"}],
  "header":       [{"text":"gh","tone":"dim"}],
  "messageMarks": [{"target":"<message id>","text":"PR","tone":"accent"}],
  "channelMarks": [{"target":"<channel id>","text":"3","tone":"warn"}],
  "pane":         {"title":"Clock","rows":[{"text":"12:34","tone":"accent"}]}
}
```

A `tone` is one of `default`, `dim`, `accent`, `warn`, `error`, `ok`, `bold`. `text` is a single line; newlines and control
characters are stripped, and each text is capped at 2000 characters (a panel at 500 rows, a pane at 20, 64 commands, 32 status
segments).

## Capabilities

- **`draft`** puts text in the input box for the user to read and send. It is dropped if the user is already writing, and refused
  under `--read-only`.
- **`send`** needs `messages:write`. It is rate-limited (1/s, 20/min), only ever writes in the **channel that is open**, and is
  refused under `--read-only`. buzzr flashes a note and scrolls to the message when it arrives.
- **`panel_action`** is sent when the user presses Enter on a panel row that has an `action`. Panels are pull-based: buzzr asks for
  a panel with `invoke`, the plugin answers with one; status and badges are push-based. buzzr owns the panel's cursor, scrolling,
  focus and dismissal (Esc).
- **`pane`** is always on screen: buzzr draws it under the channel list, one per plugin, stacked in plugin-id order. It takes no focus,
  no key and no click, and its rows are clipped to the column. The `ready` pane and a `pane` message are the same object;
  `{"type":"pane"}` (no title, no rows) clears it, and the space goes back to the channel list. It is how a clock, a build or a counter
  stays visible between keypresses — with `tickMs` in the manifest, buzzr wakes the plugin on that interval so it can redraw.
- The plugin's stderr is captured to `<config dir>/plugin-logs/<id>.log`; `log` with `level:"error"` also flashes.

Not in this build (kept off the fixed surface on purpose): `open`/`copy`, a `store` API, `exec`, and an `agents` view. The design
notes are in `BUZZR_PLUGIN_FRAMEWORK.md` in the maintainer's workspace; add them here when they ship.

## On the command line

```bash
buzzr plugins list        # every folder, whether it would load, and where it lives
buzzr plugins disable hello
buzzr plugins enable hello
buzzr --no-plugins        # start without plugins
BUZZR_SAFE_MODE=1 buzzr    # the same
```

Inside the UI, `/plugins` shows the same list plus the commands plugins added.
