# Writing a buzzr plugin

A plugin is a **separate process** that adds commands, status items, badges, panes and panels to buzzr. It never draws and never touches
the network: it speaks newline-delimited JSON on stdin/stdout, and buzzr owns every cell of the terminal. That is what makes hot
reload (a process swap), crash isolation and any language possible.

That is one of buzzr's two kinds of plugin: the **runtime-loaded** one, added later, in any language. The other kind is
**compiled-in**: a Rust type in buzzr's own tree (or in a crate that depends on buzzr) that registers itself with
`register_plugin!`. Both speak the same contract described here and are held to the same rules — a compiled-in plugin gets no door
this one lacks. `plugins/agents.rs` (`/agents`) is the worked example of the compiled-in kind; the rest of this document is the
wire a runtime-loaded plugin speaks.

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

`~/.config/buzzr/plugins/<id>/` — or `$BUZZR_PLUGINS_DIR`, or `--plugins-dir DIR`.

**There is no manifest, and no file describing the plugin.** A plugin is one folder, and everything it declares about itself — its
display name, what it is allowed to do, and whether it wants a heartbeat — it says in its own code, over the protocol, when it is
ready. Nothing about a plugin lives outside the plugin.

The **folder name is the plugin's id**: lowercase letters, digits, `-`, `_` or `.`, at most 64 characters. A folder named anything
else is listed as broken and never run — the id reaches file names and the UI, so it is checked before anything starts.

### The entry file

Inside the folder, buzzr runs the first of these names that exists:

```
main.ts   main.js   main.mjs   main.py   main.rb   main.sh   main   run
```

It must be a file with the executable bit set (`chmod +x main.sh` in the plugin folder tells you what to do). buzzr runs **that
file**, and the kernel's `#!` line decides the interpreter, so a plugin can be a shell script, a Node, Bun or Deno program, a Python
script, or a compiled binary. buzzr keeps no table of languages: adding one is a matter of writing the program. The child's working
directory is the plugin folder.

To turn a plugin off without deleting anything, put a file named `DISABLED` in the folder (`buzzr plugins disable <id>` does it).

See `examples/plugins/hello` for a complete, tiny plugin written by hand against the raw protocol, and `examples/plugins/clock`
for one built on the TypeScript SDK that keeps a pane on screen and redraws it from a heartbeat.

## Lifecycle

discovered → starting → ready → stopped/failed. buzzr sends `hello`, the plugin must answer `ready` within **2 seconds** or it is
refused. buzzr polls the plugin folder every 2 s and, after a 300 ms debounce, **hot reloads** on any edit: it starts the new
revision, waits for its `ready`, and only then swaps the contributions and kills the old process. If the new revision fails, the
old one keeps running and buzzr flashes "kept previous". A crash drops the plugin's contributions and flashes; there is no
auto-restart (a crash loop is worse than a missing plugin). Plugin state is lost on reload — a plugin that needs to remember
something keeps its own file.

## The protocol

One JSON object per line, both ways, at most 64 KiB a line. **Unknown message types and unknown fields are ignored** (and logged),
so a newer plugin can talk to an older buzzr. The `apiVersion` in `hello` is the hard contract: this buzzr speaks `2`, and a plugin
that cannot speak it should say why on the log and exit.

### buzzr → plugin

```json
{"type":"hello","protocol":1,"apiVersion":2,"host":"buzzr/0.2.0","plugin":"hello"}
{"type":"snapshot","seq":7,"projection":{ /* see below */ }}
{"type":"tick","seq":3}
{"type":"invoke","id":11,"command":"hello","args":"the text after /hello"}
{"type":"panel_action","id":"hello","row":1,"value":"pick"}
{"type":"prompt_answer","id":"name","text":"Scout"}
{"type":"agents","id":"list","agents":[{"name":"Scout","pubkey":"<hex>","npub":"npub1…","harness":"claude-code-acp","workspace":"/…/agents/scout","created_at":1700000000}]}
{"type":"created","id":"new","agent":{"name":"Scout","pubkey":"<hex>","npub":"npub1…","published":true,"detail":""}}
{"type":"bye"}
```

`snapshot` is a bounded projection of what is on screen — debounced and sent only when it changes:

```json
{
  "me": "<your pubkey>",
  "connection": "live | connecting | offline: <why>",
  "channel": { "id": "…", "label": "dev-buzzr" },
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
{"type":"ready","name":"Hello","permissions":["read:visible"],"contributions":{ /* see below */ }}
{"type":"status","text":"2 open PRs"}
{"type":"status","segments":[{"text":"2 open PRs","tone":"accent"}]}
{"type":"pane","title":"Clock","rows":[{"text":"12:34","tone":"accent"}]}
{"type":"panel","id":"prs","title":"Pull requests","rows":[{"text":"#42 fix the anchor","action":"open:42"}]}
{"type":"flash","text":"done"}
{"type":"draft","text":"text to put in the input box"}
{"type":"send","channel":"<channel id, optional>","text":"a message"}
{"type":"prompt","id":"name","title":"new agent · name","hint":"Type a name, then Enter","multiline":false}
{"type":"list_agents","id":"list"}
{"type":"create_agent","id":"new","name":"Scout","instructions":"watch the repo"}
{"type":"log","level":"error","text":"something went wrong"}
```

### `ready`: everything the plugin declares, in one message

