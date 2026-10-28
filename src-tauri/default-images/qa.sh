# agent-browser (vercel-labs): a static native binary, no Node. Its own Chrome download is a glibc
# build, so Alpine's chromium is used (AGENT_BROWSER_EXECUTABLE_PATH and AGENT_BROWSER_ARGS are baked
# into the image's environment). --no-sandbox because agents run as an unprivileged user inside the VM.
apk add --no-cache --no-progress chromium font-noto ca-certificates
case "$(uname -m)" in
  x86_64) arch=x64 ;;
  aarch64) arch=arm64 ;;
  *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
wget -qO /usr/local/bin/agent-browser "https://github.com/vercel-labs/agent-browser/releases/latest/download/agent-browser-linux-musl-${arch}"
chmod 755 /usr/local/bin/agent-browser
agent-browser --version
