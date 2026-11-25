/**
 * FeedCourier Channel Plugin: Bark Push (Plugin API v1)
 * Pure ECMAScript (Boa sandbox safe) — zero imports, synchronous execution via globalThis.fc.
 *
 * Formats RSS articles for iOS/macOS Bark notifications inspired by FeedCourier's
 * Telegram Compact Card (摘要模式) layout: concise headline, clean summary with
 * duplicate-title suppression, compact editorial footer ("— Source · Author · Time"),
 * tap-to-open article URL, and optional feed icon.
 */

const MAX_BARK_BODY_BYTES = 2400;
const MAX_BARK_TITLE_CHARS = 160;
const MAX_BARK_TITLE_BYTES = 300;
const MAX_BARK_SUBTITLE_CHARS = 100;
const MAX_BARK_SUBTITLE_BYTES = 180;
const MAX_BARK_FOOTER_BYTES = 240;

const VALID_LEVELS = new Set(["active", "timeSensitive", "passive", "critical"]);
const SOUND_RE = /^[A-Za-z0-9_.-]{1,64}$/;
const DEVICE_KEY_RE = /^[\x21-\x7E]{1,256}$/;
const SERVER_URL_RE = /^(https?):\/\/([^/?#@\s]+)(\/[^?#\s]*)?(?:[?#].*)?$/i;
const IPV6_AUTHORITY_RE = /^\[[0-9a-fA-F:]+\](?::(\d{1,5}))?$/;
const HOST_AUTHORITY_RE =
  /^([A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)*)(?::(\d{1,5}))?$/;
const DEDUPE_STRIP_RE =
  /[\s.,!?;:'"`()[\]{}<>《》【】「」『』（）—–\-_·•…，。！？；：、~@#$%^&*+=|\\\/]+/g;

export const meta = {
  id: "bark",
  name: "Bark Push",
  version: "0.3.1",
  api_version: "1",
  kind: "channel",
  min_core: "0.3.1",
  category: "delivery",
  icon: "bell",
  description: {
    en: "Push RSS article summaries to iOS/macOS via Bark (official api.day.app or self-hosted server), formatted like Telegram Compact Card.",
    zh: "通过 Bark 向 iOS/macOS 推送文章摘要卡片（参考 Telegram 摘要模式排版，支持官方 api.day.app 与自建服务端）。",
  },
  homepage: "https://feedcourier.ellery.me/plugins/js-plugins",
  repository: "https://github.com/ElleryHe/FeedCourier-Plugins/tree/main/plugins/channels/bark",
  allowed_hosts: ["api.day.app"],
  hooks: [],
  permissions: ["network"],
  account_schema: {
    type: "object",
    required: ["server_url"],
    properties: {
      server_url: {
        type: "string",
        format: "uri",
        title: "Bark Server URL",
        default: "https://api.day.app",
        placeholder: "https://api.day.app",
        description: "Official https://api.day.app or your self-hosted Bark server URL.",
      },
      username: {
        type: "string",
        title: "Basic Auth Username (Optional)",
        description: "Optional HTTP Basic Auth username for a self-hosted Bark server.",
      },
      password: {
        type: "string",
        format: "password",
        title: "Basic Auth Password (Optional)",
        description: "Optional HTTP Basic Auth password for a self-hosted Bark server.",
      },
    },
  },
  destination_schema: {
    type: "object",
    required: ["device_key"],
    properties: {
      device_key: {
        type: "string",
        format: "password",
        title: "Device Key",
        description: "Bark app Device Key (encrypted at rest and masked in UI/logs).",
      },
      group: {
        type: "string",
        title: "Notification Group (Optional)",
        description: "Optional Bark group name. Leave blank to group automatically by Feed title.",
      },
      level: {
        type: "string",
        title: "Interruption Level",
        enum: ["active", "timeSensitive", "passive", "critical"],
        default: "active",
        description: "iOS notification interruption level.",
      },
      sound: {
        type: "string",
        title: "Sound (Optional)",
        placeholder: "minuet",
        description: "Optional Bark notification sound name (e.g. alarm, chime, minuet).",
      },
      send_icon: {
        type: "boolean",
        title: "Include Feed Icon",
        default: true,
        description: "Attach the feed's HTTPS favicon as the Bark notification icon when available.",
      },
    },
  },
  capabilities: {
    supports_html: false,
    max_text_len: MAX_BARK_BODY_BYTES,
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

function normalizeHttpUrl(raw, httpsOnly) {
  const trimmed = String(raw ?? "").trim();
  if (!trimmed || trimmed.length > 2048 || /[\x00-\x1f\x7f\s]/.test(trimmed)) {
    return null;
  }
  const re = httpsOnly
    ? /^(https):\/\/([^/?#@\s]+)(?:[/?#].*)?$/i
    : /^(https?):\/\/([^/?#@\s]+)(?:[/?#].*)?$/i;
  const match = re.exec(trimmed);
  if (!match || !isValidAuthority(match[2])) {
    return null;
  }
  return trimmed;
}

function collapseSingleLine(raw) {
  return String(raw ?? "")
    .replace(/[\r\n\t\x00-\x1f\x7f]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

function cleanMultilineText(raw) {
  const normalized = String(raw ?? "")
    .replace(/\r\n?/g, "\n")
    .replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g, "");
  const lines = normalized.split("\n").map((line) => line.replace(/[ \t]+/g, " ").trim());
  const out = [];
  let blankStreak = 0;
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].length === 0) {
      blankStreak += 1;
      if (out.length > 0 && blankStreak === 1) {
        out.push("");
      }
    } else {
      blankStreak = 0;
      out.push(lines[i]);
    }
  }
  while (out.length > 0 && out[out.length - 1] === "") {
    out.pop();
  }
  return out.join("\n").trim();
}

function truncateCodePointsAndBytes(text, maxChars, maxBytes, ellipsis) {
  const str = String(text ?? "");
  const suffix = ellipsis ?? "";
  const chars = Array.from(str);
  let truncated = false;
  let candidate = str;

  if (chars.length > maxChars) {
    const keep = Math.max(0, maxChars - Array.from(suffix).length);
    candidate = chars.slice(0, keep).join("").trimEnd();
    truncated = true;
  }

  const suffixBytes = suffix ? fc.utf8ByteLength(suffix) : 0;
  const maxContentBytes = truncated ? Math.max(0, maxBytes - suffixBytes) : maxBytes;
  if (fc.utf8ByteLength(candidate) > maxContentBytes) {
    const budget = Math.max(0, maxBytes - suffixBytes);
    candidate = fc.truncateUtf8Bytes(candidate, budget).trimEnd();
    truncated = true;
  }

  return truncated && suffix ? `${candidate}${suffix}` : candidate;
}

function truncateExcerptToBytes(text, maxBytes) {
  if (maxBytes <= 0) return "";
  if (fc.utf8ByteLength(text) <= maxBytes) {
    return text;
  }
  const ellipsis = "…";
  const ellipsisBytes = fc.utf8ByteLength(ellipsis);
  if (maxBytes <= ellipsisBytes) {
    return fc.truncateUtf8Bytes(text, maxBytes);
  }
  let clipped = fc.truncateUtf8Bytes(text, maxBytes - ellipsisBytes);
  // Prefer breaking cleanly at a word or paragraph boundary without discarding > 25% of content.
  const minKeep = Math.floor(clipped.length * 0.75);
  let breakIdx = -1;
  for (let i = clipped.length - 1; i >= minKeep; i--) {
    const ch = clipped[i];
    if (ch === "\n" || ch === " " || ch === "\t") {
      breakIdx = i;
      break;
    }
  }
  if (breakIdx > 0) {
    clipped = clipped.slice(0, breakIdx);
  }
  clipped = clipped.trimEnd();
  return clipped ? `${clipped}${ellipsis}` : fc.truncateUtf8Bytes(text, maxBytes);
}

function normalizeForComparison(text) {
  return String(text ?? "")
    .toLowerCase()
    .replace(DEDUPE_STRIP_RE, "");
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
  const username = typeof account?.username === "string" ? account.username.trim() : "";
  const password = typeof account?.password === "string" ? account.password : "";
  if (username && password && !/[\r\n]/.test(username) && !/[\r\n]/.test(password)) {
    headers.Authorization = `Basic ${fc.base64Encode(`${username}:${password}`)}`;
  }
  return headers;
}

function formatCompactCard({ entry, feed, destination }) {
  const feedTitle = collapseSingleLine(feed?.title);
  const rawEntryTitle = collapseSingleLine(entry?.title);
  const author = collapseSingleLine(entry?.author);
  const publishedLocal = collapseSingleLine(entry?.published_local);

  const rawTitle = rawEntryTitle || feedTitle || "New article";
  const title = truncateCodePointsAndBytes(rawTitle, MAX_BARK_TITLE_CHARS, MAX_BARK_TITLE_BYTES, "…");

  const customGroup = collapseSingleLine(destination?.group);
  const group = truncateCodePointsAndBytes(
    customGroup || feedTitle || "FeedCourier",
    128,
    200,
    "",
  );

  // Concise subtitle when Feed title adds context beyond the headline.
  let subtitle = "";
  if (feedTitle && feedTitle.toLowerCase() !== rawTitle.toLowerCase()) {
    let rawSubtitle = feedTitle;
    if (
      author &&
      author.toLowerCase() !== feedTitle.toLowerCase() &&
      feedTitle.length + author.length + 3 <= 80
    ) {
      rawSubtitle = `${feedTitle} · ${author}`;
    }
    subtitle = truncateCodePointsAndBytes(
      rawSubtitle,
      MAX_BARK_SUBTITLE_CHARS,
      MAX_BARK_SUBTITLE_BYTES,
      "…",
    );
  }

  // Prefer summary_text, fall back to plain_text, then stripped content_html.
  let rawSummary = cleanMultilineText(entry?.summary_text);
  if (!rawSummary) {
    rawSummary = cleanMultilineText(entry?.plain_text);
  }
  if (!rawSummary && typeof entry?.content_html === "string" && entry.content_html.trim()) {
    rawSummary = cleanMultilineText(fc.htmlToText(entry.content_html));
  }

  // Duplicate-title omission (matching Telegram Compact Card behavior).
  const normTitle = normalizeForComparison(rawTitle);
  const normSummary = normalizeForComparison(rawSummary);
  const excerpt = rawSummary && normSummary && normSummary !== normTitle ? rawSummary : "";

  // Compact editorial metadata footer ("— Source · Author · YYYY-MM-DD HH:MM").
  const metaParts = [];
  if (feedTitle) metaParts.push(feedTitle);
  if (author && (!feedTitle || author.toLowerCase() !== feedTitle.toLowerCase())) {
    metaParts.push(author);
  }
  if (publishedLocal) metaParts.push(publishedLocal);

  const footer =
    metaParts.length > 0
      ? truncateCodePointsAndBytes(`— ${metaParts.join(" · ")}`, 180, MAX_BARK_FOOTER_BYTES, "…")
      : "";

  let body = "";
  if (excerpt && footer) {
    const separatorAndFooter = `\n\n${footer}`;
    const footerBytes = fc.utf8ByteLength(separatorAndFooter);
    const excerptBudget = Math.max(0, MAX_BARK_BODY_BYTES - footerBytes);
    const clippedExcerpt = truncateExcerptToBytes(excerpt, excerptBudget);
    body = clippedExcerpt ? `${clippedExcerpt}${separatorAndFooter}` : footer;
  } else if (excerpt) {
    body = truncateExcerptToBytes(excerpt, MAX_BARK_BODY_BYTES);
  } else if (footer) {
    body = footer;
  } else {
    body = title;
  }

  if (fc.utf8ByteLength(body) > MAX_BARK_BODY_BYTES) {
    body = fc.truncateUtf8Bytes(body, MAX_BARK_BODY_BYTES);
  }

  const payload = {
    title,
    body,
    group,
  };
  if (subtitle) {
    payload.subtitle = subtitle;
  }

  const level = String(destination?.level ?? "active").trim() || "active";
  payload.level = VALID_LEVELS.has(level) ? level : "active";

  const sound = String(destination?.sound ?? "").trim();
  if (sound && SOUND_RE.test(sound)) {
    payload.sound = sound;
  }

  const clickUrl = normalizeHttpUrl(entry?.url, false);
  if (clickUrl) {
    payload.url = clickUrl;
  }

  if (destination?.send_icon !== false) {
    const iconUrl = normalizeHttpUrl(feed?.favicon_url, true);
    if (iconUrl) {
      payload.icon = iconUrl;
    }
  }

  return payload;
}

function sendBarkPush({ account, destination, entry, feed }) {
  const serverUrl = normalizeServerUrl(account?.server_url);
  const deviceKey = String(destination?.device_key ?? "").trim();
  if (!serverUrl || !deviceKey || !DEVICE_KEY_RE.test(deviceKey) || /[/?#]/.test(deviceKey)) {
    return {
      outcome: "permanent_failure",
      diagnostic: "Invalid Bark server_url or device_key configuration",
    };
  }

  const card = formatCompactCard({ entry, feed, destination });
  const payload = {
    device_key: deviceKey,
    ...card,
  };

  let resp;
  try {
    resp = fc.fetch({
      method: "POST",
      url: `${serverUrl}/push`,
      headers: {
        "Content-Type": "application/json; charset=utf-8",
        ...buildAuthHeaders(account),
      },
      body: JSON.stringify(payload),
      timeoutMs: 15000,
    });
  } catch (err) {
    const msg = err?.message ?? String(err);
    if (msg.indexOf("SSRF_BLOCKED:") === 0 || msg.indexOf("PROXY_UNCONFIGURED:") === 0) {
      return { outcome: "permanent_failure", diagnostic: msg };
    }
    return {
      outcome: "retry",
      diagnostic: `Bark network error: ${msg}`,
    };
  }

  const rawText = typeof resp.body === "string" ? resp.body : "";
  let parsed = null;
  try {
    if (rawText.trim()) {
      parsed = JSON.parse(rawText);
    }
  } catch {
    // Non-JSON response body
  }

  if (resp.status === 429) {
    return {
      outcome: "retry",
      status_code: resp.status,
      retry_after_secs: parseRetryAfterSecs(getHeader(resp.headers, "retry-after")) ?? 60,
      diagnostic: `Bark rate limited (429): ${rawText.slice(0, 200)}`,
    };
  }

  if (resp.status >= 500) {
    const retryAfter = parseRetryAfterSecs(getHeader(resp.headers, "retry-after"));
    const out = {
      outcome: "retry",
      status_code: resp.status,
      diagnostic: `Bark server error (${resp.status}): ${rawText.slice(0, 200)}`,
    };
    if (retryAfter !== undefined) {
      out.retry_after_secs = retryAfter;
    }
    return out;
  }

  if (resp.status < 200 || resp.status >= 300) {
    const msg = typeof parsed?.message === "string" ? parsed.message : rawText.slice(0, 200);
    return {
      outcome: "permanent_failure",
      status_code: resp.status,
      diagnostic: `Bark rejected delivery (${resp.status}): ${msg}`,
    };
  }

  if (parsed && typeof parsed.code === "number" && parsed.code !== 200) {
    const msg = typeof parsed.message === "string" ? parsed.message : rawText.slice(0, 200);
    if (parsed.code === 429) {
      return {
        outcome: "retry",
        status_code: 429,
        retry_after_secs: parseRetryAfterSecs(getHeader(resp.headers, "retry-after")) ?? 60,
        diagnostic: `Bark API rate limited (${parsed.code}): ${msg}`,
      };
    }
    if (parsed.code >= 400 && parsed.code < 500) {
      return {
        outcome: "permanent_failure",
        status_code: parsed.code,
        diagnostic: `Bark API rejected delivery (${parsed.code}): ${msg}`,
      };
    }
    return {
      outcome: "retry",
      status_code: parsed.code,
      diagnostic: `Bark API error (${parsed.code}): ${msg}`,
    };
  }

  return { outcome: "delivered", status_code: resp.status };
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
    const username = typeof config.username === "string" ? config.username.trim() : "";
    const password = typeof config.password === "string" ? config.password : "";
    if (/[\r\n\x00-\x1f\x7f]/.test(username) || /[\r\n\x00-\x1f\x7f]/.test(password)) {
      return { ok: false, error: "Basic Auth credentials must not contain control characters" };
    }
    if (Boolean(username) !== Boolean(password)) {
      return {
        ok: false,
        error: "Both username and password are required when configuring HTTP Basic Auth",
      };
    }
    const normalized = { server_url: normalizedUrl };
    if (username && password) {
      normalized.username = username;
      normalized.password = password;
    }
    return { ok: true, normalized_config: normalized };
  }

  if (scope === "destination") {
    const deviceKey = String(config.device_key ?? "").trim();
    if (!deviceKey) {
      return { ok: false, error: "device_key is required" };
    }
    if (deviceKey.indexOf("://") !== -1 || /[/?#]/.test(deviceKey)) {
      return {
        ok: false,
        error: "device_key must be the Bark Device Key itself, not a full URL",
      };
    }
    if (!DEVICE_KEY_RE.test(deviceKey)) {
      return {
        ok: false,
        error: "device_key must be 1–256 printable ASCII characters without spaces",
      };
    }

    const level = String(config.level ?? "active").trim() || "active";
    if (!VALID_LEVELS.has(level)) {
      return {
        ok: false,
        error: `level must be one of: ${Array.from(VALID_LEVELS).join(", ")}`,
      };
    }

    const normalized = {
      device_key: deviceKey,
      level,
      send_icon: config.send_icon !== false,
    };

    if (typeof config.group === "string" && config.group.trim()) {
      const group = collapseSingleLine(config.group).slice(0, 128);
      if (group) {
        normalized.group = group;
      }
    }

    if (typeof config.sound === "string" && config.sound.trim()) {
      const sound = config.sound.trim();
      if (!SOUND_RE.test(sound)) {
        return {
          ok: false,
          error: "sound must contain only ASCII letters, digits, '.', '-' or '_' (max 64 chars)",
        };
      }
      normalized.sound = sound;
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
        url: `${serverUrl}/ping`,
        headers: buildAuthHeaders(ctx?.account),
        timeoutMs: 8000,
      });
    } catch (err) {
      return {
        ok: false,
        error: `Bark health check failed: ${err?.message ?? String(err)}`,
      };
    }
    if (!resp.ok) {
      return {
        ok: false,
        error: `Bark /ping returned HTTP ${resp.status}`,
      };
    }
    const rawText = typeof resp.body === "string" ? resp.body : "";
    let parsed = null;
    try {
      if (rawText.trim()) {
        parsed = JSON.parse(rawText);
      }
    } catch {
      // Plain text response
    }
    const okJson = parsed && (parsed.code === 200 || parsed.message === "pong");
    const okText = rawText.toLowerCase().indexOf("pong") !== -1;
    if (!okJson && !okText) {
      return {
        ok: false,
        error: `Unexpected Bark /ping response: ${rawText.slice(0, 120)}`,
      };
    }
    return { ok: true, message: `Bark server ${serverUrl} reachable` };
  }

  const res = sendBarkPush({
    account: ctx?.account,
    destination: ctx?.destination,
    entry: {
      title: "FeedCourier Test Notification",
      summary_text: "Bark Compact Card delivery is configured and working.",
      url: "https://feedcourier.ellery.me",
      author: "FeedCourier",
    },
    feed: {
      title: "FeedCourier",
    },
  });
  if (res.outcome === "delivered") {
    return { ok: true, message: "Test push sent via Bark" };
  }
  return { ok: false, error: res.diagnostic ?? "Bark test failed" };
}

export function deliver(ctx) {
  return sendBarkPush({
    account: ctx?.account,
    destination: ctx?.destination,
    entry: ctx?.entry ?? {},
    feed: ctx?.feed ?? {},
  });
}
