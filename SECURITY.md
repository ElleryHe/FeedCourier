# Security Policy

## Supported Versions

`FeedCourier` is currently in `0.1.x` development; only the latest released
version receives security fixes.

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |
| < 0.1.0 | :x:                |

## Scope

`FeedCourier` is a self-hosted feed delivery gateway that fetches untrusted
external feeds, web pages, and media streams, stores encrypted delivery
credentials in SQLite, and exposes an administrative WebUI, REST API, and
Streamable HTTP MCP server (`/mcp`). Security-relevant issues include:

- **Outbound SSRF & Redirect Re-Validation (`src/net/ssrf.rs`)**: Bypasses of
  connect-time DNS IP validation (including DNS rebinding, IPv4-mapped IPv6
  addresses, loopback, link-local, cloud metadata `169.254.169.254`, or private
  RFC1918/ULA ranges), hop-by-hop `30x` redirect re-validation flaws, or
  `SSRF_ALLOWLIST` matching bypasses.
- **Credential Vault & Authentication (`src/crypto.rs`, `src/api/auth.rs`, `src/mcp/auth.rs`)**:
  Weaknesses in at-rest `ChaCha20-Poly1305` channel secret encryption
  (`master.key`), WebUI `Argon2id` password verification or `fc_session`
  cookie session security (authentication bypass, session fixation, or CSRF),
  and `/mcp` `SHA-256` Bearer token verification.
- **MCP Privilege & Trust Boundaries (`src/mcp/`)**: Privilege escalation from
  `readonly` to `admin` tool scopes, exfiltration or modification of
  WebUI-locked channel credentials and recipient destinations (SMTP addresses,
  Telegram `chat_id`, Notion `data_source_id`), or escapes from
  `<untrusted_feed_content>` XML boundary framing (`src/mcp/sanitize.rs`)
  enabling indirect prompt injection.
- **Untrusted Feed & HTML Sanitization (`src/ingest/`, `src/channels/`)**:
  HTML, Markdown, or template injection (including dangerous URI schemes such
  as `javascript:`, `data:`, or `vbscript:`) across `ammonia`,
  `telegram-rich`, `htmd`, or `minijinja`, as well as panics, unbounded
  stream/memory allocation, or algorithmic complexity denial-of-service (DoS)
  bypassing response body caps.

When exposing `FeedCourier` outside `127.0.0.1` or a private overlay network,
always terminate TLS in front of `LISTEN_ADDR` and restrict `SSRF_ALLOWLIST` to
explicitly trusted internal endpoints.

## Reporting a Vulnerability

Please **do not** open a public GitHub issue or pull request for security
vulnerabilities.

Instead, report vulnerabilities privately through
[GitHub Private Vulnerability Reporting](https://github.com/ElleryHe/FeedCourier/security/advisories/new)
with the affected version or container image digest, minimal reproduction steps
or payload, and expected vs. actual behavior. You can expect an initial
response within 7 days.
