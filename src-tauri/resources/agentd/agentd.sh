#!/bin/sh
# agentd: the program inside each agent VM. One task per boot: it reads its bootstrap and task from
# the app over the virtio-serial port, runs `claude` on the task as an unprivileged user, streams
# what `claude` prints and ends with one `final` message. Messages are one JSON object per line
# (src/protocol.rs). Needs busybox ash, jq, setpriv (util-linux) and setsid.
#
#   agentd.sh [--channel <path>] [--out <path>] [--workspace <dir>] [--claude <path>]
#             [--run-as <user>] [--poweroff]
#
# --out is only for tests: with it the channel is split into two files (fifos). Everything
# `claude` prints is untrusted data: wrapped by jq, truncated, never run.
set -u

IN=/dev/virtio-ports/agent
OUT=
WS=/home/agent/work
CLAUDE=claude
RUN_AS=
POWEROFF=0
while [ $# -gt 0 ]; do
	case $1 in
	--channel) IN=$2; shift ;;
	--out) OUT=$2; shift ;;
	--workspace) WS=$2; shift ;;
	--claude) CLAUDE=$2; shift ;;
	--run-as) RUN_AS=$2; shift ;;
	--poweroff) POWEROFF=1 ;;
	*) echo "agentd: unknown argument $1" >&2; exit 2 ;;
	esac
	shift
done
[ -n "$OUT" ] || OUT=$IN

# Events above this many bytes are replaced by a stub (messages are capped at 1 MiB).
MAX_EVENT=400000

# The device is opened once, read-write: a virtio port cannot be opened twice.
exec 3<>"$IN"
if [ "$OUT" = "$IN" ]; then exec 4>&3; else exec 4<>"$OUT"; fi

T=$(mktemp -d) || exit 1
FINISHED=0

send() { printf '%s\n' "$1" >&4; }

# Waits up to $1 seconds for a line from the app into $line. Before the app connects the port reads
# as end of file, so that is retried.
readline() {
	_end=$(($(date +%s) + $1))
	while :; do
		IFS= read -r line <&3 && return 0
		[ "$(date +%s)" -lt "$_end" ] || return 1
		sleep 1
	done
}

finish() {
	FINISHED=1
	rm -rf "$T"
	if [ "$POWEROFF" = 1 ]; then
		sleep 2 # let the last message leave
		poweroff
	fi
	exit 0
}

fail() {
	send "$(jq -nc --arg r "$1" '{kind:"final",body:{status:"failed",result:$r}}')"
	finish
}

# Safety net: whatever goes wrong, the app gets a final and the VM goes away.
trap '[ "$FINISHED" = 1 ] || fail "agentd stopped unexpectedly"' EXIT

readline 300 || fail "no bootstrap from the app"
TOKEN=$(printf '%s' "$line" | jq -r '.claude_token // empty') || fail "bad bootstrap"
AGENT_ID=$(printf '%s' "$line" | jq -r '.id // empty')
[ -n "$TOKEN" ] || fail "bootstrap without a token"

send '{"kind":"ready"}'
readline 300 || fail "no task from the app"
TASK=$line
field() { printf '%s' "$TASK" | jq -r ".body.$1 // empty"; }
PROMPT=$(field prompt)
SID=$(field session_id)
MODEL=$(field model)
EFFORT=$(field effort)
RESUME=$(field resume)
SCHEMA=$(field schema)
TIMEOUT=$(field timeout_secs)
[ -n "$PROMPT" ] && [ -n "$SID" ] && [ -n "$SCHEMA" ] || fail "bad task"
SYSTEM=$(field system_prompt)
# The profile's variables as `export NAME='value'` lines, for `claude` only. Names that are not
# plain identifiers or that agentd and the image own are dropped; values are shell-quoted by jq.
ENVSET=$(printf '%s' "$TASK" | jq -r '
	.body.env[]? | select((.name | type == "string") and (.name | test("^[A-Za-z_][A-Za-z0-9_]*$"))
		and ((.name | IN("CLAUDE_CODE_OAUTH_TOKEN", "PATH", "HOME", "USER", "LOGNAME", "SHELL", "USE_BUILTIN_RIPGREP", "CCAF_AGENT_ID")) | not)
		and (.name | startswith("LD_") | not)) |
	"export \(.name)=\(.value // "" | tostring | @sh)"') || fail "bad task environment"
case $TIMEOUT in '' | *[!0-9]*) TIMEOUT=1800 ;; esac

