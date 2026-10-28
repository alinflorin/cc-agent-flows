# jtk (Jira CLI) from open-cli-collective/atlassian-cli: a static Go binary. The repo holds more than
# one tool, so there is no "latest" release: the newest jtk-v* tag is used. It reads ATLASSIAN_URL,
# ATLASSIAN_EMAIL and JIRA_API_TOKEN from the environment (set by the agent profile), no config file.
apk add --no-cache --no-progress ca-certificates
case "$(uname -m)" in
  x86_64) arch=amd64 ;;
  aarch64) arch=arm64 ;;
  *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
tool=jtk
tag=$(wget -qO- "https://api.github.com/repos/open-cli-collective/atlassian-cli/releases?per_page=100" | grep -o "\"tag_name\": *\"$tool-v[^\"]*\"" | head -n 1 | sed "s/.*\"\\($tool-v[^\"]*\\)\"/\\1/")
[ -n "$tag" ] || { echo "could not find a $tool release" >&2; exit 1; }
wget -qO "/tmp/$tool.tgz" "https://github.com/open-cli-collective/atlassian-cli/releases/download/${tag}/${tool}_${tag#"$tool"-v}_linux_${arch}.tar.gz"
mkdir "/tmp/$tool"
tar -xzf "/tmp/$tool.tgz" -C "/tmp/$tool"
install -m 755 "$(find "/tmp/$tool" -type f -name "$tool" | head -n 1)" "/usr/local/bin/$tool"
rm -rf "/tmp/$tool" "/tmp/$tool.tgz"
"$tool" --version
