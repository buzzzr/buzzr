#!/bin/sh
# A buzzr plugin: the time, in large digits, in a pane under the channel list.
# See PLUGIN.md in the repository root.
#
# Try it:  cp -r examples/plugins/clock ~/.config/buzzr/plugins/ && buzzr
#
# The pane has no key of its own. buzzr draws whatever the plugin last sent, from the pane in its
# `ready` contributions until the process stops, so nothing here has to run on a user's keypress.
# `"tickMs": 1000` in plugin.json asks buzzr for a heartbeat; each `tick` redraws the clock, which
# is why this stays a plain POSIX shell script with no timer of its own.
#
# Five rows, three cells per glyph, so "12:34" is 20 columns and fits the 28-column channel list.

# `draw HH:MM` leaves the five rows of the time in $r1 … $r5.
draw() {
    rest=$1
    r1=; r2=; r3=; r4=; r5=
    while [ -n "$rest" ]; do
        ch=${rest%"${rest#?}"}   # the first character
        rest=${rest#?}           # everything after it
        case "$ch" in
            0) a='███'; b='█ █'; c='█ █'; d='█ █'; e='███' ;;
            1) a=' █ '; b='██ '; c=' █ '; d=' █ '; e='███' ;;
            2) a='███'; b='  █'; c='███'; d='█  '; e='███' ;;
            3) a='███'; b='  █'; c='███'; d='  █'; e='███' ;;
            4) a='█ █'; b='█ █'; c='███'; d='  █'; e='  █' ;;
            5) a='███'; b='█  '; c='███'; d='  █'; e='███' ;;
            6) a='███'; b='█  '; c='███'; d='█ █'; e='███' ;;
            7) a='███'; b='  █'; c='  █'; d='  █'; e='  █' ;;
            8) a='███'; b='█ █'; c='███'; d='█ █'; e='███' ;;
            9) a='███'; b='█ █'; c='███'; d='  █'; e='███' ;;
            *) a='   '; b=' █ '; c='   '; d=' █ '; e='   ' ;;  # the colon, and anything unexpected
        esac
        r1="$r1$a "; r2="$r2$b "; r3="$r3$c "; r4="$r4$d "; r5="$r5$e "
    done
}

row() {
    printf '{"text":" %s","tone":"accent"}' "$1"
}

# The pane's fields, without the braces: the same object answers `ready` and a `pane` message.
pane() {
    draw "$(date +%H:%M)"
    printf '"title":"Clock","rows":[%s,%s,%s,%s,%s]' "$(row "$r1")" "$(row "$r2")" "$(row "$r3")" "$(row "$r4")" "$(row "$r5")"
}

while IFS= read -r line; do
    case "$line" in
        *'"type":"hello"'*)
            # Say we are ready, and bring the first clock with us so the pane is right from the start.
            printf '{"type":"ready","contributions":{"pane":{%s}}}\n' "$(pane)"
            ;;
        *'"type":"tick"'*)
            # The heartbeat plugin.json asked for: replace the pane with the time now.
            printf '{"type":"pane",%s}\n' "$(pane)"
            ;;
        *'"type":"bye"'*)
            exit 0
            ;;
    esac
done
