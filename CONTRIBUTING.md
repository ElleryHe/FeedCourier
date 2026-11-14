# Contributing to `FeedCourier`

Thanks for your interest in improving `FeedCourier`! Well-isolated bug reports
(especially tricky real-world RSS/Atom/JSON feeds, Readability edge cases, or
channel delivery regressions) and focused pull requests are very welcome.

## Prerequisites

- **Rust `stable`** (`2024` Edition, `1.94+`) with `rustfmt` and `clippy`
- **Node.js `24+`** and **`pnpm 10+`** (for example via `corepack enable`)
- **`cmake`** and **`pkg-config`** (required to compile `aws-lc-rs` for `rustls`)
- Optional: [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) for
  local license and supply-chain checks

## Build & Verification

The single-file Rust binary embeds `web/dist` at compile time via `rust-embed`,
so build the frontend bundle before running `cargo` checks or tests. Run the
full CI verification suite locally before opening a pull request:

```bash
# 1. Install dependencies, typecheck, and build the embedded WebUI (web/dist)
pnpm --dir web install --frozen-lockfile
pnpm --dir web exec tsc --noEmit
pnpm --dir web build

# 2. Check Rust formatting, Clippy lints, and unit + integration tests
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets

# 3. Audit dependency licenses, crate bans, and registry sources
cargo deny check licenses bans sources
```

For iterative frontend work, run the Rust gateway (`DATA_DIR=./data LISTEN_ADDR=127.0.0.1:8080 cargo run`)
alongside `pnpm --dir web dev` (which proxies `/api` and `/mcp` to `127.0.0.1:8080`),
then run `pnpm --dir web build` once finished.

## Architectural & Security Guardrails

- **Streaming Delivery Gateway, Not an Online Reader**: Keep `FeedCourier`
  lightweight and single-binary. Store permanent 64-char SHA-256 fingerprints
  for anti-drift deduplication, keep HTML/media payloads ephemeral behind
  `RETENTION_HOURS` and zero-refcount `NOT EXISTS` garbage collection, and
  stream media relays in memory without caching files on disk.
- **Mandatory SSRF-Guarded Outbound HTTP**: Every outbound HTTP request touching
  user-supplied or feed-derived URLs (feed polling, Readability full-text
  extraction, anti-hotlink media relays, favicon discovery, and MCP live
  previews) **must** use the guarded HTTP client in `src/net/` (`src/net/ssrf.rs`)
  with connect-time DNS IP validation, hop-by-hop redirect re-validation, and
  bounded stream caps. Never create an unguarded `reqwest::Client` for external
  content.
- **SQLite Single-Writer Pool Discipline**: Preserve the split SQLite WAL pool
  design in `src/db/`. Mutations and outbox lease claims (`lease_until`,
  `checkpoint_json`) belong on the serialized single-writer pool
  (`max_connections = 1`), while read paths use the concurrent
  `query_only = ON` read pool. Never hold a SQLite transaction open across
  network I/O.
- **MCP Credential & Recipient Black-Box Invariant**: Channel secrets
  (`ChaCha20-Poly1305` encrypted at rest) and recipient destinations (SMTP
  addresses, Telegram `chat_id`, Notion `data_source_id`) are strictly
  WebUI-managed. Never expose secrets or allow recipient mutation through `/mcp`
  tools, and pass all external feed strings surfaced to MCP clients through
  `wrap_untrusted_feed_text` (`src/mcp/sanitize.rs`).
- **Regression Tests & Dependencies**: Cover bug fixes and behavioral changes
  with targeted tests under `tests/`, and verify any dependency changes against
  `deny.toml`.

## License

`FeedCourier` is licensed under the GNU Affero General Public License v3.0 only
(`AGPL-3.0-only`). By submitting a contribution to this repository, you agree
that your contribution is licensed under `AGPL-3.0-only` as set forth in
[`LICENSE`](./LICENSE).
