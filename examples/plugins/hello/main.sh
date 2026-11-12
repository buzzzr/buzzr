#!/bin/sh
# A tiny buzzr plugin. It reads one JSON message per line on stdin and answers with one JSON message per line on
# stdout. See PLUGIN.md in the repository root.
#
# This is the same protocol the TypeScript SDK speaks (see examples/plugins/clock); it is spelled out by hand here so a
# plugin in any language, with no library at all, has something to copy.
#
# Try it:  cp -r examples/plugins/hello ~/.config/buzzr/plugins/ && buzzr
# then run the hello command from the Ctrl+K palette.
while IFS= read -r line; do
  case "$line" in
    *'"type":"hello"'*)
      # buzzr greets us with the contract version it speaks; refuse a host we do not understand.
      case "$line" in
        *'"apiVersion":2'*) ;;
        *) printf '%s\n' '{"type":"log","level":"error","text":"this plugin speaks apiVersion 2"}'; exit 1 ;;
      esac
      # Declare everything in code: buzzr reads no manifest, so `name` and `permissions` come from here. `read:visible` lets us
      # see the projection (nothing here uses it, but it is what a real plugin asks for).
      printf '%s\n' '{"type":"ready","name":"Hello","permissions":["read:visible"],"contributions":{"commands":[{"id":"hello","title":"Hello"}],"status":[{"text":"hello","tone":"ok"}]}}'
      ;;
    *'"type":"snapshot"'*)
      # A projection of what is on screen arrived; a real plugin would react to it here.
      printf '%s\n' '{"type":"status","segments":[{"text":"hello","tone":"ok"}]}'
      ;;
    *'"type":"invoke"'*)
      # the hello command was run: answer with a panel. Rows with an "action" can be picked with Enter.
      printf '%s\n' '{"type":"panel","id":"hello","title":"Hello","rows":[{"text":"You ran the hello command.","tone":"default"},{"text":"Pick me with Enter.","tone":"accent","action":"pick"}]}'
      ;;
    *'"type":"panel_action"'*)
      printf '%s\n' '{"type":"flash","text":"You picked a row. Edit examples/plugins/hello/main.sh to change me."}'
      ;;
    *'"type":"bye"'*)
      exit 0
      ;;
  esac
done
