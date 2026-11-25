/**
 * FeedCourier Channel Plugin: ntfy Push (Plugin API v1)
 * Pure ECMAScript (Boa sandbox safe) — zero imports, synchronous execution via globalThis.fc.
 */

const MAX_NTFY_TEXT_BYTES = 4096;
const VALID_PRIORITIES = new Set(["min", "low", "default", "high", "urgent"]);
const TOPIC_RE = /^[A-Za-z0-9_-]{1,128}$/;
const SERVER_URL_RE = /^(https?):\/\/([^/?#@\s]+)(\/[^?#\s]*)?(?:[?#].*)?$/i;
const IPV6_AUTHORITY_RE = /^\[[0-9a-fA-F:]+\](?::(\d{1,5}))?$/;
const HOST_AUTHORITY_RE =
  /^([A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)*)(?::(\d{1,5}))?$/;

export const meta = {
  id: "ntfy",
  name: "ntfy Push",
  version: "0.3.1",
  api_version: "1",
  kind: "channel",
  min_core: "0.3.1",
  category: "delivery",
  icon: "bell",
  description: {
    en: "Deliver RSS articles as push notifications to ntfy.sh or self-hosted ntfy servers with topic, priority, tags, click URL, and optional bearer token auth.",
    zh: "将 RSS 文章推送到 ntfy.sh 或自建 ntfy 服务器，支持自定义主题、优先级、标签、点击跳转链接与可选 Bearer Token 认证。",
  },
  homepage: "https://feedcourier.ellery.me/plugins/js-plugins",
  repository: "https://github.com/ElleryHe/FeedCourier-Plugins/tree/main/plugins/channels/ntfy",
  allowed_hosts: ["ntfy.sh"],
  hooks: [],
  permissions: ["network"],
  account_schema: {
    type: "object",
    required: ["server_url"],
    properties: {
      server_url: {
        type: "string",
        format: "uri",
        title: "ntfy Server URL",
        default: "https://ntfy.sh",
        placeholder: "https://ntfy.sh",
        description: "Base URL of ntfy.sh or your self-hosted ntfy server.",
      },
      access_token: {
        type: "string",
        format: "password",
        title: "Access Token (Optional)",
        placeholder: "tk_...",
        description: "Optional Bearer token (tk_...) for protected ntfy topics.",
      },
    },
  },
  destination_schema: {
    type: "object",
    required: ["topic"],
    properties: {
      topic: {
        type: "string",
        title: "Topic",
        placeholder: "feedcourier-alerts",
        description: "Target ntfy topic name (ASCII letters, digits, '-' or '_').",
      },
      priority: {
        type: "string",
        title: "Priority",
        enum: ["min", "low", "default", "high", "urgent"],
        default: "default",
        description: "Notification priority level.",
      },
      tags: {
        type: "string",
        title: "Tags (Optional)",
        placeholder: "newspaper,rss",
        description: "Comma-separated ntfy tag/emoji slugs (e.g. newspaper,rss).",
      },
    },
  },
  capabilities: {
    supports_html: false,
    max_text_len: MAX_NTFY_TEXT_BYTES,
  },
};

function isValidAuthority(authority) {
  let match = IPV6_AUTHORITY_RE.exec(authority);
  let portStr;
  if (match) {
    portStr = match[1];
  } else {
    match = HOST_AUTHORITY_RE.exec(authority);
    if (!match) return false;
    portStr = match[2];
  }
  if (portStr !== undefined) {
    const port = Number.parseInt(portStr, 10);
    if (!Number.isFinite(port) || port < 1 || port > 65535) {
      return false;
    }
  }
  return true;
}

function normalizeServerUrl(raw) {
  const trimmed = String(raw ?? "").trim();
  if (!trimmed || /[\x00-\x1f\x7f]/.test(trimmed)) {
    return null;
  }
  const match = SERVER_URL_RE.exec(trimmed);
  if (!match) {
    return null;
  }
  const scheme = match[1].toLowerCase();
  const authority = match[2];
  if (authority.indexOf("@") !== -1 || !isValidAuthority(authority)) {
    return null;
  }
  const rawPath = match[3] || "";
  const cleanPath = rawPath.replace(/\/+$/, "");
  return `${scheme}://${authority.toLowerCase()}${cleanPath}`;
}

function normalizeHttpUrl(raw) {
  const trimmed = String(raw ?? "").trim();
  if (!trimmed || /[\x00-\x1f\x7f\s]/.test(trimmed)) {
    return null;
  }
  const match = /^(https?):\/\/([^/?#@\s]+)(?:[/?#].*)?$/i.exec(trimmed);
  if (!match || !isValidAuthority(match[2])) {
    return null;
  }
  return trimmed;
}

function sanitizeHeaderValue(value, maxLen) {
  return String(value ?? "")
    .replace(/[\r\n\x00-\x1f\x7f]+/g, " ")
    .trim()
    .slice(0, maxLen);
}

function getHeader(headers, name) {
  if (!headers || typeof headers !== "object") return undefined;
  const target = name.toLowerCase();
  if (Object.prototype.hasOwnProperty.call(headers, target)) {
    return headers[target];
  }
  const keys = Object.keys(headers);
  for (let i = 0; i < keys.length; i++) {
    if (keys[i].toLowerCase() === target) {
      return headers[keys[i]];
    }
  }
  return undefined;
}

function parseRetryAfterSecs(header) {
  if (header === undefined || header === null || header === "") return undefined;
  const trimmed = String(header).trim();
  if (/^\d+$/.test(trimmed)) {
    const secs = Number.parseInt(trimmed, 10);
    return secs > 0 ? secs : undefined;
  }
  const dateMs = Date.parse(trimmed);
  if (Number.isFinite(dateMs)) {
    const nowSecs =
      typeof fc !== "undefined" && typeof fc.unixNow === "function"
        ? fc.unixNow()
        : Math.floor(Date.now() / 1000);
    const delta = Math.ceil(dateMs / 1000 - nowSecs);
    return delta > 0 ? delta : undefined;
  }
  return undefined;
}

function buildAuthHeaders(account) {
  const headers = {};
  if (typeof account?.access_token === "string") {
    const token = account.access_token.trim();
    if (token && !/[\r\n\x00-\x1f\x7f]/.test(token)) {
      headers.Authorization = `Bearer ${token}`;
    }
  }
  return headers;
}

function publishToNtfy({ account, destination, title, body, clickUrl }) {
  const serverUrl = normalizeServerUrl(account?.server_url);
  const topic = String(destination?.topic ?? "").trim();
  if (!serverUrl || !TOPIC_RE.test(topic)) {
    return {
      outcome: "permanent_failure",
      diagnostic: "Invalid ntfy server_url or topic configuration",
    };
  }

  const headers = {
    "Content-Type": "text/plain; charset=utf-8",
    ...buildAuthHeaders(account),
  };

  const cleanTitle = sanitizeHeaderValue(title, 200);
  if (cleanTitle) {
    // Use RFC 2047 UTF-8 Base64 encoding when non-ASCII characters appear in Title header.
    headers.Title = /^[\x20-\x7E]+$/.test(cleanTitle)
      ? cleanTitle
      : `=?UTF-8?B?${fc.base64Encode(cleanTitle)}?=`;
  }

  const safeClickUrl = normalizeHttpUrl(clickUrl);
  if (safeClickUrl) {
    headers.Click = sanitizeHeaderValue(safeClickUrl, 1024);
  }

  const priority = String(destination?.priority ?? "").trim();
  if (priority && VALID_PRIORITIES.has(priority)) {
    headers.Priority = priority;
  }

  if (typeof destination?.tags === "string" && destination.tags.trim()) {
    const cleanTags = sanitizeHeaderValue(destination.tags, 256);
    if (cleanTags) {
      headers.Tags = cleanTags;
    }
  }

  const rawBody = String(body ?? "").trim();
  const textBody =
    fc.truncateUtf8Bytes(rawBody, MAX_NTFY_TEXT_BYTES) ||
    fc.truncateUtf8Bytes(cleanTitle, MAX_NTFY_TEXT_BYTES) ||
    "New item";

  let resp;
  try {
    resp = fc.fetch({
      method: "POST",
      url: `${serverUrl}/${encodeURIComponent(topic)}`,
      headers,
      body: textBody,
      timeoutMs: 15000,
    });
  } catch (err) {
    const msg = err?.message ?? String(err);
    if (msg.indexOf("SSRF_BLOCKED:") === 0 || msg.indexOf("PROXY_UNCONFIGURED:") === 0) {
      return { outcome: "permanent_failure", diagnostic: msg };
    }
    return {
      outcome: "retry",
      diagnostic: `ntfy network error: ${msg}`,
    };
  }

  const rawText = typeof resp.body === "string" ? resp.body : "";
  if (resp.status >= 200 && resp.status < 300) {
    let externalId;
    try {
      const parsed = JSON.parse(rawText);
      if (typeof parsed?.id === "string" && parsed.id) {
        externalId = parsed.id;
      }
    } catch {
      // Plain text OK response
    }
    return externalId
      ? { outcome: "delivered", status_code: resp.status, external_id: externalId }
      : { outcome: "delivered", status_code: resp.status };
  }

  if (resp.status === 429) {
    return {
      outcome: "retry",
      status_code: resp.status,
      retry_after_secs: parseRetryAfterSecs(getHeader(resp.headers, "retry-after")) ?? 60,
      diagnostic: `ntfy rate limited (429): ${rawText.slice(0, 200)}`,
    };
  }

  if (resp.status >= 500) {
    const retryAfter = parseRetryAfterSecs(getHeader(resp.headers, "retry-after"));
    const out = {
      outcome: "retry",
      status_code: resp.status,
      diagnostic: `ntfy server error (${resp.status}): ${rawText.slice(0, 200)}`,
    };
    if (retryAfter !== undefined) {
      out.retry_after_secs = retryAfter;
    }
    return out;
  }

  return {
    outcome: "permanent_failure",
    status_code: resp.status,
    diagnostic: `ntfy rejected delivery (${resp.status}): ${rawText.slice(0, 200)}`,
  };
}

export function validate(ctx) {
  const scope = ctx?.scope;
  const config = ctx?.config;
  if (!config || typeof config !== "object" || Array.isArray(config)) {
    return { ok: false, error: "config must be a JSON object" };
  }

  if (scope === "account") {
    const normalizedUrl = normalizeServerUrl(config.server_url);
    if (!normalizedUrl) {
      return {
        ok: false,
        error: "server_url must be a valid http:// or https:// URL without embedded credentials",
      };
    }
    const normalized = { server_url: normalizedUrl };
    if (typeof config.access_token === "string" && config.access_token.trim()) {
      const token = config.access_token.trim();
      if (/[\r\n\x00-\x1f\x7f]/.test(token)) {
        return { ok: false, error: "access_token must not contain control or newline characters" };
      }
      normalized.access_token = token;
    }
    return { ok: true, normalized_config: normalized };
  }

  if (scope === "destination") {
    const topic = String(config.topic ?? "").trim();
    if (!TOPIC_RE.test(topic)) {
      return {
        ok: false,
        error: "topic must be 1–128 characters of ASCII letters, digits, '-' or '_'",
      };
    }
    const priority = String(config.priority ?? "default").trim() || "default";
    if (!VALID_PRIORITIES.has(priority)) {
      return {
        ok: false,
        error: `priority must be one of: ${Array.from(VALID_PRIORITIES).join(", ")}`,
      };
    }
    const normalized = { topic, priority };
    if (typeof config.tags === "string" && config.tags.trim()) {
      const cleanTags = config.tags
        .split(",")
        .map((t) => sanitizeHeaderValue(t, 64))
        .filter(Boolean)
        .join(",");
      if (cleanTags) {
        normalized.tags = cleanTags;
      }
    }
    return { ok: true, normalized_config: normalized };
  }

  return { ok: true, normalized_config: config };
}

export function test(ctx) {
  const scope = ctx?.scope;
  if (scope === "account") {
    const serverUrl = normalizeServerUrl(ctx?.account?.server_url);
    if (!serverUrl) {
      return { ok: false, error: "Invalid server_url" };
    }
    let resp;
    try {
      resp = fc.fetch({
        method: "GET",
        url: `${serverUrl}/v1/health`,
        headers: buildAuthHeaders(ctx?.account),
        timeoutMs: 8000,
      });
    } catch (err) {
      return {
        ok: false,
        error: `ntfy health check failed: ${err?.message ?? String(err)}`,
      };
    }
    if (!resp.ok) {
      return {
        ok: false,
        error: `ntfy /v1/health returned HTTP ${resp.status}`,
      };
    }
    return { ok: true, message: `ntfy server ${serverUrl} reachable` };
  }

  const topic = String(ctx?.destination?.topic ?? "").trim();
  const res = publishToNtfy({
    account: ctx?.account,
    destination: ctx?.destination,
    title: "FeedCourier Test Notification",
    body: `Connectivity verified for topic "${topic}".`,
    clickUrl: "https://feedcourier.ellery.me",
  });
  if (res.outcome === "delivered") {
    return {
      ok: true,
      message: `Test push sent to ntfy topic "${topic}"`,
    };
  }
  return { ok: false, error: res.diagnostic ?? "ntfy test failed" };
}

export function deliver(ctx) {
  const entry = ctx?.entry ?? {};
  const feedTitle = String(ctx?.feed?.title ?? "").trim();
  const entryTitle = String(entry.title ?? "").trim() || "(Untitled)";
  const title = feedTitle ? `[${feedTitle}] ${entryTitle}` : entryTitle;
  const body =
    String(entry.plain_text ?? "").trim() ||
    String(entry.summary_text ?? "").trim() ||
    entryTitle;

  return publishToNtfy({
    account: ctx?.account,
    destination: ctx?.destination,
    title,
    body,
    clickUrl: entry.url,
  });
}
