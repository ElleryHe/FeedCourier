import type {
  ActionAckResponse,
  ApiErrorBody,
  AuthPasswordRequest,
  AuthStatusResponse,
  ChannelDto,
  CreateChannelRequest,
  CreateFeedRequest,
  CreateMcpTokenRequest,
  CreateMcpTokenResponse,
  CreateRouteRequest,
  DeliveryTaskDto,
  FeedDto,
  FeedPreviewRequest,
  FeedPreviewResponse,
  GcResponse,
  ListTasksParams,
  McpTokenDto,
  NotionInitDbRequest,
  NotionInitDbResponse,
  NotionInspectSchemaRequest,
  NotionInspectSchemaResponse,
  OpmlImportRequest,
  OpmlImportResponse,
  OverviewResponse,
  PollFeedResponse,
  RetryTasksRequest,
  RetryTasksResponse,
  RouteDto,
  SystemSettingsDto,
  TestChannelResponse,
  UpdateChannelRequest,
  UpdateFeedRequest,
  UpdateRouteRequest,
  UpdateSystemSettingsRequest,
} from "./types";

const API_BASE = "/api/v1";

export class ApiError extends Error {
  readonly status: number;
  readonly code?: string;
  readonly details?: string;

  constructor(status: number, message: string, code?: string, details?: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.details = details;
  }
}

type UnauthorizedListener = () => void;
const unauthorizedListeners = new Set<UnauthorizedListener>();

export function onUnauthorized(listener: UnauthorizedListener): () => void {
  unauthorizedListeners.add(listener);
  return () => {
    unauthorizedListeners.delete(listener);
  };
}

function notifyUnauthorized(): void {
  for (const listener of unauthorizedListeners) {
    listener();
  }
}

async function parseErrorResponse(res: Response): Promise<ApiError> {
  const contentType = res.headers.get("content-type") ?? "";
  if (contentType.includes("application/json")) {
    try {
      const data = (await res.json()) as Partial<ApiErrorBody> & {
        message?: string;
      };
      const message =
        data.error ?? data.message ?? `Request failed with HTTP ${res.status}`;
      return new ApiError(res.status, message, data.code, data.details);
    } catch {
      // Fall through to text fallback
    }
  }
  const text = (await res.text().catch(() => "")).trim();
  return new ApiError(
    res.status,
    text.length > 0 && text.length < 300
      ? text
      : `Request failed with HTTP ${res.status}`,
  );
}

interface RequestOptions extends Omit<RequestInit, "body"> {
  body?: unknown;
  skipUnauthorizedHook?: boolean;
}

async function request<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const { body, headers, skipUnauthorizedHook = false, ...rest } = options;
  const reqHeaders = new Headers(headers);
  reqHeaders.set("Accept", "application/json");

  let serializedBody: BodyInit | undefined;
  if (body !== undefined) {
    reqHeaders.set("Content-Type", "application/json");
    serializedBody = JSON.stringify(body);
  }

  const res = await fetch(`${API_BASE}${path}`, {
    ...rest,
    credentials: "same-origin",
    headers: reqHeaders,
    body: serializedBody,
  });

  if (!res.ok) {
    if (res.status === 401 && !skipUnauthorizedHook) {
      notifyUnauthorized();
    }
    throw await parseErrorResponse(res);
  }

  if (res.status === 204) {
    return undefined as T;
  }

  const text = await res.text();
  if (!text) {
    return undefined as T;
  }
  return JSON.parse(text) as T;
}

function parseContentDispositionFilename(header: string | null): string | null {
  if (!header) return null;
  const utf8Match = /filename\*=UTF-8''([^;\s]+)/i.exec(header);
  if (utf8Match?.[1]) {
    try {
      return decodeURIComponent(utf8Match[1]);
    } catch {
      return utf8Match[1];
    }
  }
  const quotedMatch = /filename="([^"]+)"/i.exec(header);
  if (quotedMatch?.[1]) {
    return quotedMatch[1];
  }
  const plainMatch = /filename=([^;\s]+)/i.exec(header);
  return plainMatch?.[1] ?? null;
}

function triggerBrowserDownload(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 5000);
}

function formatCompactTimestamp(): string {
  const iso = new Date().toISOString(); // e.g. 2026-11-13T09:48:00.000Z
  return iso.replace(/[-:]/g, "").replace(/\.\d+Z$/, "Z");
}

