# Docker

The gateway can run in a self-contained container. The image builds the Rust `obscura-gateway` and `obscura-cli` binaries and downloads the pinned Obscura release binary from GitHub.

Default Obscura release:

- `v0.1.1`
- `obscura-x86_64-linux.tar.gz`

The runtime stage uses Ubuntu 24.04 because the upstream Obscura Linux binary requires glibc 2.39. Debian bookworm is too old for the v0.1.1 release binary.

## Build

```bash
docker build -t obscura-gateway:local .
```

Override the Obscura release:

```bash
docker build \
  --build-arg OBSCURA_VERSION=v0.1.1 \
  --build-arg OBSCURA_ASSET=obscura-x86_64-linux.tar.gz \
  -t obscura-gateway:local .
```

## Run

```bash
docker compose up --build
```

The compose file exposes the gateway on `http://localhost:18789` and stores state in the `obscura-gateway-state` volume.

If `OBSCURA_GATEWAY_API_KEY` is not set, setup generates one in the state volume. Read it with:

```bash
docker compose exec obscura-gateway sh -c "awk -F'\"' '/^api_key =/{print \$2}' /data/.obscura-gateway/config.toml"
```

## Configuration

The entrypoint runs `obscura-gateway setup` on first boot, then applies these environment variables to `config.toml`:

- `OBSCURA_GATEWAY_SERVER_URL`
- `OBSCURA_GATEWAY_API_KEY`
- `OBSCURA_GATEWAY_LISTEN_ADDR`
- `OBSCURA_GATEWAY_OBSCURA_BIN`
- `OBSCURA_GATEWAY_DEFAULT_PROXY_POLICY`

Container defaults:

- `HOME=/data`
- state root: `/data/.obscura-gateway`
- `listen_addr=0.0.0.0:18789`
- `server_url=http://127.0.0.1:18789`
- `obscura_bin=/usr/local/bin/obscura`

For Traefik or another reverse proxy, set `OBSCURA_GATEWAY_SERVER_URL` to the public URL, for example:

```yaml
environment:
  OBSCURA_GATEWAY_SERVER_URL: https://gw.bwc.ad
```

Keep `OBSCURA_GATEWAY_LISTEN_ADDR=0.0.0.0:18789` inside the container so Docker networking can reach it.

## Notes

- Sessions remain ephemeral. Restarting the container marks previously active sessions failed.
- Profiles, cookies, database state, artifacts, and generated API keys live in the state volume.
- CDP child processes run inside the same container; their local WebSocket ports stay on container loopback and are proxied by the gateway.
- The image includes `obscura-cli` for container-local diagnostics, but normal client usage should use the downloaded host CLI against the published gateway URL.
- The bundled Dockerfile currently defaults to Linux `amd64`, matching the published Obscura Linux release asset documented by upstream.
