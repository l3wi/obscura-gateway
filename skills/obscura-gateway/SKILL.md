---
name: obscura-gateway
description: "Use this skill when a task involves operating or changing the Obscura Gateway server: deploying or running the gateway, Dockerizing it, configuring listen/server URLs, API keys, Obscura binary paths, state directories, default stealth, browser fingerprint defaults, API behavior, child Obscura process lifecycle, server-side quotas, release packaging, or gateway/server troubleshooting. Do not use this for routine obscura-cli session/profile/cookie commands unless server behavior is being debugged."
---

# Obscura Gateway

## Use This Skill For

Obscura Gateway is the long-running control plane around the `obscura` browser binary. It owns state, exposes the HTTP API, enforces policy, and starts/stops short-lived `obscura serve` child processes.

Use this skill when the task is about the server process, deployment, Docker image, HTTP API, state files, process lifecycle, default stealth/fingerprint behavior, or release packaging.

For day-to-day client workflows such as `obscura-cli session create`, profile management, cookie import/export, and CDP grant commands, use the `obscura-cli` skill instead.

## Fast Start

From the repo root:

```bash
cargo run --bin obscura-gateway -- setup
cargo run --bin obscura-gateway -- run
```

Docker gateway:

```bash
docker compose up --build
docker compose exec obscura-gateway sh -c "awk -F'\"' '/^api_key =/{print \$2}' /data/.obscura-gateway/config.toml"
```

Default local API: `http://127.0.0.1:18789`.

Always verify `obscura` is installed or configured before diagnosing gateway failures.

## Server Rules

- `listen_addr` is where the gateway binds.
- `server_url` is what clients call and what CDP grants use for public URLs.
- `api_key` protects `/v1` routes with bearer auth.
- Local state defaults to `~/.obscura-gateway`.
- Docker state lives at `/data/.obscura-gateway`.
- `default_stealth` defaults to `true` and is inherited by sessions unless a profile or session override is provided.
- Effective stealth launches child Obscura with `--stealth`.
- Effective profile user agents launch child Obscura with `--user-agent`.
- Profile sessions fill missing identity fields with the built-in Chrome 145/macOS fingerprint defaults.
- Active sessions are live child processes; persisted DB rows cannot recover them after restart.
- A gateway restart marks previously active sessions `failed`.
- Child CDP ports stay on loopback and are proxied or granted by the gateway.

## Reference Files

Read only the reference that matches the task:

- [Gateway operations](references/gateway-operations.md): setup, Docker, runtime configuration, state layout, API/auth, stealth/fingerprint behavior, releases, and tests.
- [Gateway troubleshooting](references/gateway-troubleshooting.md): stale sessions, Obscura binary failures, proxy/CDP server issues, and diagnostic workflow.

## Validation

- Run `cargo test` after changing gateway code.
- Run `cargo build --release --locked --bins` after changing release packaging or binary layout.
- Run `docker build -t obscura-gateway:local .` after changing Docker files.
