import type {
  AccountDto,
  ActionAckResponse,
  ApiErrorBody,
  AuthLogin2faRequest,
  AuthLoginRequest,
  AuthLoginResponse,
  AuthPasswordRequest,
  AuthStatusResponse,
  ChannelDestinationDto,
  ChannelDto,
  ClearFeedCacheRequest,
  ClearFeedCacheResponse,
  ClearPluginCacheResponse,
  CreateChannelRequest,
  CreateDestinationRequest,
  CreateFeedRequest,
  CreateMcpTokenRequest,
  CreateMcpTokenResponse,
  CreatePluginInstanceRequest,
  CreateRouteRequest,
  DeliveryTaskDto,
  FeedDto,
  FeedPreviewPushRequest,
  FeedPreviewPushResponse,
  FeedPreviewRequest,
  FeedPreviewResponse,
  GcResponse,
  ListTasksParams,
  MarketResponse,
  McpTokenDto,
  NotionInitDbRequest,
  NotionInitDbResponse,
  NotionInspectSchemaRequest,
  NotionInspectSchemaResponse,
  OpmlImportRequest,
  OpmlImportResponse,
  OverviewResponse,
  PluginDto,
  PluginInstanceDto,
  PollFeedResponse,
  QueueStateDto,
  RecoveryCodesResponse,
  RegenerateRecoveryCodesRequest,
  RetryTasksRequest,
  RetryTasksResponse,
  RouteDto,
  RouteReferenceDto,
  SendVerificationResponse,
  SmtpRecipientDto,
  SystemSettingsDto,
  TestChannelRequest,
  TestChannelResponse,
  TestDestinationRequest,
  TestPluginInstanceResponse,
  TotpDisableRequest,
  TotpEnableRequest,
  TotpSetupRequest,
  TotpSetupResponse,
  UpdateAccountRequest,
  UpdateChannelRequest,
  UpdateDestinationRequest,
  UpdateFeedRequest,
  UpdatePluginInstanceRequest,
  UpdatePluginRequest,
  UpdateQueueRequest,
  UpdateRouteRequest,
  UpdateSystemSettingsRequest,
} from "./types";
import { getMessages } from "@/i18n";

const API_BASE = "/api/v1";

export class ApiError extends Error {
  readonly status: number;
  readonly code?: string;
  readonly referencing_routes?: RouteReferenceDto[];
  readonly retry_after_secs?: number;

  constructor(
    status: number,
    message: string,
    code?: string,
    referencingRoutes?: RouteReferenceDto[],
    retryAfterSecs?: number,
  ) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.referencing_routes = referencingRoutes;
    this.retry_after_secs = retryAfterSecs;
  }
}

function parseRetryAfterSecs(header: string | null): number | undefined {
  if (!header) return undefined;
  const trimmed = header.trim();
  if (!trimmed) return undefined;
  if (/^\d+$/.test(trimmed)) {
    const secs = Number(trimmed);
    return Number.isFinite(secs) && secs > 0 ? secs : undefined;
  }
  const parsedDate = Date.parse(trimmed);
  if (Number.isFinite(parsedDate)) {
    const delta = Math.ceil((parsedDate - Date.now()) / 1000);
    return delta > 0 ? delta : undefined;
  }
  return undefined;
}

