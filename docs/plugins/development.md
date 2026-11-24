---
description: "FeedCourier Plugin API v1 (plugin.js) specification: single-file ES module exports, globalThis.fc host utilities, boa_engine sandbox limits, manifest schema, node --test workflow, and registry submissions."
---

# Plugin Development & API v1 (`plugin.js`)

FeedCourier defines a stable **Plugin API v1 (`api_version: "1"`)** shared by first-party compiled Rust plugins, in-process sandboxed **JS Plugins** (`runtime: "js"`, powered by `boa_engine` in `v0.3.1+`), and the public marketplace registry ([`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins)).

Each JS plugin is a **single self-contained ES module (`plugin.js`)** with zero external runtime dependencies — deployable with one click from the WebUI Marketplace or via custom `.js` upload.

---

## 1. Single-File `plugin.js` Exports

A `plugin.js` module exports a static `meta` object plus the handler functions matching its `kind` and `hooks`:

```js
export const meta = {
  id: "my-channel",
  name: "My Custom Channel",
  version: "0.1.0",
  api_version: "1",
  kind: "channel", // "channel" | "processor"
  runtime: "js",
  min_core: "0.3.1",
  category: "delivery",
  icon: "bell",
  description: "Delivers FeedCourier articles to My Custom Channel.",
  description_i18n: {
    "zh-CN": "将 FeedCourier 文章推送至自定义通道。"
  },
  homepage: "https://feedcourier.ellery.me/plugins/js-plugins",
  hooks: [], // e.g. ["on_render"] or ["on_delivered"]
  permissions: ["network"],
  allowed_hosts: ["api.example.com"],
  account_schema: {
    type: "object",
    required: ["server_url"],
    properties: {
      server_url: {
        type: "string",
        format: "uri",
        title: "Server URL",
        default: "https://api.example.com"
      },
      access_token: {
        type: "string",
        format: "password",
        "x-secret": true,
        title: "Access Token"
      }
    }
  },
  destination_schema: {
    type: "object",
    required: ["target"],
    properties: {
      target: { type: "string", title: "Target Room / Topic" }
    }
  }
};

export function validate(input) {
  // input: { scope: "account" | "destination" | "instance", config }
  return { ok: true, normalized_config: input.config };
}

export function test(input) {
  // input: { scope, account?, destination?, instance?, config?, locale? }
  const res = fc.fetch({ method: "GET", url: "https://api.example.com/ping", timeoutMs: 8000 });
  return { ok: res.ok, message: res.ok ? "Connected" : `HTTP ${res.status}` };
}

export function deliver(input) {
  // input: { idempotency_key, task_id, attempt, account, destination, entry, feed, route, checkpoint }
  const body = fc.truncateUtf8Bytes(input.entry.summary_text || input.entry.title || "", 3500);
  const res = fc.fetch({
    method: "POST",
    url: `${input.account.server_url.replace(/\/+$/, "")}/send`,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ target: input.destination.target, title: input.entry.title, body }),
    timeoutMs: 15000
  });
  if (res.status === 429) return { outcome: "retry", status_code: 429, retry_after_secs: 60 };
  if (res.status >= 500) return { outcome: "retry", status_code: res.status, diagnostic: `Upstream HTTP ${res.status}` };
  if (!res.ok) return { outcome: "permanent_failure", status_code: res.status, diagnostic: `HTTP ${res.status}` };
  return { outcome: "delivered", status_code: res.status };
}
```

### Supported Exported Functions

| Export | Kind / Hook | Budget | Contract |
| :--- | :---: | :---: | :--- |
| `meta` | All (required) | Compile-time | Static manifest object describing `id`, `name`, `version`, `api_version: "1"`, `kind`, `min_core`, `allowed_hosts`, schemas, and `hooks`. |
| `validate(input)` | All (optional) | `10s` | Validates `{ scope: "account" \| "destination" \| "instance", config }`; returns `{ ok: true, normalized_config? }` or `{ ok: false, error }`. |
| `test(input)` | All (optional) | `10s` | Runs an interactive connectivity test for `{ scope, account?, destination?, instance?, locale? }`; returns `{ ok: true \| false, message?, error? }`. |
| `deliver(input)` | `kind: "channel"` | `30s` | Delivers a rendered article to the custom channel. Returns `{ outcome, status_code?, external_id?, checkpoint?, retry_after_secs?, next_window_at?, diagnostic? }`. |
| `onRender(input)` | `hooks: ["on_render"]` | `15s` | Transforms `content_html` / `primary_link` before channel rendering. Returned HTML is **always re-sanitized through `ammonia`**. |
| `onDelivered(input)` | `hooks: ["on_delivered"]` | `10s` | Receives persistent at-least-once post-delivery notifications dispatched from `plugin_events` (up to 5 attempts with exponential backoff). |

### `deliver(input)` Payload & Outcomes

- **`input` fields**:
  - `idempotency_key`: deterministic SHA-256 string (unique per debug push)
  - `task_id`, `attempt`: outbox task metadata
  - `account`, `destination`: decrypted account & destination configs
  - `entry`: `{ title, url, summary_text, plain_text, content_html, author, published_at, published_local, media }` (`published_local` formatted as `YYYY-MM-DD HH:MM` in the route timezone)
  - `feed`: `{ id, title, url, site_url, favicon_url }`
  - `route`: `{ id, name, locale, timezone }`
  - `checkpoint`: prior checkpoint JSON or `null`
- **Returned `outcome`**:
  - `"delivered"` — delivery succeeded (optional `status_code`, `external_id`, `checkpoint`, `diagnostic`)
  - `"retry"` — transient error; honors optional `retry_after_secs` (and triggers account-group cooldown on `429` or `retry_after_secs`)
  - `"permanent_failure"` — non-retryable failure (moves task to `dead_letter`)
  - `"deferred"` — postpones delivery without consuming an attempt (optional `next_window_at` or `retry_after_secs`)

---

## 2. Host Utilities (`globalThis.fc`)

FeedCourier injects a read-only `globalThis.fc` namespace into every sandboxed execution (plugins must use pure ECMAScript + `globalThis.fc` only — no `import`, `fetch`, `URL`, `Buffer`, or `TextEncoder`):

| Function | Signature | Description |
| :--- | :--- | :--- |
| `fc.fetch` | `fc.fetch({ method?, url, headers?, body?, timeoutMs? }) -> { status, ok, headers, body }` | Synchronous HTTP(S) client (max 4 calls per channel/validate/test/onDelivered hook, 10 per `onRender`; `timeoutMs` clamped to `100..=15000`; `1 MiB` response cap; redirects disabled; lowercase response header keys). Enforces the dual allowlist (`meta.allowed_hosts` via `ProtectedHttpClient` SSRF guard + exact `(scheme, host, port)` admin-configured `format: "uri"` origins) and hard-blocks cloud metadata IPs. Honors channel/instance `use_proxy`. |
| `fc.sha256` | `fc.sha256(input: string, encoding?: "hex" \| "base64" \| "base64url") -> string` | SHA-256 digest of UTF-8 input (defaults to lowercase `"hex"`). |
| `fc.hmacSha256` | `fc.hmacSha256(message: string, secret: string, encoding?: "hex" \| "base64" \| "base64url") -> string` | HMAC-SHA256 signature (defaults to lowercase `"hex"`). |
| `fc.md5` | `fc.md5(input: string) -> string` | Lowercase hex MD5 digest (useful for legacy API signatures). |
| `fc.base64Encode` | `fc.base64Encode(input: string) -> string` | Standard Base64 encoding of UTF-8 text (useful for `Basic` auth or RFC 2047 headers). |
| `fc.base64Decode` | `fc.base64Decode(b64: string) -> string` | Decodes Standard or URL-safe Base64 back to UTF-8 text. |
| `fc.base64UrlEncode` | `fc.base64UrlEncode(input: string) -> string` | URL-safe Base64 without padding (`RFC 4648`). |
| `fc.utf8ByteLength` | `fc.utf8ByteLength(str: string) -> number` | Exact UTF-8 byte length of a string. |
| `fc.truncateUtf8Bytes` | `fc.truncateUtf8Bytes(str: string, maxBytes: number) -> string` | Truncates a string to at most `maxBytes` UTF-8 bytes on a valid Unicode scalar boundary (ideal for ntfy / APNs / Bark 4 KB payload caps). |
| `fc.htmlToText` | `fc.htmlToText(html: string) -> string` | Converts HTML fragments to clean plain text. |
| `fc.unixNow` | `fc.unixNow() -> number` | Current UTC Unix epoch timestamp in seconds. |
| `fc.uuid` | `fc.uuid() -> string` | Generates a random RFC 4122 UUID v4 string. |

---

## 3. Sandbox & Core Safety Boundaries

- **Isolated Pure-Rust Engine (`boa_engine`)**: No filesystem, `process`, `env`, native addons, `import` statements (`IdleModuleLoader`), or raw sockets exist inside the sandbox. All hooks must execute synchronously (returning a `Promise` is rejected).
- **Dual Network Allowlist Enforcement**: Any `fc.fetch()` attempt to a hostname not listed in `meta.allowed_hosts` or an administrator-configured `format: "uri"` field is rejected immediately before DNS resolution (`403 ssrf_blocked`). Cloud metadata IPs (`169.254.169.254`, `100.100.100.200`, `fd00:ec2::254`, `0.0.0.0`, `::`, `metadata.google.internal`) and HTTP `3xx` redirects are unconditionally blocked.
- **Resource Caps**:
  - Script source size is capped at **`512 KiB`**; HTTP response bodies are capped at **`1 MiB`**.
  - Each invocation runs under hard VM limits (`loop_iteration_limit = 200,000`, `recursion_limit = 128`, `stack_size_limit = 64 KiB`) and wall-clock timeouts (`5s` compile, `10s` validate/test/onDelivered, `15s` `onRender`, `30s` `deliver`).
  - HTML returned by `onRender` is always re-sanitized via `ammonia`.
  - Serialized `checkpoint` JSON returned by `deliver` is capped at **`16 KiB`**.
- **Durable `on_delivered` Outbox**: When a delivery succeeds, FeedCourier inserts one row into `plugin_events` **inside the same SQLite transaction** as marking `delivery_tasks.status = 'delivered'` for every enabled plugin declaring `on_delivered`. A dedicated background worker invokes `onDelivered` with exponential backoff (`15s, 30s, 60s, 120s`, max **5 attempts**) without affecting delivery status or channel quotas.

---

## 4. Marketplace Registry & Testing (`ElleryHe/FeedCourier-Plugins`)

Official and community JS plugins live in **[`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins)** — including the reference **`ntfy`** and **`bark`** channel plugins — with zero-dependency unit tests runnable directly under Node.js 24+:

