#!/bin/sh
# One `letronna -p` run inside a Harbor trial, driven over stream-json.
#
#   drive.sh MESSAGE LOG letronna -p --input-format stream-json ...
#
# MESSAGE is a file holding the one `user` line to send. Every line Letronna
# writes is appended to LOG, and its `result` line is also printed, so Harbor
# sees why a run failed. Each `can_use_tool` request is answered with allow:
# the task container is thrown away after the trial, and with nobody to ask,
# `-p` refuses what is hard to take back, such as `rm -r` or `git reset
# --hard`, even with --yes. What Letronna never runs, it still refuses
# before asking. Once the result line is in, stdin is closed, which is what
# lets Letronna exit; its exit code is this script's.
#
# Plain sh with a fifo each way, as the task image may have no Python or
# bash for a driver.

set -u

message=$1
log=$2
shift 2

work="$(mktemp -d)" || exit 1
trap 'rm -rf "$work"' EXIT
mkfifo "$work/in" "$work/out" || exit 1

"$@" <"$work/in" >"$work/out" &
letronna=$!
# Letronna can exit before it reads, and a write to it then has to fail
# rather than end this script before it logs why. Set after Letronna starts,
# so Letronna keeps the signal.
trap '' PIPE
# Opened in the order Letronna opens them, as each open waits for the other
# end.
exec 3>"$work/in" 4<"$work/out"
cat "$message" >&3

# The request id, from either key order serde_json may write in: sorted, or
# as written with preserve_order. Both are anchored at an end of the line, so
# a tool input holding the same words cannot be taken for them.
request_id() {
    printf '%s\n' "$1" | sed -n \
        -e 's/^{"type":"control_request","request_id":"\(letronna-[0-9][0-9]*\)".*/\1/p' \
        -e 's/.*,"request_id":"\(letronna-[0-9][0-9]*\)","type":"control_request"}$/\1/p'
}

while IFS= read -r line <&4; do
    printf '%s\n' "$line" >>"$log"
    case $line in
    '{"type":"control_request",'* | *',"type":"control_request"}')
        id="$(request_id "$line")"
        if [ -n "$id" ]; then
            # In a subshell, as bash keeps what a printf could not write
            # and puts it out with its next one, here the log.
            (printf '{"type":"control_response","response":{"request_id":"%s","subtype":"success","response":{"behavior":"allow"}}}\n' "$id") >&3
        else
            # Left unanswered, the call would wait forever; with stdin
            # closed, Letronna refuses it and every later one itself.
            echo "drive.sh: a request with no id it could read; closing stdin" >&2
            exec 3>&-
        fi
        ;;
    *',"type":"result"}')
        printf '%s\n' "$line"
        break
        ;;
    esac
done

exec 3>&-
# Read to the end, so Letronna never blocks on a full pipe as it exits.
cat <&4 >>"$log"
exec 4<&-
wait "$letronna"
