# CLI And Operations

## State And Defaults

- State root: `~/.obscura-gateway`
- Config: `~/.obscura-gateway/config.toml`
- Database: `~/.obscura-gateway/gateway.db`
- Default server URL: `http://127.0.0.1:18789`
- Default listen address: `127.0.0.1:18789`

The CLI sends requests to `server_url` with `Authorization: Bearer <api_key>`.

## Setup And Run

Verify local setup:

```bash
cargo run -- setup
```

Run the gateway:

```bash
cargo run -- run
```

Inspect operational state:

```bash
cargo run -- status
cargo run -- quotas
cargo run -- session list
cargo run -- profile list
cargo run -- config show
```

## Configure CLI Or Remote Gateway

Point the local CLI at a gateway:

```bash
cargo run -- config set-server-url https://gw.example.com
cargo run -- config set-api-key <gateway_api_key>
```

For a local server on a non-default bind address, edit `listen_addr` in `config.toml` and set `server_url` to the URL clients can reach.

Use these commands for binary and proxy defaults:

```bash
cargo run -- config set-obscura-bin /path/to/obscura
cargo run -- config set-default-proxy-policy direct
```

## Common Operations

Create and use a direct session:

```bash
cargo run -- session create
cargo run -- session navigate <session_id> https://example.com/
cargo run -- session eval <session_id> "document.title"
cargo run -- session dump <session_id> --format html
cargo run -- session dump <session_id> --format text
cargo run -- session dump <session_id> --format links
cargo run -- session close <session_id>
```

List session artifacts:

```bash
cargo run -- artifacts list <session_id>
```

Tail events:

```bash
cargo run -- events tail <session_id>
```

## Testing

Default validation:

```bash
cargo test
```

Live smoke tests require a working `obscura` binary:

```bash
OBSCURA_LIVE_SMOKE=1 cargo test --test negative_restart_smoke -- --nocapture
OBSCURA_LIVE_SMOKE=1 cargo test --test events_grants_artifacts_smoke -- --nocapture
OBSCURA_LIVE_SMOKE=1 cargo test --test live_non_proxy_concurrency -- --nocapture
```

Proxy smoke tests may depend on host-specific proxy settings from `/root/dev/camofox-browser/.env` and `OBSCURA_PROXY_BRIDGE_HOST`.

If `cargo clippy` is unavailable, report that rather than installing toolchain components unless asked.
