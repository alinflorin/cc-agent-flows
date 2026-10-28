apk add --no-cache --no-progress git github-cli python3 nodejs npm
# --system, not --global: agents run as an unprivileged user, root's ~/.gitconfig would not reach them.
git config --system user.name "CC Agent"
git config --system user.email "agent@cc-agent-flows.local"
git config --system init.defaultBranch main
