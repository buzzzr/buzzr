/**
 * The buzzr plugin SDK — the typed way to declare a plugin.
 *
 * A plugin is a program that speaks one JSON object per line on stdin and stdout with buzzr (see `PLUGIN.md`). This file
 * wraps that protocol so the whole plugin — its name, what it is allowed to do, its contributions and its handlers —
 * lives in one typed place, and nothing is handwritten JSON.
 *
 * It is one file with no dependencies: copy it next to your plugin's entry file. `examples/plugins/clock` is a complete
 * plugin built on it; that example folder carries its own copy of this file so it can be copied on its own, and a test keeps the
 * two byte-for-byte identical.
 */

/** The apiVersion this SDK speaks. The plugin refuses a host that speaks anything else. */
export const API_VERSION = 2;

/** How a piece of text is drawn. The host owns the colours, so `BUZZR_THEME` applies to plugin output too. */
export type Tone = "default" | "dim" | "accent" | "warn" | "error" | "ok" | "bold";

/** A piece of one line: some text and how it should look. */
export interface Segment {
  text: string;
  tone?: Tone;
}

/** One command a plugin adds to the input box, run as `/id`. */
export interface Command {
  id: string;
  title?: string;
  usage?: string;
}

/** A short tag in front of one message (by event id) or one channel (by channel id). */
export interface Mark {
  target: string;
  text: string;
  tone?: Tone;
}

/** A region buzzr draws under the channel list, one per plugin. It is display-only: no key, no focus, no click. */
export interface Pane {
  title?: string;
  rows: Segment[];
}

/** A row of an overlay panel. `action` is what Enter sends back to `onPanelAction`. */
export interface PanelRow {
  text: string;
  tone?: Tone;
  action?: string;
}

/** An overlay with selectable rows, usually answered from an `onCommand` handler. */
export interface Panel {
  id: string;
  title?: string;
  rows: PanelRow[];
}

/** Everything a plugin registers the moment it is ready. */
export interface Contributions {
  commands?: Command[];
  status?: Segment[];
  header?: Segment[];
  messageMarks?: Mark[];
  channelMarks?: Mark[];
  pane?: Pane;
}

/** One message in the projection. Only the rows on screen are sent, never the key or another channel's history. */
export interface MessageView {
  id: string;
  author: string;
  pubkey: string;
  at: number;
  text: string;
  replies: number;
  selected: boolean;
}

/** The channel the open view belongs to. */
export interface ChannelView {
  id: string;
  label: string;
}

/** What buzzr tells a plugin about the open view — only what is on screen. */
export interface View {
  me: string;
  connection: string;
  channel?: ChannelView;
  view: "channel" | "thread";
  selected?: string;
  messages: MessageView[];
  unread: number;
}

/** One local agent this copy made, as the answer to `listAgents`. There is no key in it — that stays in `agents.json`. */
export interface AgentView {
  name: string;
  pubkey: string;
  npub: string;
  harness: string;
  workspace: string;
  created_at: number;
}

/** The answer to `createAgent`: the agent it made and whether the relay took it (`detail` says why not). */
export interface CreatedAgent {
  name: string;
  pubkey: string;
  npub: string;
  published: boolean;
  detail: string;
}

/** The handle a plugin uses to send things back. Every call is fire-and-forget. */
export interface Buzzr {
  /** Replace the plugin's status item on the status line; a string is one plain segment. */
  status(status: string | Segment[]): void;
  /** Replace the pane under the channel list; `null` gives the space back. */
  pane(pane: Pane | null): void;
  /** Show an overlay panel (buzzr owns its cursor, scrolling and dismissal). */
  panel(panel: Panel): void;
  /** Flash a short note on the status line. */
  flash(text: string): void;
  /** Put text in the input box for the user to read and send. */
  draft(text: string): void;
  /** Send a message in the open channel; needs the `messages:write` permission. */
  send(text: string): void;
  /** Ask the user a question in the composer; the answer comes back to `onPrompt` with the same `id`. */
  prompt(id: string, title: string, hint: string, multiline?: boolean): void;
  /** Ask for the local agents; needs the `agents:read` permission. The answer comes back to `onAgents`. */
  listAgents(id: string): void;
  /** Ask buzzr to make and publish an agent; needs the `agents:write` permission. The answer comes back to `onCreated`. */
  createAgent(id: string, name: string, instructions: string): void;
  /** Write a line to the plugin's log; `error` also flashes. */
  log(text: string, level?: "info" | "error"): void;
}

/** What a plugin declares about itself, in code. This is what a manifest used to hold. */
export interface PluginSpec {
  /** The name shown by `/plugins`; the folder name is always the id. */
  name?: string;
  /** What the plugin asks to do: `read:visible`, `messages:write`, `agents:read` and/or `agents:write`. buzzr grants only what it implements. */
  permissions?: string[];
  /** Ask buzzr for a heartbeat this many milliseconds apart (clamped to 250–60000) so time-based output can redraw. */
  tickMs?: number;
  /** The contributions to register when buzzr greets the plugin. */
  ready?(buzzr: Buzzr): Contributions | void;
  /** The screen changed; `view` holds only what is on screen. */
  onSnapshot?(buzzr: Buzzr, view: View): void;
  /** `/command args` was run. */
  onCommand?(buzzr: Buzzr, command: string, args: string): void;
  /** Enter was pressed on a panel row that had an action. */
  onPanelAction?(buzzr: Buzzr, panel: string, row: number, value: string | undefined): void;
  /** The heartbeat this plugin asked for with `tickMs`. */
  onTick?(buzzr: Buzzr): void;
  /** The user answered a prompt this plugin asked for with `prompt`; `text` is `undefined` on Esc. */
  onPrompt?(buzzr: Buzzr, id: string, text: string | undefined): void;
  /** The local agents this plugin asked for with `listAgents`. */
  onAgents?(buzzr: Buzzr, id: string, agents: AgentView[]): void;
  /** The answer to a `createAgent`: the agent it made, or `error` when it could not be made. */
  onCreated?(buzzr: Buzzr, id: string, agent: CreatedAgent | undefined, error: string | undefined): void;
}

