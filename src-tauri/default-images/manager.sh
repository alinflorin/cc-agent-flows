# jira-cli (ankitpokhrel/jira-cli): a static Go binary, taken from the latest GitHub release.
apk add --no-cache --no-progress ca-certificates
case "$(uname -m)" in
  x86_64) arch=x86_64 ;;
  aarch64) arch=arm64 ;;
  *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
ver=$(wget -qO- https://api.github.com/repos/ankitpokhrel/jira-cli/releases/latest | sed -n 's/.*"tag_name": *"v\([^"]*\)".*/\1/p')
[ -n "$ver" ] || { echo "could not find the latest jira-cli release" >&2; exit 1; }
wget -qO /tmp/jira.tgz "https://github.com/ankitpokhrel/jira-cli/releases/download/v${ver}/jira_${ver}_linux_${arch}.tar.gz"
mkdir /tmp/jira
tar -xzf /tmp/jira.tgz -C /tmp/jira
install -m 755 "$(find /tmp/jira -type f -name jira | head -n 1)" /usr/local/bin/jira
rm -rf /tmp/jira /tmp/jira.tgz
jira version
