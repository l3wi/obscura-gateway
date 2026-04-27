# Sessions, Profiles, And Cookies

## Ephemeral Sessions

Create direct sessions by default:

```bash
obscura-cli session create
```

Create with domain policy:

```bash
obscura-cli session create --allowed-domain example.com
obscura-cli session create --denied-domain bad.example.com
```

Create with a proxy policy:

```bash
obscura-cli session create --proxy-policy <policy_name>
```

Rules:

- Session IDs refer to live gateway runtimes.
- Persisted DB rows alone cannot revive a browser after gateway restart.
- On gateway startup, previously active sessions are marked `failed`.
- `session navigate` enforces default and session domain allow/deny policies.
- Max concurrent sessions is exposed by `quotas`; avoid creating above that limit.

## Profiles

Profiles persist identity and cookies across sessions. Use them for logged-in state or stable browser identity.

Create a profile:

```bash
obscura-cli profile create <name> --description "purpose and owner"
```

Create with identity hints:

```bash
obscura-cli profile create research \
  --description "research profile" \
  --user-agent "<ua>" \
  --accept-language "en-US,en;q=0.9" \
  --timezone "Europe/Helsinki" \
  --viewport-width 1440 \
  --viewport-height 900 \
  --proxy-affinity <policy_name> \
  --stealth
```

Profile stealth is tri-state:

- Omit both flags to inherit the gateway default.
- Use `--stealth` to force upstream Obscura stealth mode for that profile.
- Use `--no-stealth` to disable upstream Obscura stealth mode for that profile.
- Profile sessions fill missing identity fields with a Chrome 145 on macOS default fingerprint.
- The effective profile user agent is passed to `obscura serve --user-agent` and reinforced through CDP.

Use read-only mode when cookies must not be saved back:

```bash
obscura-cli session create --profile <profile_id> --profile-mode read_only
```

Use read-write mode when session cookies should persist on close:

```bash
obscura-cli session create --profile <profile_id> --profile-mode read_write
```

Profile rules:

- Multiple `read_only` sessions can share a profile.
- Only one active `read_write` session is allowed per profile.
- Do not import cookies while the profile has active sessions.
- Do not delete a profile while active sessions are attached.
- Close read-write sessions cleanly so cookies can be fetched and persisted.
- Timezone and viewport identity fields are applied through CDP emulation on attach.

## Cookies

Import cookies into a profile:

```bash
obscura-cli cookies import --profile <profile_id> --file cookies.json --format json
obscura-cli cookies import --profile <profile_id> --file cookies.txt --format netscape
```

Export cookies:

```bash
obscura-cli cookies export --profile <profile_id> --format json --output cookies.json
obscura-cli cookies export --profile <profile_id> --format netscape --output cookies.txt
```

Cookie notes:

- `--format auto` infers JSON or Netscape format from parsing and file extension.
- JSON may be an array or `{ "cookies": [...] }`.
- Netscape import expects seven tab-separated fields.
- Cookies are saved as profile JSON and Netscape files under `~/.obscura-gateway/cookies/`.
- The last raw cookie import is stored under the profile directory as `last-cookie-import` for audit/debugging.
