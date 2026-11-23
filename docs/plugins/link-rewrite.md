---
description: "Configure FeedCourier's built-in Link Rewrite (YOURLS) plugin: authentication modes, SSRF_ALLOWLIST for LAN/Docker YOURLS, SQLite short-link caching, strict mode, and per-channel rewriting rules."
---

# Link Rewrite (YOURLS)

**Link Rewrite (`link-rewrite`)** is FeedCourier's first-party built-in plugin (`kind: "builtin"`, `on_render` hook). It rewrites outgoing article links at delivery time through one or more self-hosted **[YOURLS](https://yourls.org/)** short-link servers, keeping stored article entries untouched while letting each route choose its own short-link provider (or none).

---

## 1. Prerequisites & YOURLS Setup

1. Deploy a self-hosted YOURLS instance accessible via `https://<your-domain>/yourls-api.php` (or an internal HTTP/HTTPS URL on your LAN/Docker network).
2. In the YOURLS Admin panel, open **Tools → Secure passwordless API call** and copy your **Signature Token** (recommended over username/password authentication).
3. Ensure YOURLS accepts `POST` requests to `yourls-api.php` with `action=shorturl&format=json`.

::: warning LAN / Docker YOURLS Requires `SSRF_ALLOWLIST`
Every YOURLS API call goes through FeedCourier's `ProtectedHttpClient` with connect-time DNS SSRF validation. If your YOURLS instance runs on `127.0.0.1`, a private LAN IP (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`), or a Docker Compose service hostname (such as `http://yourls/yourls-api.php`), requests will be blocked with `403 ssrf_blocked` until you add its hostname or IP/CIDR to **`SSRF_ALLOWLIST`** (via environment variable or **Settings → Outbound Network & SSRF Guard**):

```yaml
services:
  feedcourier:
    image: elleryhe/feedcourier:latest
    environment:
      SSRF_ALLOWLIST: "yourls,172.20.0.0/16"
```
:::

---

## 2. Adding a YOURLS Provider in the WebUI

1. Open **Plugins** in the sidebar and enable **Link Rewrite (YOURLS)** (or activate it from the **Marketplace** tab).
2. Click the plugin card to open its detail view and click **Add Provider**.
3. Configure the provider fields (all credentials are encrypted at rest with ChaCha20-Poly1305 and redacted in API/WebUI/MCP responses):

| Field | Required | Default | Description |
| :--- | :---: | :--- | :--- |
| `name` | Yes | — | Display label for this YOURLS provider (e.g., `Personal sho.rt`). |
| `api_url` | Yes | — | Full YOURLS API endpoint URL (for example, `https://sho.rt/yourls-api.php`). Must use `http://` or `https://` without embedded credentials. |
| `auth_mode` | Yes | `signature` | Authentication mode:<br>• `signature` *(recommended)*: Passwordless `signature=<token>` token.<br>• `timed_signature`: Sends `timestamp=<unix>&signature=md5(timestamp + token)` to reduce replay exposure.<br>• `password`: Legacy `username` + `password` credentials. |
| `signature` | Conditional | — | YOURLS secret signature token (required for `signature` and `timed_signature`). Leave blank on edit to keep the stored secret. |
| `username` / `password` | Conditional | — | YOURLS login credentials (required for `password` auth mode). Leave `password` blank on edit to preserve the stored secret. |
| `exclude_domains` | No | `[]` | Domains that should never be shortened. Supports exact hostnames (`github.com`) and wildcard subdomains (`*.example.com`, which also matches `example.com`). The YOURLS provider's own hostname is **always skipped automatically** to prevent recursive shortening. |
| `strict_mode` | No | `false` | Failure policy:<br>• `false` *(default)*: Per-URL graceful degradation — failed URLs keep their original href, the article is still delivered, and a warning is recorded on the task (`diagnostic_log`, highlighted amber in Outbox).<br>• `true`: Any link-shortening failure returns `PluginResult::Retry`, retrying the entire delivery task via normal transient backoff. |
| `pass_title` | No | `true` | Sends the article title (up to 200 chars) as the YOURLS `title` parameter when shortening the article's primary link, preventing YOURLS from making an outbound scrape request back to the origin site. |
| `use_proxy` | No | `false` | Route YOURLS API calls through the global `OUTBOUND_PROXY`. Fails closed with an explicit error if enabled while no global proxy is configured. |

