#!/bin/sh
# A tiny buzzr plugin. It reads one JSON message per line on stdin and answers with one
# JSON message per line on stdout. See PLUGIN.md in the repository root.
#
# Try it:  cp -r examples/plugins/hello ~/.config/buzzr/plugins/ && buzzr
# then type /hello.
while IFS= read -r line; do
  case "$line" in
    *'"type":"hello"'*)
      # Register the /hello command and a status item.
      printf '%s\n' '{"type":"ready","contributions":{"commands":[{"id":"hello","title":"Hello","usage":"/hello"}],"status":[{"text":"hello","tone":"ok"}]}}'
      ;;
    *'"type":"snapshot"'*)
      # A projection of what is on screen arrived; a real plugin would react to it here.
      printf '%s\n' '{"type":"status","segments":[{"text":"hello","tone":"ok"}]}'
      ;;
    *'"type":"invoke"'*)
      # /hello was run: answer with a panel. Rows with an "action" can be picked with Enter.
      printf '%s\n' '{"type":"panel","id":"hello","title":"Hello","rows":[{"text":"You ran /hello.","tone":"default"},{"text":"Pick me with Enter.","tone":"accent","action":"pick"}]}'
      ;;
    *'"type":"panel_action"'*)
      printf '%s\n' '{"type":"flash","text":"You picked a row. Edit examples/plugins/hello/main.sh to change me."}'
      ;;
    *'"type":"bye"'*)
      exit 0
      ;;
  esac
done