/** Extract referencing routes from a structured HTTP 409 Conflict `ApiError`, if present. */
export function getReferencingRoutesConflict(
  err: unknown,
): RouteReferenceDto[] | null {
  if (
    err instanceof ApiError &&
    err.status === 409 &&
    Array.isArray(err.referencing_routes) &&
    err.referencing_routes.length > 0
  ) {
    return err.referencing_routes;
  }
  return null;
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
  const retryAfterSecs = parseRetryAfterSecs(res.headers.get("retry-after"));
  const contentType = res.headers.get("content-type") ?? "";
  if (contentType.includes("application/json")) {
    try {
      const data = (await res.json()) as Partial<ApiErrorBody>;
      const message =
        data.message?.trim() ||
        data.error?.trim() ||
        getMessages().common.requestFailedHttp(res.status);
      const referencingRoutes = Array.isArray(data.referencing_routes)
        ? data.referencing_routes.filter(
            (r): r is RouteReferenceDto =>
              typeof r === "object" &&
              r !== null &&
              typeof r.id === "number" &&
              typeof r.name === "string",
          )
        : undefined;
      return new ApiError(
        res.status,
        message,
        data.code,
        referencingRoutes,
        retryAfterSecs,
      );
    } catch {
      // Fall through to text fallback
    }
  }
  const text = (await res.text().catch(() => "")).trim();
  return new ApiError(
    res.status,
    text.length > 0 && text.length < 300
      ? text
      : getMessages().common.requestFailedHttp(res.status),
    undefined,
    undefined,
    retryAfterSecs,
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
    login: (payload: AuthLoginRequest) =>
      request<AuthLoginResponse | undefined>("/auth/login", {
        method: "POST",
        body: payload,
        skipUnauthorizedHook: true,
      }),
    login2fa: (payload: AuthLogin2faRequest) =>
      request<ActionAckResponse | undefined>("/auth/login/2fa", {
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
  // Account & Security
  // -------------------------------------------------------------------------
  account: {
    get: () => request<AccountDto>("/account", { method: "GET" }),
    update: (payload: UpdateAccountRequest) =>
      request<AccountDto>("/account", { method: "PATCH", body: payload }),
    totpSetup: (payload: TotpSetupRequest) =>
      request<TotpSetupResponse>("/account/2fa/setup", {
        method: "POST",
        body: payload,
      }),
    totpEnable: (payload: TotpEnableRequest) =>
      request<RecoveryCodesResponse>("/account/2fa/enable", {
        method: "POST",
        body: payload,
      }),
    totpDisable: (payload: TotpDisableRequest) =>
      request<AccountDto>("/account/2fa/disable", {
        method: "POST",
        body: payload,
      }),
    regenerateRecoveryCodes: (payload: RegenerateRecoveryCodesRequest) =>
      request<RecoveryCodesResponse>("/account/2fa/recovery-codes", {
        method: "POST",
        body: payload,
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
    clearCache: (id: number, payload: ClearFeedCacheRequest = {}) =>
      request<ClearFeedCacheResponse>(`/feeds/${id}/clear-cache`, {
        method: "POST",
        body: payload,
      }),
    previewPush: (payload: FeedPreviewPushRequest) =>
      request<FeedPreviewPushResponse>("/feeds/preview/push", {
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
  // Trusted Channels (Accounts) & Destinations
  // -------------------------------------------------------------------------
  channels: {
    list: () => request<ChannelDto[]>("/channels", { method: "GET" }),
    create: (payload: CreateChannelRequest) =>
      request<ChannelDto>("/channels", { method: "POST", body: payload }),
    update: (id: number, payload: UpdateChannelRequest) =>
      request<ChannelDto>(`/channels/${id}`, { method: "PATCH", body: payload }),
    remove: (id: number, cascade = false) =>
      request<ActionAckResponse | undefined>(
        `/channels/${id}${cascade ? "?cascade=true" : ""}`,
        {
          method: "DELETE",
        },
      ),
    test: (id: number, payload?: TestChannelRequest) =>
      request<TestChannelResponse>(`/channels/${id}/test`, {
        method: "POST",
        body: payload,
      }),
    listDestinations: (channelId: number) =>
      request<ChannelDestinationDto[]>(`/channels/${channelId}/destinations`, {
        method: "GET",
      }),
    createDestination: (channelId: number, payload: CreateDestinationRequest) =>
      request<ChannelDestinationDto>(`/channels/${channelId}/destinations`, {
        method: "POST",
        body: payload,
      }),
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

  destinations: {
    update: (id: number, payload: UpdateDestinationRequest) =>
      request<ChannelDestinationDto>(`/destinations/${id}`, {
        method: "PATCH",
        body: payload,
      }),
    remove: (id: number, cascade = false) =>
      request<ActionAckResponse | undefined>(
        `/destinations/${id}${cascade ? "?cascade=true" : ""}`,
        {
          method: "DELETE",
        },
      ),
    test: (id: number, payload?: TestDestinationRequest) =>
      request<TestChannelResponse>(`/destinations/${id}/test`, {
        method: "POST",
        body: payload,
      }),
    listRecipients: (destId: number) =>
      request<SmtpRecipientDto[]>(`/destinations/${destId}/recipients`, {
        method: "GET",
      }),
    sendRecipientVerification: (destId: number, rid: number) =>
      request<SendVerificationResponse>(
        `/destinations/${destId}/recipients/${rid}/send-verification`,
        {
          method: "POST",
        },
      ),
    verifyRecipientCode: (destId: number, rid: number, code: string) =>
      request<SmtpRecipientDto>(
        `/destinations/${destId}/recipients/${rid}/verify`,
        {
          method: "POST",
          body: { code },
        },
      ),
    reactivateRecipient: (destId: number, rid: number) =>
      request<SmtpRecipientDto>(
        `/destinations/${destId}/recipients/${rid}/reactivate`,
        {
          method: "POST",
        },
      ),
    unsubscribeRecipientAdmin: (destId: number, rid: number) =>
      request<SmtpRecipientDto>(
        `/destinations/${destId}/recipients/${rid}/unsubscribe`,
        {
          method: "POST",
        },
      ),
  },

  // -------------------------------------------------------------------------
  // Plugins & Marketplace
  // -------------------------------------------------------------------------
  plugins: {
    list: () => request<PluginDto[]>("/plugins", { method: "GET" }),
    update: (id: string, payload: UpdatePluginRequest) =>
      request<PluginDto>(`/plugins/${encodeURIComponent(id)}`, {
        method: "PATCH",
        body: payload,
      }),
    listInstances: (id: string) =>
      request<PluginInstanceDto[]>(
        `/plugins/${encodeURIComponent(id)}/instances`,
        { method: "GET" },
      ),
    createInstance: (id: string, payload: CreatePluginInstanceRequest) =>
      request<PluginInstanceDto>(
        `/plugins/${encodeURIComponent(id)}/instances`,
        {
          method: "POST",
          body: payload,
        },
      ),
    updateInstance: (
      id: string,
      iid: number,
      payload: UpdatePluginInstanceRequest,
    ) =>
      request<PluginInstanceDto>(
        `/plugins/${encodeURIComponent(id)}/instances/${iid}`,
        {
          method: "PATCH",
          body: payload,
        },
      ),
    deleteInstance: (id: string, iid: number) =>
      request<ActionAckResponse | undefined>(
        `/plugins/${encodeURIComponent(id)}/instances/${iid}`,
        {
          method: "DELETE",
        },
      ),
    testInstance: (id: string, iid: number) =>
      request<TestPluginInstanceResponse>(
        `/plugins/${encodeURIComponent(id)}/instances/${iid}/test`,
        {
          method: "POST",
        },
      ),
    clearInstanceCache: (id: string, iid: number) =>
      request<ClearPluginCacheResponse>(
        `/plugins/${encodeURIComponent(id)}/instances/${iid}/cache`,
        {
          method: "DELETE",
        },
      ),
    market: (refresh = false) =>
      request<MarketResponse>(
        `/plugins/market${refresh ? "?refresh=true" : ""}`,
        { method: "GET" },
      ),
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
    queue: () => request<QueueStateDto>("/tasks/queue", { method: "GET" }),
    updateQueue: (payload: UpdateQueueRequest) =>
      request<QueueStateDto>("/tasks/queue", { method: "PUT", body: payload }),
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

// Convenience named exports for v0.3.0 endpoints
export const deleteChannel = api.channels.remove;
export const deleteDestination = api.destinations.remove;
export const listDestinationRecipients = api.destinations.listRecipients;
export const sendRecipientVerification =
  api.destinations.sendRecipientVerification;
export const verifyRecipientCode = api.destinations.verifyRecipientCode;
export const reactivateRecipient = api.destinations.reactivateRecipient;
export const unsubscribeRecipientAdmin =
  api.destinations.unsubscribeRecipientAdmin;

export const listPlugins = api.plugins.list;
export const updatePlugin = api.plugins.update;
export const listPluginInstances = api.plugins.listInstances;
export const createPluginInstance = api.plugins.createInstance;
export const updatePluginInstance = api.plugins.updateInstance;
export const deletePluginInstance = api.plugins.deleteInstance;
export const testPluginInstance = api.plugins.testInstance;
export const clearPluginInstanceCache = api.plugins.clearInstanceCache;
export const getPluginMarket = api.plugins.market;
