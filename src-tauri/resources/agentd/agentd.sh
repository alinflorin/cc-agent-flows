#!/bin/sh
# agentd: the program inside each agent VM. One task per boot: it reads its bootstrap and task from
# the app over the virtio-serial port, runs `claude` on the task as an unprivileged user, streams
# what `claude` prints and ends with one `final` message. Messages are one JSON object per line
# (src/protocol.rs). Needs busybox ash, jq, setpriv (util-linux) and setsid.
#
#   agentd.sh [--channel <path>] [--out <path>] [--workspace <dir>] [--claude <path>]
#             [--tunnel-fifo <path>] [--ca-bundle <path>] [--ca-file <path>] [--tools-dir <dir>] [--run-as <user>] [--poweroff]
#
# --out is only for tests: with it the channel is split into two files (fifos). Everything
# `claude` prints is untrusted data: wrapped by jq, truncated, never run.
set -u

IN=/dev/virtio-ports/agent
OUT=
WS=/home/agent/work
CLAUDE=claude
TUNNEL_FIFO=/run/ccaf/tunnel
CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt
CA_FILE=/etc/ccaf-ca.pem
TOOLS_DIR=/usr/share/ccaf/tools
# The only tools `claude` gets: the rest (subagents, cron, worktrees, skills...) are useless in a
# throwaway VM and cost prompt tokens. The web tools still go through the egress allowlist.
CLAUDE_TOOLS="Bash,Read,Write,Edit,WebFetch,WebSearch"
RUN_AS=
POWEROFF=0
while [ $# -gt 0 ]; do
	case $1 in
	--channel) IN=$2; shift ;;
	--out) OUT=$2; shift ;;
	--workspace) WS=$2; shift ;;
	--claude) CLAUDE=$2; shift ;;
	--tunnel-fifo) TUNNEL_FIFO=$2; shift ;;
	--ca-bundle) CA_BUNDLE=$2; shift ;;
	--ca-file) CA_FILE=$2; shift ;;
	--tools-dir) TOOLS_DIR=$2; shift ;;
	--run-as) RUN_AS=$2; shift ;;
	--poweroff) POWEROFF=1 ;;
	*) echo "agentd: unknown argument $1" >&2; exit 2 ;;
	esac
	shift
done
[ -n "$OUT" ] || OUT=$IN

# Events above this many bytes are replaced by a stub (messages are capped at 1 MiB).
MAX_EVENT=900000

# The device is opened once, read-write: a virtio port cannot be opened twice.
exec 3<>"$IN"
if [ "$OUT" = "$IN" ]; then exec 4>&3; else exec 4<>"$OUT"; fi

T=$(mktemp -d) || exit 1
FINISHED=0

send() { printf '%s\n' "$1" >&4; }

# Where the time of a run goes: an event (seq 0, ignored by the UI, kept in the run log) with the
# guest's uptime in seconds, so the boot shows next to what agentd did afterwards.
mark() {
	_up=${2:-}
	[ -n "$_up" ] || read -r _up _ </proc/uptime
	send "{\"kind\":\"event\",\"body\":{\"seq\":0,\"event\":{\"type\":\"timing\",\"what\":\"$1\",\"uptime\":${_up:-0}}}}"
}

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
		sleep 1 # let the last message leave
		# the disk is a throwaway overlay: no orderly shutdown, power off at once
		poweroff -f
	fi
	exit 0
}

fail() {
	send "$(jq -nc --arg r "$1" '{kind:"final",body:{status:"failed",result:$r}}')"
	finish
}

# Safety net: whatever goes wrong, the app gets a final and the VM goes away.
trap '[ "$FINISHED" = 1 ] || fail "agentd stopped unexpectedly"' EXIT

