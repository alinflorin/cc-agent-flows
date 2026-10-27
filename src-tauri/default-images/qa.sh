# Playwright's own browser downloads are glibc builds, so Alpine's chromium is used.
# --no-sandbox because agents run as an unprivileged user inside the VM.
apk add --no-cache --no-progress nodejs npm chromium font-noto
npm install -g @playwright/cli
cat > /etc/playwright-cli.json <<'JSON'
{
  "browser": {
    "browserName": "chromium",
    "launchOptions": {
      "executablePath": "/usr/bin/chromium",
      "headless": true,
      "args": ["--no-sandbox", "--disable-dev-shm-usage"]
    }
  }
}
JSON
playwright-cli --help >/dev/null