# The task brings no code and none is collected afterwards: the workspace is a scratch folder.
mkdir -p "$WS" || fail "workspace: cannot create $WS"
HOME_DIR=${HOME:-/root}
if [ -n "$RUN_AS" ]; then
	HOME_DIR=$(awk -F: -v u="$RUN_AS" '$1 == u { print $6 }' /etc/passwd)
	[ -n "$HOME_DIR" ] || fail "no user $RUN_AS"
	chown -R "$RUN_AS:$RUN_AS" "$WS" || fail "workspace: chown"
fi

set -- -p "$PROMPT" --output-format stream-json --verbose --include-partial-messages \
	--json-schema "$SCHEMA" --dangerously-skip-permissions
if [ "$RESUME" = true ]; then set -- "$@" --resume "$SID"; else set -- "$@" --session-id "$SID"; fi
[ -z "$MODEL" ] || set -- "$@" --model "$MODEL"
[ -z "$EFFORT" ] || set -- "$@" --effort "$EFFORT"
[ -z "$SYSTEM" ] || set -- "$@" --append-system-prompt "$SYSTEM"

# The token is exported only in the subshell that becomes `claude`, so it is in no other
# environment and on no command line. setsid gives `claude` and its children their own process
# group (the group id is the pid written to pid), which is how a timeout kills all of them.
run_claude() {
	# $$ is the main shell in a subshell; /proc/self is this one
	read -r pid _ </proc/self/stat
	echo "$pid" >"$T/pid"
	cd "$WS" || exit 126
	# the profile's variables first, so they cannot override what follows
	eval "$ENVSET"
	export CLAUDE_CODE_OAUTH_TOKEN="$TOKEN" HOME="$HOME_DIR" CCAF_AGENT_ID="$AGENT_ID"
	if [ -n "$RUN_AS" ]; then
		export USER="$RUN_AS" LOGNAME="$RUN_AS"
		exec setsid setpriv --reuid "$RUN_AS" --regid "$RUN_AS" --init-groups --no-new-privs "$CLAUDE" "$@"
	fi
	exec setsid "$CLAUDE" "$@"
}

(
	{
		(run_claude "$@") 2>"$T/stderr"
		echo $? >"$T/code"
	} | tee "$T/log" | jq -cR --unbuffered --argjson max "$MAX_EVENT" '
		select(length > 0) |
		{kind: "event", body: {seq: input_line_number, event:
			(if utf8bytelength > $max
			 then {truncated: true, size: utf8bytelength, head: .[0:4096]}
			 else (try fromjson catch {type: "raw", text: .}) end)}}' >&4
	: >"$T/done"
) &

START=$(date +%s)
TIMED_OUT=0
while [ ! -e "$T/done" ]; do
	if [ $(($(date +%s) - START)) -ge "$TIMEOUT" ]; then
		TIMED_OUT=1
		kill -s KILL -- "-$(cat "$T/pid" 2>/dev/null)" 2>/dev/null
		break
	fi
	sleep 1
done
# After a kill give the pipeline a moment to drain; a child that escaped its group is left behind.
n=0
while [ ! -e "$T/done" ] && [ $n -lt 10 ]; do
	sleep 1
	n=$((n + 1))
done

CODE=$(cat "$T/code" 2>/dev/null)
case $CODE in '' | *[!0-9]*) CODE=null ;; esac

RESULT=$(jq -cR 'fromjson? | select(type == "object" and .type == "result")' "$T/log" 2>/dev/null | tail -n 1)
if [ "$TIMED_OUT" = 1 ]; then
	REASON="timed out after ${TIMEOUT}s"
elif [ -z "$RESULT" ]; then
	REASON="claude ended without a result (exit $CODE): $(tail -c 2000 "$T/stderr" 2>/dev/null)"
fi

if [ -n "${REASON:-}" ]; then
	FINAL=$(jq -nc --arg r "$REASON" --arg sid "$SID" --argjson code "$CODE" '
		{kind: "final", body: {status: "failed", result: $r, session_id: $sid, exit_code: $code}}')
else
	# `claude` failing outranks what its answer claims.
	FINAL=$(printf '%s' "$RESULT" | jq -c --arg sid "$SID" --argjson code "$CODE" '
		(.structured_output | if type == "object" then . else {} end) as $s |
		{kind: "final", body: {
			status: (if .is_error == true then "failed"
				elif ($s.status | IN("done", "needs_input", "failed")) then $s.status
				else "done" end),
			result: (($s.summary | strings) // (.result | strings) // ""),
			questions: [($s.questions // [])[]? | strings],
			session_id: ((.session_id | strings) // $sid),
			cost_usd: ((.total_cost_usd | numbers) // null),
			output: (if (.structured_output | type) == "object" then .structured_output else null end),
			exit_code: $code}}')
fi
[ -n "$FINAL" ] || fail "could not build the final message"
send "$FINAL"
finish
