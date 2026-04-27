# CDP, Proxies, And Troubleshooting

## CDP Grants

Use grants when an external agent or tool needs temporary direct CDP WebSocket access:

```bash
cargo run -- grant cdp <session_id>
```

Grant behavior:

- Grant URLs use `ws://` or `wss://` based on `server_url`.
- Grants are one-time use.
- Grants expire according to `connect_ttl_secs`.
- The grant URL path session must match the session embedded in the grant token.

Prefer CLI `session navigate`, `session eval`, and `session dump` unless raw CDP is explicitly needed.

## Proxy Policies

Add a named proxy policy:

```bash
cargo run -- config upsert-proxy-policy <name> socks5 127.0.0.1 1080 \
  --country CH \
  --city Zurich
```

Set a default policy:

```bash
cargo run -- config set-default-proxy-policy <name>
```

Use `direct` to bypass proxies. Do not delete the current default proxy policy.

## Troubleshooting Checklist

Start with state:

```bash
cargo run -- status
cargo run -- quotas
cargo run -- session list
cargo run -- profile list
```

Common failures:

- Gateway responds to `healthz` but work fails: `healthz` only proves the HTTP server is up; check `status`, quotas, and session state.
- CLI cannot connect: verify `server_url`, `api_key`, and whether the gateway is actually listening on `listen_addr`.
- CDP grant cannot connect: confirm `server_url` is externally reachable and has the right HTTP/HTTPS scheme for generated WS/WSS grants.
- Session fails after restart: stored rows are not live browser runtimes; create a new session.
- Cookie import fails: ensure the profile exists, the cookie file parses, and no active session is attached to the profile.
- Profile update/delete fails: check for active sessions using that profile.
- Proxy session fails: confirm the named proxy policy exists and that the proxy endpoint is reachable from the gateway host.

Debugging guidance:

- Reproduce with the CLI before changing code.
- Inspect server code/logs when the API only returns a bare status code.
- Check `git status --short --branch` before edits; this repo may have local smoke tests or uncommitted work.
