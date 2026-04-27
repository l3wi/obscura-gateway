# Obscura Gateway

Obscura Gateway is a control plane and CLI for running short-lived [Obscura](https://github.com/h4ckf0r0day/obscura) browser sessions. It is designed for agents and automation systems that need controlled browsing, persistent profile state, cookie import/export, proxy selection, and temporary CDP WebSocket access without exposing every browser process directly.

The gateway runs an HTTP API, stores state in SQLite, and spawns `obscura serve` child processes for active sessions. Child CDP ports stay on loopback and are accessed through gateway actions or one-time CDP grants.

## Features

- Ephemeral browser sessions with explicit create, navigate, evaluate, dump, and close operations.
- Persistent profiles with read-only and read-write modes for reusable identity and cookie state.
- Cookie import/export in JSON and Netscape formats.
- Domain allow/deny policy enforcement for navigation.
- Named proxy policies with direct/proxied session selection and profile proxy affinity.
- One-time, expiring CDP WebSocket grants for tools that need raw browser protocol access.
- Server status, quotas, session events, artifacts, and OpenAPI JSON.
- Docker image that bundles the pinned upstream Obscura release binary.
- Agent Skill metadata under `skills/obscura-gateway` for Agent Skills-compatible clients.

## Quickstart

### Docker

Docker is the easiest way to run the gateway because the image downloads and includes the pinned Obscura release binary.

```bash
docker compose up --build
```

The compose file exposes the gateway at:

```text
http://localhost:18789
```

If you do not set `OBSCURA_GATEWAY_API_KEY`, setup generates one in the persisted state volume. Read it with:

```bash
docker compose exec obscura-gateway sh -c "awk -F'\"' '/^api_key =/{print \$2}' /data/.obscura-gateway/config.toml"
```

See [DOCKER.md](DOCKER.md) for image build arguments, runtime environment variables, and reverse-proxy notes.

### From Source

Prerequisites:

- Rust toolchain with Cargo.
- `obscura` binary available in `PATH`, or configured with `config set-obscura-bin`.

Set up local state and verify the Obscura binary:

```bash
cargo run -- setup
```

Run the gateway:

```bash
cargo run -- run
```

In another shell:

```bash
cargo run -- status
cargo run -- quotas
```

## Usage

### Configuration

The default state root is `~/.obscura-gateway`.

Important files and directories:

- `~/.obscura-gateway/config.toml`
- `~/.obscura-gateway/gateway.db`
- `~/.obscura-gateway/cookies/`
- `~/.obscura-gateway/profiles/`
- `~/.obscura-gateway/artifacts/`

Point the CLI at a remote gateway:

```bash
cargo run -- config set-server-url https://gw.example.com
cargo run -- config set-api-key <gateway_api_key>
```

Configure the Obscura binary path:

```bash
cargo run -- config set-obscura-bin /usr/local/bin/obscura
```

Show current config:

```bash
cargo run -- config show
```

### Sessions

Create an ephemeral direct session:

```bash
cargo run -- session create
```

Navigate, evaluate JavaScript, dump page content, and close:

```bash
cargo run -- session navigate <session_id> https://example.com/
cargo run -- session eval <session_id> "document.title"
cargo run -- session dump <session_id> --format text
cargo run -- session close <session_id>
```

Supported dump formats:

- `html`
- `text`
- `links`

Create a session with domain policy:

```bash
cargo run -- session create --allowed-domain example.com
cargo run -- session create --denied-domain bad.example.com
```

Create a session with a proxy policy:

```bash
cargo run -- session create --proxy-policy <policy_name>
```

Sessions are intentionally ephemeral. If the gateway restarts, previously active sessions are marked `failed`; create a new session after restart.

### Profiles

Profiles persist identity and cookies across sessions.

Create a profile:

```bash
cargo run -- profile create research --description "research profile"
```

Create with identity hints:

```bash
cargo run -- profile create research \
  --description "research profile" \
  --user-agent "<user-agent>" \
  --accept-language "en-US,en;q=0.9" \
  --timezone "Europe/Helsinki" \
  --viewport-width 1440 \
  --viewport-height 900
```

Use a profile in read-only mode:

```bash
cargo run -- session create --profile <profile_id> --profile-mode read_only
```

Use read-write mode when updated cookies should be saved back on close:

```bash
cargo run -- session create --profile <profile_id> --profile-mode read_write
```

Only one active read-write session is allowed per profile. Multiple read-only sessions may share a profile.

### Cookies

Import cookies:

```bash
cargo run -- cookies import --profile <profile_id> --file cookies.json --format json
cargo run -- cookies import --profile <profile_id> --file cookies.txt --format netscape
```

Export cookies:

```bash
cargo run -- cookies export --profile <profile_id> --format json --output cookies.json
cargo run -- cookies export --profile <profile_id> --format netscape --output cookies.txt
```

Do not import cookies while the profile has active sessions attached.

### Proxy Policies

Add a named proxy policy:

```bash
cargo run -- config upsert-proxy-policy ch socks5 127.0.0.1 1080 \
  --country CH \
  --city Zurich
```

Set the default proxy policy:

```bash
cargo run -- config set-default-proxy-policy ch
```

Use `direct` for sessions that should bypass proxies.

### CDP Grants

Create a one-time CDP grant:

```bash
cargo run -- grant cdp <session_id>
```

The response contains a temporary `ws://` or `wss://` URL. Grants are single-use and expire according to `connect_ttl_secs`.

Prefer `session navigate`, `session eval`, and `session dump` unless a tool specifically needs raw CDP.

### HTTP API

The gateway exposes JSON API routes under `/v1` and OpenAPI at:

```text
/openapi.json
```

All `/v1` routes require:

```http
Authorization: Bearer <api_key>
```

Useful endpoints:

- `GET /healthz`
- `GET /v1/status`
- `GET /v1/quotas`
- `POST /v1/sessions`
- `POST /v1/sessions/{id}/actions/navigate`
- `POST /v1/sessions/{id}/actions/eval`
- `POST /v1/sessions/{id}/actions/dump`
- `POST /v1/sessions/{id}/grants/cdp`
- `GET /v1/profiles`
- `POST /v1/profiles/{id}/cookies:import`
- `GET /v1/profiles/{id}/cookies:export`

## Agent Skill

This repository includes an Agent Skills-compatible skill:

```text
skills/obscura-gateway/
```

List it with:

```bash
bunx skills add . --list
```

Install it with the Agent Skills CLI supported by your agent environment.

## Development

Run the default test suite:

```bash
cargo test
```

Run live smoke tests when a working Obscura binary is available:

```bash
OBSCURA_LIVE_SMOKE=1 cargo test --test negative_restart_smoke -- --nocapture
OBSCURA_LIVE_SMOKE=1 cargo test --test events_grants_artifacts_smoke -- --nocapture
OBSCURA_LIVE_SMOKE=1 cargo test --test live_non_proxy_concurrency -- --nocapture
```

Build and smoke test Docker:

```bash
docker build -t obscura-gateway:local .
docker compose config
docker compose up --build
```

If `cargo clippy` is installed, run it before submitting changes:

```bash
cargo clippy --all-targets -- -D warnings
```

## Contributing

Contributions are welcome.

Recommended workflow:

1. Fork or branch from `main`.
2. Keep changes focused and include tests for behavior changes.
3. Run `cargo test`.
4. Run relevant live smoke tests for session, cookie, CDP, or Docker changes.
5. Update `README.md`, `DOCKER.md`, or the Agent Skill references when behavior or usage changes.
6. Open a pull request with a short summary, validation notes, and any compatibility concerns.

Do not commit local gateway state, `.env` files, API keys, cookies, browser profiles, or generated artifacts.

## License

No license file is currently included. Until a license is added, all rights are reserved by the repository owner.