export const api = {
  // -------------------------------------------------------------------------
  // Auth & Metadata
  // -------------------------------------------------------------------------
  auth: {
    status: () =>
      request<AuthStatusResponse>("/auth/status", {
        method: "GET",
        skipUnauthorizedHook: true,
      }),
    setup: (payload: AuthPasswordRequest) =>
      request<ActionAckResponse | undefined>("/auth/setup", {
        method: "POST",
        body: payload,
        skipUnauthorizedHook: true,
      }),
    login: (payload: AuthPasswordRequest) =>
      request<ActionAckResponse | undefined>("/auth/login", {
        method: "POST",
        body: payload,
        skipUnauthorizedHook: true,
      }),
    logout: () =>
      request<ActionAckResponse | undefined>("/auth/logout", {
        method: "POST",
        skipUnauthorizedHook: true,
      }),
  },

  // -------------------------------------------------------------------------
  // Overview & Maintenance
  // -------------------------------------------------------------------------
  overview: {
    get: () => request<OverviewResponse>("/overview", { method: "GET" }),
  },

  maintenance: {
    runGc: () => request<GcResponse>("/maintenance/gc", { method: "POST" }),
    downloadBackup: async (): Promise<string> => {
      const res = await fetch(`${API_BASE}/maintenance/backup`, {
        method: "GET",
        credentials: "same-origin",
      });
      if (!res.ok) {
        if (res.status === 401) notifyUnauthorized();
        throw await parseErrorResponse(res);
      }
      const blob = await res.blob();
      const filename =
        parseContentDispositionFilename(res.headers.get("content-disposition")) ??
        `feedcourier-snapshot-${formatCompactTimestamp()}.db`;
      triggerBrowserDownload(blob, filename);
      return filename;
    },
  },

  // -------------------------------------------------------------------------
  // Feeds & OPML
  // -------------------------------------------------------------------------
  feeds: {
    list: () => request<FeedDto[]>("/feeds", { method: "GET" }),
    create: (payload: CreateFeedRequest) =>
      request<FeedDto>("/feeds", { method: "POST", body: payload }),
    update: (id: number, payload: UpdateFeedRequest) =>
      request<FeedDto>(`/feeds/${id}`, { method: "PATCH", body: payload }),
    remove: (id: number) =>
      request<ActionAckResponse | undefined>(`/feeds/${id}`, {
        method: "DELETE",
      }),
    pollNow: (id: number) =>
      request<PollFeedResponse>(`/feeds/${id}/poll`, { method: "POST" }),
    preview: (payload: FeedPreviewRequest) =>
      request<FeedPreviewResponse>("/feeds/preview", {
        method: "POST",
        body: payload,
      }),
    importOpml: (payload: OpmlImportRequest) =>
      request<OpmlImportResponse>("/feeds/opml/import", {
        method: "POST",
        body: payload,
      }),
    exportOpml: async (): Promise<string> => {
      const res = await fetch(`${API_BASE}/feeds/opml/export`, {
        method: "GET",
        credentials: "same-origin",
        headers: {
          Accept: "application/xml, text/xml, application/json;q=0.8",
        },
      });
      if (!res.ok) {
        if (res.status === 401) notifyUnauthorized();
        throw await parseErrorResponse(res);
      }
      const contentType = res.headers.get("content-type") ?? "";
      let xmlContent: string;
      if (contentType.includes("application/json")) {
        const json = (await res.json()) as { opml_xml?: string };
        xmlContent = json.opml_xml ?? "";
      } else {
        xmlContent = await res.text();
      }
      const filename =
        parseContentDispositionFilename(res.headers.get("content-disposition")) ??
        `feedcourier-feeds-${formatCompactTimestamp()}.opml`;
      const blob = new Blob([xmlContent], {
        type: "text/x-opml;charset=utf-8",
      });
      triggerBrowserDownload(blob, filename);
      return filename;
    },
  },

  // -------------------------------------------------------------------------
  // Trusted Channels
  // -------------------------------------------------------------------------
  channels: {
    list: () => request<ChannelDto[]>("/channels", { method: "GET" }),
    create: (payload: CreateChannelRequest) =>
      request<ChannelDto>("/channels", { method: "POST", body: payload }),
    update: (id: number, payload: UpdateChannelRequest) =>
      request<ChannelDto>(`/channels/${id}`, { method: "PATCH", body: payload }),
    remove: (id: number) =>
      request<ActionAckResponse | undefined>(`/channels/${id}`, {
        method: "DELETE",
      }),
    test: (id: number) =>
      request<TestChannelResponse>(`/channels/${id}/test`, { method: "POST" }),
    notionInitDb: (payload: NotionInitDbRequest) =>
      request<NotionInitDbResponse>("/channels/notion/init-db", {
        method: "POST",
        body: payload,
      }),
    notionInspectSchema: (payload: NotionInspectSchemaRequest) =>
      request<NotionInspectSchemaResponse>("/channels/notion/inspect-schema", {
        method: "POST",
        body: payload,
      }),
  },

  // -------------------------------------------------------------------------
  // Routes
  // -------------------------------------------------------------------------
  routes: {
    list: () => request<RouteDto[]>("/routes", { method: "GET" }),
    create: (payload: CreateRouteRequest) =>
      request<RouteDto>("/routes", { method: "POST", body: payload }),
    update: (id: number, payload: UpdateRouteRequest) =>
      request<RouteDto>(`/routes/${id}`, { method: "PATCH", body: payload }),
    remove: (id: number) =>
      request<ActionAckResponse | undefined>(`/routes/${id}`, {
        method: "DELETE",
      }),
  },

  // -------------------------------------------------------------------------
  // Outbox & Dead-Letter Tasks
  // -------------------------------------------------------------------------
  tasks: {
    list: (params: ListTasksParams = {}) => {
      const search = new URLSearchParams();
      if (params.status) search.set("status", params.status);
      if (params.limit !== undefined) search.set("limit", String(params.limit));
      const qs = search.toString();
      return request<DeliveryTaskDto[]>(`/tasks${qs ? `?${qs}` : ""}`, {
        method: "GET",
      });
    },
    retry: (payload: RetryTasksRequest) =>
      request<RetryTasksResponse>("/tasks/retry", {
        method: "POST",
        body: payload,
      }),
  },

  // -------------------------------------------------------------------------
  // System Settings & MCP Tokens
  // -------------------------------------------------------------------------
  settings: {
    get: () => request<SystemSettingsDto>("/settings", { method: "GET" }),
    update: (payload: UpdateSystemSettingsRequest) =>
      request<SystemSettingsDto>("/settings", { method: "PUT", body: payload }),
  },

  mcpTokens: {
    list: () => request<McpTokenDto[]>("/mcp-tokens", { method: "GET" }),
    create: (payload: CreateMcpTokenRequest) =>
      request<CreateMcpTokenResponse>("/mcp-tokens", {
        method: "POST",
        body: payload,
      }),
    remove: (id: number) =>
      request<ActionAckResponse | undefined>(`/mcp-tokens/${id}`, {
        method: "DELETE",
      }),
  },
};