```bash
cd FeedCourier-Plugins
node --test tests/*.test.mjs
node scripts/build-index.mjs --check --verify-index
```

### Registry Entry & `index.json` Fields

| Field | Type | Description |
| :--- | :--- | :--- |
| `id`, `name`, `version`, `api_version` | `string` | Unique kebab-case ID, display name, latest SemVer version, `"1"`. |
| `kind` | `string` | `"builtin"` \| `"channel"` \| `"processor"` \| `"coming_soon"`. |
| `runtime` | `string` | `"builtin"` \| `"js"`. |
| `min_core` | `string` | Minimum compatible FeedCourier version (checked before install). |
| `category`, `icon`, `description`, `description_i18n` | `string` / `object` | Marketplace filtering, icon key, and localized descriptions (`{ "en": "...", "zh": "..." }`). |
| `homepage`, `repository` | `string` | Strict `https://` documentation and source links. |
| `allowed_hosts` | `string[]` | Public hostnames the plugin may contact via `fc.fetch()` (displayed as security pills in WebUI). |
| `latest`, `versions` | `string` / `object[]` | Version list (`{ version, min_core, path, sha256, changelog_path?, changelog? }`) verified by SHA-256 during 1-click install. |
| `hooks`, `permissions` | `string[]` | Declared hooks (`on_render`, `on_delivered`) and capabilities (`["network"]`). |
| `account_schema`, `destination_schema`, `instance_schema`, `route_option_schema` | `object` | JSON Schemas driving WebUI dynamic forms (`format: "password"` and `x-secret: true` fields are encrypted & masked automatically). |

### Submitting a Plugin

1. Fork `ElleryHe/FeedCourier-Plugins`, add `plugins/<channels|processors>/<id>/<version>/plugin.js`, `CHANGELOG.md`, `plugins/<channels|processors>/<id>/README.md` (`README.zh-CN.md`), and `tests/<id>.test.mjs`.
2. Validate locally with `node --test tests/*.test.mjs` and `node scripts/build-index.mjs --check`.
3. **Do not manually edit `index.json` in your PR.** GitHub Actions validates every PR and automatically rebuilds `index.json` (including verified `sha256` digests) upon merge to `main`.
