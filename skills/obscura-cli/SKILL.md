---
name: obscura-cli
description: "Use this skill when a task involves installing, configuring, or using the obscura-cli client: connecting to local or remote Obscura Gateway servers, setting server URLs/API keys/default stealth, creating and closing sessions, controlling session/profile stealth, managing browser fingerprint identity fields, navigating/evaluating/dumping pages, managing profiles, importing/exporting cookies, issuing CDP grants, setting proxy/domain policies through the CLI, or giving agents command examples for browser automation workflows."
---

# Obscura CLI

## Use This Skill For

`obscura-cli` is the first-class command-line client for Obscura Gateway. It configures local client state and sends authenticated HTTP requests to the gateway API.

Use this skill for agent/browser workflows: sessions, profiles, cookies, stealth/fingerprint controls, CDP grants, proxy policies, domain policies, remote gateway setup, and CLI troubleshooting.

For server deployment, Docker, gateway process lifecycle, API implementation, and release packaging, use the `obscura-gateway` skill instead.

## Fast Start

Point the CLI at a gateway:

```bash
obscura-cli config set-server-url http://127.0.0.1:18789
obscura-cli config set-api-key <gateway_api_key>
obscura-cli config set-default-stealth true
```

Create and use an ephemeral browser session:

```bash
obscura-cli session create
obscura-cli session navigate <session_id> https://example.com/
obscura-cli session eval <session_id> "document.title"
obscura-cli session dump <session_id> --format text
obscura-cli session close <session_id>
```

Override stealth per session when needed:

```bash
obscura-cli session create --no-stealth
obscura-cli session create --stealth
```

From source, replace `obscura-cli` with:

```bash
cargo run --bin obscura-cli --
```

Always close sessions when finished. Sessions are ephemeral and backed by live gateway-owned `obscura` child processes.

## Core Rules

- `server_url` is what the CLI calls and what CDP grants use for public URLs.
- `api_key` must match the gateway API key.
- Session IDs refer to live gateway runtimes; after gateway restart, create a new session.
- Prefer `session navigate`, `session eval`, and `session dump` over raw CDP unless the tool specifically needs CDP.
- Use profiles for persistent identity/cookies; use ephemeral direct sessions for one-off browsing.
- Stealth defaults on unless overridden by gateway config, profile settings, or session flags.
- Session `--stealth` and `--no-stealth` override gateway/profile defaults for that session only.
- Profile `--stealth` and `--no-stealth` are persistent overrides; omit them to inherit the gateway default.
- Profile sessions fill missing identity fields with the built-in Chrome 145/macOS fingerprint defaults.
- Effective profile user agents are passed to `obscura serve --user-agent`; timezone and viewport are applied through CDP emulation.
- Do not import cookies into a profile while it has active sessions.

## Reference Files

Read only the reference that matches the task:

- [CLI operations](references/cli.md): install, setup, remote config, status, quotas, common commands, and source equivalents.
- [Sessions, profiles, and cookies](references/sessions-profiles-cookies.md): lifecycle rules, profile modes, stealth/fingerprint controls, identity fields, cookie formats, and persistence gotchas.
- [CLI CDP, proxies, and troubleshooting](references/cdp-proxy-troubleshooting.md): CDP grant commands, proxy policy commands, domain policy commands, and diagnostic workflow.