`ready` is where a plugin says who it is. All four fields are optional, and leaving one out never breaks anything:

| Field | Meaning |
|---|---|
| `name` | The name `/plugins` and the plugin's own rows show. Control characters are stripped; empty or absent means "call it by the folder name". |
| `permissions` | What the plugin asks to be allowed to do, as a list. **Unknown names are dropped** (and the drop is logged), and the granted set is what `/plugins` shows, so a plugin never appears to hold more than buzzr enforces. In this build: `read:visible` (get the screen projection), `messages:write` (send messages), `agents:read` (see the local agents) and `agents:write` (make and publish one). |
| `tickMs` | An **opt-in heartbeat**, in milliseconds: buzzr then sends this plugin a `tick` on that interval, so time-based output (a clock, a build) can redraw without running a timer of its own. Absent or `0` means never — most plugins only react to commands and snapshots and are never woken. A value is clamped to `250`–`60000`. |
| `contributions` | What the plugin puts on screen to begin with — see below. Absent means it adds nothing until asked. |

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

## The TypeScript SDK

`sdk/typescript/buzzr.ts` wraps the protocol in types. A plugin declares the whole of itself — name, permissions, heartbeat,
contributions and handlers — in one `definePlugin` call, and writes no JSON by hand. It is a single file with no dependencies:
copy it next to your entry file.

It needs a JavaScript runtime on `PATH` and nothing else: Node, Bun or Deno. A recent Node runs the `.ts` file directly by
stripping the types; on an older one, put `--experimental-strip-types` after the interpreter in the shebang.

The entry file of `examples/plugins/clock` is the whole story:

```ts
#!/usr/bin/env node
import { definePlugin, type Pane, type Segment } from "./buzzr.ts";

function clock(): Pane {
  const now = new Date();
  const time = `${String(now.getHours()).padStart(2, "0")}:${String(now.getMinutes()).padStart(2, "0")}`;
  return { title: "Clock", rows: draw(time).map(text => ({ text, tone: "accent" })) };
}

definePlugin({
  name: "Clock",
  // Ask buzzr for a heartbeat, in code: no manifest, no timer of our own.
  tickMs: 1000,
  // What to show the moment we are ready, and again on every tick.
  ready: () => ({ pane: clock() }),
  onTick: (buzzr) => buzzr.pane(clock()),
});
```

`definePlugin` takes `name`, `permissions` and `tickMs` (the `ready` fields), plus `ready`, `onSnapshot`, `onCommand`,
`onPanelAction`, `onTick`, `onPrompt`, `onAgents` and `onCreated`. Its handle has `status`, `pane`, `panel`, `flash`, `draft`, `send`,
`prompt`, `listAgents`, `createAgent` and `log`; the types are in the file and are shorter to read than this paragraph.

The clock example carries its own copy of the SDK so that `cp -r examples/plugins/clock ~/.config/buzzr/plugins/` is enough — a test
keeps that copy byte-for-byte identical to `sdk/typescript/buzzr.ts`.

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
  stays visible between keypresses — with `tickMs` in `ready`, buzzr wakes the plugin on that interval so it can redraw.
- **`prompt`** asks buzzr to put a question in the composer — one line, or (`multiline: true`) a block of text — so a plugin can collect
  something from the user without drawing. buzzr owns the box: the `title` names the prompt in its border, the `hint` is the placeholder
  while it is empty, and Enter accepts (Esc cancels). The answer comes back as `prompt_answer` with the same `id`, and `text` is left out
  when the user cancelled. A prompt is refused while a prompt is already up, and typing in the box is not a write, so a prompt is allowed
  under `--read-only`. One prompt at a time is the rule: a second request is dropped with a flash rather than clobbering the first.
- **`list_agents`** (needs `agents:read`) asks for the local agents this copy made. The answer is an `agents` message with the same `id`
  and one entry each: `name`, `pubkey`, `npub`, `harness`, `workspace`, `created_at`. There is **no key** in it — the key stays in
  `agents.json`, which only buzzr reads.
- **`create_agent`** (needs `agents:write`) asks buzzr to make a local agent — a key, a folder and a record of its own — and publish it.
  It is refused under `--read-only`, and the answer is a `created` message with the same `id`: the `agent` it made (with `published`
  telling whether the relay took it and `detail` why not), or an `error`. buzzr owns the key, the folder and the relay; the plugin never
  sees them.
- The process runs with a **clean environment** (only `PATH`, `HOME`, `LANG`, `LC_ALL`, `TZ`, `TMPDIR` and the `BUZZR_PLUGIN_*`
  variables are passed on), so no key or relay credential in buzzr's environment can leak into a plugin. Its stderr is captured to
  `<config dir>/plugin-logs/<id>.log`; `log` with `level:"error"` also flashes.

Not in this build (kept off the fixed surface on purpose): `open`/`copy`, a `store` API, and `exec`.

## On the command line

```bash
buzzr plugins list        # every folder, whether it would load, and the file buzzr would run
buzzr plugins disable hello
buzzr plugins enable hello
buzzr --no-plugins        # start without plugins
BUZZR_SAFE_MODE=1 buzzr    # the same
```

Inside the UI, `/plugins` shows what loaded, what each plugin may do, and the commands it added. The name in that list is the one
the plugin itself sent in `ready`, which is why `plugins list` — which never starts anything — shows ids and files instead.
