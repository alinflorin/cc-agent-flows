#!/usr/bin/env python3
"""Rebuild latest.json of a release from its assets.

The release jobs run in parallel and each one read-modify-writes latest.json, so
a platform can get lost (darwin-x86_64 and windows did). Usage:
fix-latest-json.py <owner/repo> <tag>   (needs `gh`, writes latest.json)."""
import json, subprocess, sys, urllib.request

repo, tag = sys.argv[1], sys.argv[2]
rel = json.loads(subprocess.check_output(
    ["gh", "api", f"repos/{repo}/releases/tags/{tag}"]))
assets = {a["name"]: a for a in rel["assets"]}
old = {}
if "latest.json" in assets:
    with urllib.request.urlopen(assets["latest.json"]["browser_download_url"]) as r:
        old = json.load(r)

# (file suffix, platform keys); the updater bundle must have a .sig next to it
RULES = [
    ("_aarch64.app.tar.gz", ["darwin-aarch64", "darwin-aarch64-app"]),
    ("_x64.app.tar.gz", ["darwin-x86_64", "darwin-x86_64-app"]),
    ("_amd64.AppImage", ["linux-x86_64", "linux-x86_64-appimage"]),
    ("_aarch64.AppImage", ["linux-aarch64", "linux-aarch64-appimage"]),
    ("_x64-setup.exe", ["windows-x86_64", "windows-x86_64-nsis"]),
]
platforms = dict(old.get("platforms", {}))
for name, a in assets.items():
    for suffix, keys in RULES:
        if name.endswith(suffix) and name + ".sig" in assets:
            with urllib.request.urlopen(assets[name + ".sig"]["browser_download_url"]) as r:
                sig = r.read().decode().strip()
            for k in keys:
                platforms[k] = {"signature": sig, "url": a["browser_download_url"]}
out = {
    "version": old.get("version") or tag.lstrip("v"),
    "notes": old.get("notes", ""),
    "pub_date": old.get("pub_date") or rel["published_at"],
    "platforms": platforms,
}
json.dump(out, open("latest.json", "w"), indent=2)
print("platforms:", ", ".join(sorted(platforms)))
