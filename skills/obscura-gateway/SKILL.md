---
name: obscura-gateway
description: "Use this skill when a task involves obscura-gateway browser automation or operations: starting or configuring the gateway, using the CLI/API, creating ephemeral browser sessions, managing persistent profiles, importing or exporting cookies, issuing CDP grants, setting proxy/domain policies, running smoke tests, or troubleshooting session/profile/cookie failures."
---

# Obscura Gateway

## Use This Skill For

Obscura Gateway is a control plane around the `obscura` browser binary. It gives agents short-lived browser sessions, optional persistent profiles, cookie import/export, domain controls, proxy policies, and one-time CDP WebSocket grants.

Use the gateway primitives rather than ad-hoc browser automation when the user needs controlled browsing, repeatable session lifecycle, logged-in profile state, cookie handling, proxy selection, or raw CDP access.

## Fast Start

From the repo root:

```bash
cargo run --bin obscura-gateway -- setup
cargo run --bin obscura-gateway -- run
```

In another shell:

```bash
obscura-cli status
obscura-cli quotas
obscura-cli session create
obscura-cli session navigate <session_id> https://example.com/
obscura-cli session eval <session_id> "document.title"
obscura-cli session dump <session_id> --format text
obscura-cli session close <session_id>
```

From source, replace `obscura-cli` with `cargo run --bin obscura-cli --`.

Always close sessions when finished. Sessions are ephemeral and backed by live child `obscura` processes.

## Choose The Right Primitive

- Use an ephemeral session for stateless browsing, page inspection, and one-off automation.
- Use a profile when the task needs persistent identity or cookies across sessions.
- Use cookie import/export when the user provides browser state or needs a reusable artifact.
- Use a CDP grant only when an external agent/tool needs temporary raw WebSocket access.
- Use proxy policies when location, egress, or traffic separation matters.

## Reference Files

Read only the reference that matches the task:

- [CLI and operations](references/cli.md): setup, run, remote CLI config, status, quotas, tests, and live smoke commands.
- [Sessions, profiles, and cookies](references/sessions-profiles-cookies.md): lifecycle rules, profile modes, identity fields, cookie formats, and persistence gotchas.
- [CDP, proxies, and troubleshooting](references/cdp-proxy-troubleshooting.md): grants, proxy policy commands, stale sessions, failures, and diagnostic workflow.

## Core Rules

- `server_url` is what CLI clients call and what CDP grants use for public URLs.
- `listen_addr` is where the gateway binds; changing `server_url` alone does not move the listener.
- Stored sessions are history plus status, not recoverable browser runtimes.
- A gateway restart marks previously active sessions failed; create new sessions after restart.
- Prefer `session navigate`, `session eval`, and `session dump` over raw CDP unless raw CDP is required.
- Run `cargo test` after changing gateway code.
