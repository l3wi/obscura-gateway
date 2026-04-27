# Gateway Troubleshooting

## Diagnostic Order

Start with server state:

```bash
obscura-cli status
obscura-cli quotas
obscura-cli session list
obscura-cli profile list
```

From source:

```bash
cargo run --bin obscura-cli -- status
```

Check the gateway config:

```bash
obscura-cli config show
```

## Common Server Failures

- Gateway responds to `/healthz` but work fails: `/healthz` only proves the HTTP server is up; check `/v1/status`, quotas, sessions, and logs.
- CLI cannot connect: verify `server_url`, `api_key`, and whether the gateway is listening on `listen_addr`.
- Gateway cannot start sessions: verify `obscura_bin` points to an executable `obscura` binary and that the runtime has required system libraries.
- Session fails after restart: active sessions are live child processes; create a new session after restart.
- Docker gateway is unreachable: keep the container bind address at `0.0.0.0:18789` and publish the port.
- Public CDP grant URL is wrong: set `server_url` to the externally reachable HTTP/HTTPS URL.
- Proxy session fails: confirm the named proxy policy exists and that the proxy endpoint is reachable from the gateway host/container.

## CDP And Child Processes

The gateway starts `obscura serve` child processes for active sessions. Child CDP ports should stay on loopback; external clients should use gateway actions or one-time CDP grants.

Grant URLs use `ws://` or `wss://` based on `server_url`, not `listen_addr`.

## Restart Semantics

Stored sessions are history plus status, not recoverable browser runtimes. On startup, previously active sessions are marked `failed`.

## Code Investigation

- Inspect server routes under `src/server.rs`.
- Inspect child process/session lifecycle under `src/gateway.rs`.
- Inspect config and state paths under `src/config.rs`.
- Inspect persistent records under `src/db.rs`.

Run `git status --short --branch` before edits; this repo may have uncommitted smoke-test or release work.
