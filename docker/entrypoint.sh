#!/bin/sh
set -eu

state_root="${OBSCURA_GATEWAY_STATE_ROOT:-${HOME}/.obscura-gateway}"
config_file="${state_root}/config.toml"

mkdir -p "${state_root}"

if [ ! -f "${config_file}" ]; then
  obscura-gateway setup >/dev/null
fi

python3 - "$config_file" <<'PY'
import os
import sys
from pathlib import Path

path = Path(sys.argv[1])
raw = path.read_text()

updates = {
    "server_url": os.environ.get("OBSCURA_GATEWAY_SERVER_URL"),
    "api_key": os.environ.get("OBSCURA_GATEWAY_API_KEY"),
    "listen_addr": os.environ.get("OBSCURA_GATEWAY_LISTEN_ADDR"),
    "obscura_bin": os.environ.get("OBSCURA_GATEWAY_OBSCURA_BIN"),
    "default_proxy_policy": os.environ.get("OBSCURA_GATEWAY_DEFAULT_PROXY_POLICY"),
}

lines = raw.splitlines()
seen = set()

for i, line in enumerate(lines):
    stripped = line.strip()
    if not stripped or stripped.startswith("#") or "=" not in stripped:
        continue
    key = stripped.split("=", 1)[0].strip()
    if key in updates and updates[key]:
        value = updates[key].replace("\\", "\\\\").replace('"', '\\"')
        lines[i] = f'{key} = "{value}"'
        seen.add(key)

insert_at = 0
while insert_at < len(lines) and (
    not lines[insert_at].strip() or not lines[insert_at].lstrip().startswith("[")
):
    insert_at += 1

for key, value in updates.items():
    if value and key not in seen:
        escaped = value.replace("\\", "\\\\").replace('"', '\\"')
        lines.insert(insert_at, f'{key} = "{escaped}"')
        insert_at += 1

path.write_text("\n".join(lines) + "\n")
PY

exec obscura-gateway "$@"
