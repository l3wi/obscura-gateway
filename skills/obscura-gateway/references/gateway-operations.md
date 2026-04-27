# Gateway Operations

## Role

`obscura-gateway` is the server/control-plane binary. It stores state, exposes the HTTP API, and starts `obscura serve` child processes for active sessions.

Use `obscura-cli` as the normal client binary. The gateway binary should be used directly for setup, local server startup, Docker entrypoints, and server diagnostics.

## State And Defaults

- State root: `~/.obscura-gateway`
- Config: `~/.obscura-gateway/config.toml`
- Database: `~/.obscura-gateway/gateway.db`
- Cookie files: `~/.obscura-gateway/cookies/`
- Profile files: `~/.obscura-gateway/profiles/`
- Artifacts: `~/.obscura-gateway/artifacts/`
- Default listen address: `127.0.0.1:18789`
- Default server URL: `http://127.0.0.1:18789`
- Default stealth: `true`

Docker uses `HOME=/data` and stores state at `/data/.obscura-gateway`.

## Setup And Run

Verify local setup:

```bash
cargo run --bin obscura-gateway -- setup
```

Run the gateway:

```bash
cargo run --bin obscura-gateway -- run
```

Installed binary:

```bash
obscura-gateway setup
obscura-gateway run
```

## Docker

Build and run:

```bash
docker build -t obscura-gateway:local .
docker compose up --build
```

Read generated API key from the persisted volume:

```bash
docker compose exec obscura-gateway sh -c "awk -F'\"' '/^api_key =/{print \$2}' /data/.obscura-gateway/config.toml"
```

Important Docker environment variables:

- `OBSCURA_GATEWAY_SERVER_URL`
- `OBSCURA_GATEWAY_API_KEY`
- `OBSCURA_GATEWAY_LISTEN_ADDR`
- `OBSCURA_GATEWAY_OBSCURA_BIN`
- `OBSCURA_GATEWAY_DEFAULT_PROXY_POLICY`

For reverse proxies, keep `OBSCURA_GATEWAY_LISTEN_ADDR=0.0.0.0:18789` in the container and set `OBSCURA_GATEWAY_SERVER_URL` to the public URL clients should use.

## API And Auth

Health check:

```text
GET /healthz
```

OpenAPI:

```text
GET /openapi.json
```

All `/v1` routes require:

```http
Authorization: Bearer <api_key>
```

Useful server endpoints:

- `GET /v1/status`
- `GET /v1/quotas`
- `GET /v1/sessions`
- `POST /v1/sessions`
- `GET /v1/profiles`

## Fingerprint And Stealth

- New sessions inherit `default_stealth=true` unless a session or profile override is provided.
- When stealth is effective, the gateway launches child Obscura with `--stealth`.
- Profile sessions fill missing identity fields with a Chrome 145 on macOS default fingerprint.
- Effective profile user agents are passed to `obscura serve --user-agent`.
- Existing pre-stealth `gateway.db` state is not migrated automatically; recreate state for this hard-cut schema.

## Release Packaging

Tagged releases publish both binaries:

```bash
git tag v0.1.0
git push origin v0.1.0
```

Release assets:

- `obscura-cli-<tag>-x86_64-unknown-linux-gnu.tar.gz`
- `obscura-gateway-<tag>-x86_64-unknown-linux-gnu.tar.gz`
- `SHA256SUMS`

## Tests

Default validation:

```bash
cargo test
```

Release build validation:

```bash
cargo build --release --locked --bins
```

Live smoke tests require a working `obscura` binary:

```bash
OBSCURA_LIVE_SMOKE=1 cargo test --test negative_restart_smoke -- --nocapture
OBSCURA_LIVE_SMOKE=1 cargo test --test events_grants_artifacts_smoke -- --nocapture
OBSCURA_LIVE_SMOKE=1 cargo test --test live_non_proxy_concurrency -- --nocapture
```