4. Click **Test** to verify connectivity — FeedCourier runs a live `action=shorturl` call for `https://feedcourier.ellery.me/` and reports the resulting short URL.
5. Open any route under **Routes**, expand the **Plugins** section, and select the provider from the **Link Rewrite Provider** dropdown.

::: tip Duplicate URL Handling (`code: "error:url"`)
When YOURLS receives a URL that already exists in its database, it returns `status: "fail"` / `code: "error:url"` alongside the existing `"shorturl"` field (sometimes with HTTP 400 behind certain proxies). FeedCourier detects the returned `shorturl` and treats it as a valid success.
:::

---

## 3. Performance Limits & SQLite Link Cache

To protect outbox delivery throughput on link-heavy digests and avoid hammering your YOURLS instance:

- **Per-Article New-URL Cap**: FeedCourier batch-queries `plugin_link_cache` first so already-cached URLs do not consume budget; at most **`50` unique uncached eligible URLs** (`MAX_REWRITE_URLS_PER_ENTRY = 50`) are shortened per delivery (the article's primary link first, followed by unique `<a href>` links in document order). Remaining uncached links stay untouched with a warning note.
- **Bounded Concurrency & Timeout**: Cache misses are dispatched with at most **`4` concurrent requests** (`REWRITE_CONCURRENCY = 4`), a **`5`-second timeout** per URL (`REWRITE_REQUEST_TIMEOUT = 5s`), and a `64 KiB` response size cap.
- **SQLite Link Cache (`plugin_link_cache`)**:
  - Shortened links are cached per provider instance keyed by `(instance_id, sha256(long_url))`.
  - Retries, multi-route fanout, and multi-recipient SMTP deliveries reuse cached short links without repeat YOURLS calls.
  - Cache hits refresh `last_used_at`; the background maintenance job automatically purges entries unused for more than **90 days**.
  - Changing a provider's `api_url` automatically clears that instance's `plugin_link_cache`, and you can also clear cache manually from the plugin detail page (or `DELETE /api/v1/plugins/{id}/instances/{iid}/cache`).
  - Stateless **Debug Push** runs the full `on_render` hook so you can verify rewriting output, but does **not** write new entries into `plugin_link_cache`.

---

## 4. What Is Rewritten vs. Preserved Across Channels

Link Rewrite strictly parses and rewrites only `http://` and `https://` `<a href="...">` attributes and the delivery's primary link. It **never** rewrites `mailto:`, `tel:`, `#anchors`, the provider's own short-link host, or domains matching `exclude_domains`.

| Channel | Rewritten to Short URL | Preserved as Original URL |
| :--- | :--- | :--- |
| **Telegram** | Rich Message body `<a href>` links, **Read original** link/button, Compact Card link, and Media Mode caption `<a href>` link | **`link_preview_options.url`** (keeps `entry.canonical_url` so Telegram fetches accurate OpenGraph previews directly from the origin), and **all media URLs** |
| **SMTP Email** | HTML & plain-text body `<a href>` links, **Read original →** / **Continue reading →** buttons | All `<img>` `src` attributes, `List-Unsubscribe` header, `/u/{token}` unsubscribe links, and `/v/{token}` verification links |
| **Notion** | Markdown body links and the optional **`Short URL`** database property (when present/mapped) | The **`Link`** database property always retains the original `entry.canonical_url`; all external image / media embed URLs remain untouched |

### Why Media URLs Are Never Rewritten

Media elements (`<img>`, `<video>`, `<audio>`, `<source>`) are intentionally excluded from link rewriting:

1. **Telegram Media Relay & Upload**: FeedCourier fetches origin images/videos directly (with anti-hotlink `Referer` synthesis and magic-byte MP4/image sniffing) before uploading to Telegram. Redirecting media through a URL shortener introduces extra redirect hops, strips or alters origin headers, and can break MIME/container detection.
2. **Email Inline Images**: Mail clients (Gmail, Apple Mail, Outlook) proxy and cache `<img src>` URLs strictly; short-link redirects on image tags increase spam scoring and frequently fail image proxy fetches.
3. **Notion External Images**: Notion embeds external image blocks by fetching the URL directly and validating image content types; shortened redirect URLs frequently fail Notion's image renderer.