// `process` is the one host object every runtime a plugin can be written for provides (Node, Bun, and Deno's Node
// compatibility). Reaching it through `globalThis` keeps this file free of `@types/node`, so copying it into a plugin
// folder needs no package to install and no tsconfig to write.
interface Stdio {
  stdin: { setEncoding(encoding: string): void; on(event: "data" | "end", listener: (chunk: string) => void): void };
  stdout: { write(text: string, done?: () => void): void };
  exit(code?: number): void;
}
const io = (globalThis as unknown as { process: Stdio }).process;

function send(message: Record<string, unknown>): void {
  io.stdout.write(JSON.stringify(message) + "\n");
}

/** `bye`, or a fatal misunderstanding: say why on the log and stop. */
function quit(why: string): void {
  io.stdout.write(JSON.stringify({ type: "log", level: "error", text: why }) + "\n", () => io.exit(1));
}

const buzzr: Buzzr = {
  status: (status) => send({ type: "status", ...(typeof status === "string" ? { text: status } : { segments: status }) }),
  pane: (pane) => send(pane === null ? { type: "pane" } : { type: "pane", ...pane }),
  panel: (panel) => send({ type: "panel", ...panel }),
  flash: (text) => send({ type: "flash", text }),
  draft: (text) => send({ type: "draft", text }),
  send: (text) => send({ type: "send", text }),
  prompt: (id, title, hint, multiline = false) => send({ type: "prompt", id, title, hint, multiline }),
  listAgents: (id) => send({ type: "list_agents", id }),
  createAgent: (id, name, instructions) => send({ type: "create_agent", id, name, instructions }),
  log: (text, level = "info") => send({ type: "log", level, text }),
};

/**
 * Declare a plugin and start talking to buzzr. Call it once, at the top level of the entry file:
 *
 * ```ts
 * definePlugin({
 *   name: "Clock",
 *   tickMs: 1000,
 *   ready: () => ({ pane: { title: "Clock", rows: [{ text: clock(), tone: "accent" }] } }),
 *   onTick: (b) => b.pane({ title: "Clock", rows: [{ text: clock(), tone: "accent" }] }),
 * });
 * ```
 */
export function definePlugin(spec: PluginSpec): Buzzr {
  let buffer = "";
  const handle = (line: string): void => {
    let message: { type?: string; apiVersion?: number; projection?: View; command?: string; args?: string; id?: string; row?: number; value?: string; text?: string; agents?: AgentView[]; agent?: CreatedAgent; error?: string };
    try {
      message = JSON.parse(line);
    } catch {
      return; // a line that is not JSON is not ours to interpret; buzzr logs its own copy of the protocol
    }
    switch (message.type) {
      case "hello": {
        if (message.apiVersion !== API_VERSION) {
          quit(`this plugin speaks apiVersion ${API_VERSION}, but buzzr speaks ${message.apiVersion}`);
          return;
        }
        const contributions = spec.ready?.(buzzr) ?? {};
        // One message, and only the fields the plugin actually set: an absent name means "call it by the folder name".
        send({
          type: "ready",
          contributions,
          ...(spec.name === undefined ? {} : { name: spec.name }),
          ...(spec.permissions === undefined ? {} : { permissions: spec.permissions }),
          ...(spec.tickMs === undefined ? {} : { tickMs: spec.tickMs }),
        });
        break;
      }
      case "snapshot":
        if (message.projection !== undefined) spec.onSnapshot?.(buzzr, message.projection);
        break;
      case "tick":
        spec.onTick?.(buzzr);
        break;
      case "invoke":
        spec.onCommand?.(buzzr, message.command ?? "", message.args ?? "");
        break;
      case "panel_action":
        spec.onPanelAction?.(buzzr, message.id ?? "", message.row ?? 0, message.value);
        break;
      case "prompt_answer":
        spec.onPrompt?.(buzzr, message.id ?? "", message.text);
        break;
      case "agents":
        spec.onAgents?.(buzzr, message.id ?? "", message.agents ?? []);
        break;
      case "created":
        spec.onCreated?.(buzzr, message.id ?? "", message.agent, message.error);
        break;
      case "bye":
        io.exit(0);
        break;
      default:
        break; // an unknown message type is ignored, so a newer buzzr can talk to this plugin
    }
  };

  io.stdin.setEncoding("utf8");
  io.stdin.on("data", (chunk: string) => {
    buffer += chunk;
    for (let newline = buffer.indexOf("\n"); newline >= 0; newline = buffer.indexOf("\n")) {
      const line = buffer.slice(0, newline);
      buffer = buffer.slice(newline + 1);
      if (line.trim() !== "") handle(line);
    }
  });
  io.stdin.on("end", () => io.exit(0));
  return buzzr;
}
