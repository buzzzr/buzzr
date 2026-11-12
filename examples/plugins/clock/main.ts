#!/usr/bin/env node
// A buzzr plugin: the time, in large digits, in a pane under the channel list.
//
// Try it:  cp -r examples/plugins/clock ~/.config/buzzr/plugins/ && buzzr
//
// The pane has no key of its own: buzzr draws whatever the plugin last sent, and `tickMs` asks buzzr for a heartbeat, so
// the clock redraws without a timer of its own. `buzzr.ts` next to this file is the SDK — see PLUGIN.md.
//
// Five rows of three cells per glyph, so "12:34" is 20 columns and fits the 28-column channel list.

import { definePlugin, type Pane, type Segment } from "./buzzr.ts";

const GLYPHS: Record<string, string[]> = {
  "0": ["███", "█ █", "█ █", "█ █", "███"],
  "1": [" █ ", "██ ", " █ ", " █ ", "███"],
  "2": ["███", "  █", "███", "█  ", "███"],
  "3": ["███", "  █", "███", "  █", "███"],
  "4": ["█ █", "█ █", "███", "  █", "  █"],
  "5": ["███", "█  ", "███", "  █", "███"],
  "6": ["███", "█  ", "███", "█ █", "███"],
  "7": ["███", "  █", "  █", "  █", "  █"],
  "8": ["███", "█ █", "███", "█ █", "███"],
  "9": ["███", "█ █", "███", "  █", "███"],
};
// The colon, and anything unexpected.
const BLANK = ["   ", " █ ", "   ", " █ ", "   "];

/** The five rows of block digits for a time like "12:34". */
function draw(time: string): string[] {
  const rows = ["", "", "", "", ""];
  for (const glyph of time) {
    for (const [row, line] of (GLYPHS[glyph] ?? BLANK).entries()) rows[row] += line + " ";
  }
  return rows;
}

/** The pane as it looks right now. */
function clock(): Pane {
  const now = new Date();
  const time = `${String(now.getHours()).padStart(2, "0")}:${String(now.getMinutes()).padStart(2, "0")}`;
  const rows: Segment[] = draw(time).map((text) => ({ text: ` ${text}`, tone: "accent" }));
  return { title: "Clock", rows };
}

definePlugin({
  name: "Clock",
  // The heartbeat is declared here, in code, not in a manifest: buzzr sends `tick` about once a second and the pane
  // redraws. Nothing above this line has to know how the protocol works.
  tickMs: 1000,
  ready: () => ({ pane: clock() }),
  onTick: (buzzr) => buzzr.pane(clock()),
});
