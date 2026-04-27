# CLI Operations

## Role

`obscura-cli` is the client binary. It talks to the configured gateway URL with `Authorization: Bearer <api_key>`.

The gateway server is separate. If the task is to run/deploy the server, use `obscura-gateway`.

## Install

Install latest Linux `x86_64` release:

```bash
curl -fsSL https://raw.githubusercontent.com/l3wi/obscura-gateway/main/scripts/install-obscura-cli.sh | sh
```

Install a specific tag:

```bash
curl -fsSL https://raw.githubusercontent.com/l3wi/obscura-gateway/main/scripts/install-obscura-cli.sh | VERSION=v0.2.0 sh
```

Manual downloads:

```text
https://github.com/l3wi/obscura-gateway/releases
```

## Configure

Point the CLI at a gateway:

```bash
obscura-cli config set-server-url https://gw.example.com
obscura-cli config set-api-key <gateway_api_key>
```

Show current config:

```bash
obscura-cli config show
```

Set default stealth mode for new sessions:

```bash
obscura-cli config set-default-stealth true
```

For local development from source:

```bash
cargo run --bin obscura-cli -- config show
```

## Inspect

```bash
obscura-cli status
obscura-cli quotas
obscura-cli session list
obscura-cli profile list
```

## Common Session Flow

```bash
obscura-cli session create
obscura-cli session navigate <session_id> https://example.com/
obscura-cli session eval <session_id> "document.title"
obscura-cli session dump <session_id> --format html
obscura-cli session dump <session_id> --format text
obscura-cli session dump <session_id> --format links
obscura-cli session close <session_id>
```

Stealth is enabled by default. Override it per session when needed:

```bash
obscura-cli session create --no-stealth
obscura-cli session create --stealth
```

Supported dump formats:

- `html`
- `text`
- `links`

## Artifacts And Events

List session artifacts:

```bash
obscura-cli artifacts list <session_id>
```

Tail events:

```bash
obscura-cli events tail <session_id>
```

## Local Gateway From Source

If no gateway is running and the repo is checked out:

```bash
cargo run --bin obscura-gateway -- setup
cargo run --bin obscura-gateway -- run
```

Then use `cargo run --bin obscura-cli -- ...` in another shell.
