#!/bin/sh
# agentd: the program inside each agent VM. One task per boot: it reads its bootstrap and task from
# the app over the virtio-serial port, runs `claude` on the task as an unprivileged user, streams
# what `claude` prints and ends with one `final` message. Messages are one JSON object per line
# (src/protocol.rs). Needs busybox ash, jq, setpriv (util-linux) and setsid.
#
#   agentd.sh [--channel <path>] [--out <path>] [--workspace <dir>] [--claude <path>]
#             [--ssh <path>] [--ca-bundle <path>] [--ca-file <path>] [--tools-dir <dir>] [--run-as <user>] [--poweroff]
#
# --out is only for tests: with it the channel is split into two files (fifos). Everything
# `claude` prints is untrusted data: wrapped by jq, truncated, never run.
set -u

IN=/dev/virtio-ports/agent
OUT=
WS=/home/agent/work
CLAUDE=claude
SSH=ssh
CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt
CA_FILE=/etc/ccaf-ca.pem
TOOLS_DIR=/usr/share/ccaf/tools
RUN_AS=
POWEROFF=0
while [ $# -gt 0 ]; do
	case $1 in
	--channel) IN=$2; shift ;;
	--out) OUT=$2; shift ;;
	--workspace) WS=$2; shift ;;
	--claude) CLAUDE=$2; shift ;;
	--ssh) SSH=$2; shift ;;
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

readline 300 || fail "no bootstrap from the app"
TOKEN=$(printf '%s' "$line" | jq -r '.claude_token // empty') || fail "bad bootstrap"
AGENT_ID=$(printf '%s' "$line" | jq -r '.id // empty')
[ -n "$TOKEN" ] || fail "bootstrap without a token"
# With a sealed network the app sends the keys of the tunnel to its egress proxy.
TUNNEL_KEY=$(printf '%s' "$line" | jq -r '.tunnel.key // empty')
TUNNEL_HOST=$(printf '%s' "$line" | jq -r '.tunnel.host_key // empty')
# The CA that signs the certificates of the hosts the app injects credentials for (public).
CA=$(printf '%s' "$line" | jq -r '.ca // empty')

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

# The only way out of the VM is one connection to the app (QEMU forwards 10.0.2.100:22 to it).
# ssh carries the traffic of `claude` to the app's proxy as channels: HTTPS_PROXY points at the
# local end. The key is read by ssh as root and deleted at once; `claude` never sees it.
if [ -n "$TUNNEL_KEY" ]; then
	[ -n "$TUNNEL_HOST" ] || fail "tunnel: no host key"
	(
		umask 077
		printf '%s\n' "$TUNNEL_KEY" >"$T/tunnel.key"
		printf '10.0.2.100 %s\n' "$TUNNEL_HOST" >"$T/tunnel.hosts"
	) || fail "tunnel: cannot write the key"
	# -f returns once the tunnel is up (or ssh failed), in the background it keeps running
	timeout 90 "$SSH" -F /dev/null -f -N -T -i "$T/tunnel.key" -o IdentitiesOnly=yes -o BatchMode=yes \
		-o UserKnownHostsFile="$T/tunnel.hosts" -o StrictHostKeyChecking=yes -o ExitOnForwardFailure=yes \
		-o ConnectTimeout=30 -o ServerAliveInterval=30 -o ServerAliveCountMax=4 \
		-L 127.0.0.1:3128:egress:3128 -p 22 agent@10.0.2.100 2>"$T/ssh.err"
	_rc=$?
	rm -f "$T/tunnel.key"
	[ "$_rc" = 0 ] || fail "tunnel: cannot reach the network proxy (ssh exit $_rc): $(tail -c 500 "$T/ssh.err" 2>/dev/null)"
fi

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

set -- -p "$PROMPT" --output-format stream-json --verbose --include-partial-messages \
	--json-schema "$SCHEMA" --dangerously-skip-permissions
if [ "$RESUME" = true ]; then set -- "$@" --resume "$SID"; else set -- "$@" --session-id "$SID"; fi
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
	if [ -n "$CA" ]; then
		export NODE_EXTRA_CA_CERTS=$CA_FILE REQUESTS_CA_BUNDLE=$CA_BUNDLE
	fi
	if [ -n "$TUNNEL_KEY" ]; then
		export HTTPS_PROXY=http://127.0.0.1:3128 HTTP_PROXY=http://127.0.0.1:3128 NO_PROXY=localhost,127.0.0.1,::1
		export https_proxy=$HTTPS_PROXY http_proxy=$HTTP_PROXY no_proxy=$NO_PROXY
	fi
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
			tokens: (if (.usage | type) == "object" then ([.usage.input_tokens, .usage.cache_creation_input_tokens, .usage.cache_read_input_tokens, .usage.output_tokens] | map(numbers) | add) else null end),
			output: (if (.structured_output | type) == "object" then .structured_output else null end),
			exit_code: $code}}')
fi
[ -n "$FINAL" ] || fail "could not build the final message"
send "$FINAL"
finish