mark agentd_up
readline 300 || fail "no bootstrap from the app"
mark bootstrap
# One jq for the whole bootstrap (each spawn costs a lot under software emulation); the values come
# back shell-quoted by jq (@sh), so evaluating them cannot run anything.
_b=$(printf '%s' "$line" | jq -r '
	"TOKEN=\(.claude_token // "" | tostring | @sh)",
	"AGENT_ID=\(.id // "" | tostring | @sh)",
	"TUNNEL=\(.tunnel // false | tostring | @sh)",
	"CA=\(.ca // "" | tostring | @sh)"') || fail "bad bootstrap"
eval "$_b"
[ -n "$TOKEN" ] || fail "bootstrap without a token"
# With a sealed network the app says so (`tunnel`): `ccaf-init` has already opened the tunnel to the
# app's egress proxy (no key: the channel belongs to this VM) and reports how it went through a fifo,
# read just before `claude` starts. The CA signs the certificates of the hosts the app injects
# credentials for (public).

send '{"kind":"ready"}'
readline 300 || fail "no task from the app"
mark task
TASK=$line
# One jq for the whole task. The profile's variables become `export NAME='value'` lines, for
# `claude` only: names that are not plain identifiers or that agentd and the image own are dropped,
# values are shell-quoted by jq.
_t=$(printf '%s' "$TASK" | jq -r '
	.body as $b |
	([$b.env[]? | select((.name | type == "string") and (.name | test("^[A-Za-z_][A-Za-z0-9_]*$"))
		and ((.name | IN("CLAUDE_CODE_OAUTH_TOKEN", "PATH", "HOME", "USER", "LOGNAME", "SHELL", "USE_BUILTIN_RIPGREP", "CCAF_AGENT_ID")) | not)
		and (.name | startswith("LD_") | not)) |
		"export \(.name)=\(.value // "" | tostring | @sh)"] | join("\n")) as $env |
	($b.schema // "" | tostring) as $schema |
	"PROMPT=\($b.prompt // "" | tostring | @sh)",
	"SID=\($b.session_id // "" | tostring | @sh)",
	"MODEL=\($b.model // "" | tostring | @sh)",
	"EFFORT=\($b.effort // "" | tostring | @sh)",
	"RESUME=\($b.resume // "" | tostring | @sh)",
	"SCHEMA=\($schema | @sh)",
	"TIMEOUT=\($b.timeout_secs // "" | tostring | @sh)",
	"SYSTEM=\($b.system_prompt // "" | tostring | @sh)",
	"DEVICE=\($b.device_id // "" | tostring | @sh)",
	"ENVSET=\($env | @sh)",
	"KEYS=\(try ($schema | fromjson | (.properties // {}) | keys | join(", ")) catch "" | @sh)"') || fail "bad task"
eval "$_t"
[ -n "$PROMPT" ] && [ -n "$SID" ] && [ -n "$SCHEMA" ] || fail "bad task"
case $TIMEOUT in '' | *[!0-9]*) TIMEOUT=1800 ;; esac

# Make the VM trust that CA: appended to the system bundle (curl, git, Go and most others read it)
# and kept in a file for Node (NODE_EXTRA_CA_CERTS) and Python requests. It is a public certificate.
if [ -n "$CA" ]; then
	{
		printf '%s\n' "$CA" >"$CA_FILE" && chmod 644 "$CA_FILE" &&
			{ [ ! -s "$CA_BUNDLE" ] || [ "$(tail -c1 "$CA_BUNDLE" | wc -l)" -gt 0 ] || echo >>"$CA_BUNDLE"; } &&
			cat "$CA_FILE" >>"$CA_BUNDLE"
	} || fail "ca: cannot install the certificate"
fi

# The task brings no code and none is collected afterwards: the workspace is a scratch folder.
mkdir -p "$WS" || fail "workspace: cannot create $WS"
HOME_DIR=${HOME:-/root}
if [ -n "$RUN_AS" ]; then
	HOME_DIR=$(awk -F: -v u="$RUN_AS" '$1 == u { print $6 }' /etc/passwd)
	[ -n "$HOME_DIR" ] || fail "no user $RUN_AS"
	chown -R "$RUN_AS:$RUN_AS" "$WS" || fail "workspace: chown"
fi

# The device id Claude Code reports (`userID`), stable across runs: without one each VM makes its own
# and shows up in the account's session list as a new device. Only 64 hex characters are taken.
case $DEVICE in
	*[!0-9a-f]*) ;;
	????????????????????????????????????????????????????????????????)
		_cj=$HOME_DIR/.claude.json
		[ -f "$_cj" ] || echo '{}' >"$_cj"
		jq --arg u "$DEVICE" '.userID = $u' "$_cj" >"$_cj.new" && mv "$_cj.new" "$_cj"
		[ -z "$RUN_AS" ] || chown "$RUN_AS:$RUN_AS" "$_cj"
		;;
esac

# No `--include-partial-messages`: the token-by-token stream is a flood of events (each one parsed
# and wrapped by jq in the guest, sent over the channel, passed to the UI and written to the run
# log) that nothing shows; the session log has always dropped it and the views read whole messages.
set -- -p "$PROMPT" --output-format stream-json --verbose \
	--dangerously-skip-permissions --tools "$CLAUDE_TOOLS" --disable-slash-commands --strict-mcp-config
if [ "$RESUME" = true ]; then set -- "$@" --resume "$SID"; else set -- "$@" --session-id "$SID" --no-session-persistence; fi
[ -z "$MODEL" ] || set -- "$@" --model "$MODEL"
[ -z "$EFFORT" ] || set -- "$@" --effort "$EFFORT"
# The image's notes on its command-line tools (`*.md` written when it was built, see images.rs),
# at the end of the system prompt. Capped: the prompt is an argument.
TOOLS=$(for f in "$TOOLS_DIR"/*.md; do
	[ -f "$f" ] && cat "$f"
done 2>/dev/null | head -c 40000)
if [ -n "$TOOLS" ]; then
	SYSTEM="${SYSTEM:+$SYSTEM

}# Command-line tools in this environment

$TOOLS"
fi
# No `--json-schema`: it adds a StructuredOutput tool and a model turn. The answer is asked for in
# the prompt instead and read from the end of `claude`'s last message (see the final below).
SYSTEM="${SYSTEM:+$SYSTEM

}# Final answer

End your last message with one JSON object, as plain text (no code fence, nothing after it): your answer, with a real value for each of the keys the schema below lists ($KEYS). Print the answer, never the schema itself, and use the exact keys and allowed values the schema gives:
$SCHEMA"
set -- "$@" --append-system-prompt "$SYSTEM"

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
	# less to do at start: no update check, telemetry or error reports, no CLAUDE.md search, no
	# auto-memory (--bare would skip these too but never reads the OAuth token)
	export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 DISABLE_AUTOUPDATER=1 DISABLE_TELEMETRY=1 DISABLE_ERROR_REPORTING=1 \
		CLAUDE_CODE_DISABLE_CLAUDE_MDS=1 CLAUDE_CODE_DISABLE_AUTO_MEMORY=1
	if [ -n "$CA" ]; then
		export NODE_EXTRA_CA_CERTS=$CA_FILE REQUESTS_CA_BUNDLE=$CA_BUNDLE
	fi
	if [ "$TUNNEL" = true ]; then
		export HTTPS_PROXY=http://127.0.0.1:3128 HTTP_PROXY=http://127.0.0.1:3128 NO_PROXY=localhost,127.0.0.1,::1
		export https_proxy=$HTTPS_PROXY http_proxy=$HTTP_PROXY no_proxy=$NO_PROXY
	fi
	if [ -n "$RUN_AS" ]; then
		export USER="$RUN_AS" LOGNAME="$RUN_AS"
		exec setsid setpriv --reuid "$RUN_AS" --regid "$RUN_AS" --init-groups --no-new-privs "$CLAUDE" "$@"
	fi
	exec setsid "$CLAUDE" "$@"
}

# `ccaf-init` started the tunnel at boot; it has been coming up since. `claude` needs it.
if [ "$TUNNEL" = true ]; then
	read -r _rc _tu <"$TUNNEL_FIFO" 2>/dev/null
	[ "${_rc:-1}" = 0 ] || fail "tunnel: cannot reach the network proxy (relay exit ${_rc:-?}): $(tail -c 500 "$TUNNEL_FIFO.err" 2>/dev/null)"
fi

mark tunnel_up "${_tu:-}"

mark claude_start
(
	{
		(run_claude "$@") 2>"$T/stderr"
		echo $? >"$T/code"
	} | tee "$T/log" | jq -cR --unbuffered --argjson max "$MAX_EVENT" '
		select(length > 0) |
		{kind: "event", body: {seq: input_line_number, event:
			(if utf8bytelength > $max
			 then {truncated: true, size: utf8bytelength, head: .[0:4096]}
			 else (. as $l | try fromjson catch {type: "raw", text: $l}) end)}}' >&4
	: >"$T/done"
) &
PIPE=$!

# The deadline: a watcher kills claude's process group when it passes, and the pipeline (in case a
# child escaped the group and keeps it open) ten seconds later. Waiting for the pipeline with
# `wait` ends the run the moment claude does, with no polling.
TIMED_OUT=0
(
	sleep "$TIMEOUT" &
	echo $! >"$T/sleep"
	wait
	: >"$T/timedout"
	kill -s KILL -- "-$(cat "$T/pid" 2>/dev/null)" 2>/dev/null
	sleep 10
	kill -s KILL "$PIPE" 2>/dev/null
) &
WATCHER=$!
wait "$PIPE"
kill "$WATCHER" "$(cat "$T/sleep" 2>/dev/null)" 2>/dev/null
[ -e "$T/timedout" ] && TIMED_OUT=1

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
		# The answer is the last JSON object in the reply: the whole text, a fenced block, or the span
		# from the first "{", or from the last line starting with "{", to the last "}". Anything else is plain text, reported as done.
		((.result // "" | strings) as $t |
			([$t, ($t | capture("```(?:json)?\\s*(?<j>\\{[^`]*\\})\\s*```"; "g") | .j), ($t | sub("^[^{]*(?=\\{)"; "") | sub("(?<=\\})[^}]*$"; "")), ($t | sub("^(?s:.*\\n)(?=\\{)"; "") | sub("(?<=\\})[^}]*$"; ""))]
				| map(try fromjson catch null | select(type == "object" and (has("properties") | not))) | last // {})) as $s |
		{kind: "final", body: {
			status: (if .is_error == true then "failed"
				elif ($s.status | IN("done", "needs_input", "failed")) then $s.status
				else "done" end),
			result: (($s.summary | strings) // (.result | strings) // ""),
			questions: [($s.questions // [])[]? | strings],
			session_id: ((.session_id | strings) // $sid),
			tokens: (if (.usage | type) == "object" then ([.usage.input_tokens, .usage.cache_creation_input_tokens, .usage.cache_read_input_tokens, .usage.output_tokens] | map(numbers) | add) else null end),
			output: (if $s == {} then null else $s end),
			exit_code: $code}}')
fi
[ -n "$FINAL" ] || fail "could not build the final message"
send "$FINAL"
finish
