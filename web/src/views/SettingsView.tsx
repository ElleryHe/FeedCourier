import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  Bot,
  Check,
  ChevronDown,
  Copy,
  Clock,
  Globe,
  KeyRound,
  Link2,
  Plus,
  Save,
  ShieldCheck,
  Trash2,
} from "lucide-react";
import { type FormEvent, useEffect, useMemo, useState } from "react";
import { api } from "@/api/client";
import type {
  CreateMcpTokenResponse,
  McpTokenDto,
  McpTokenScope,
} from "@/api/types";
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  ConfirmDialog,
  FieldError,
  FieldHint,
  Input,
  Label,
  Modal,
  TagInput,
} from "@/components/ui/primitives";
import { useToast } from "@/components/ui/toast";
import { useI18n } from "@/i18n";
import {
  copyToClipboard,
  errorMessage,
  formatDateTime,
  formatRelativeTime,
  isValidHttpUrl,
  isValidProxyUrl,
} from "@/lib/utils";
import {
  browserTimeZone,
  currentTimeIn,
  listTimeZones,
  utcOffsetLabel,
} from "@/lib/timezone";
import { AccountSecurityCard } from "@/views/AccountSecurityCard";

export function SettingsView() {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();

  // --- System Settings (`GET` & `PUT /api/v1/settings`) ---
  const settingsQuery = useQuery({
    queryKey: ["settings"],
    queryFn: () => api.settings.get(),
  });

  const [proxyUrl, setProxyUrl] = useState("");
  const [ssrfAllowlist, setSsrfAllowlist] = useState<string[]>([]);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [timezone, setTimezone] = useState("");
  const [timezoneError, setTimezoneError] = useState<string | null>(null);
  const [publicUrl, setPublicUrl] = useState("");
  const [publicUrlError, setPublicUrlError] = useState<string | null>(null);
  const timeZones = useMemo(() => listTimeZones(), []);
  const detectedZone = useMemo(() => browserTimeZone(), []);

  useEffect(() => {
    if (settingsQuery.data) {
      setTimezone(settingsQuery.data.timezone ?? "");
      setProxyUrl(settingsQuery.data.outbound_proxy_url ?? "");
      setSsrfAllowlist(settingsQuery.data.ssrf_allowlist);
      setPublicUrl(settingsQuery.data.public_url ?? "");
    }
  }, [settingsQuery.data]);

  const publicUrlEnvManaged =
    settingsQuery.data?.public_url_env_managed === true;
  const isNonHttpsPublicUrl = publicUrl
    .trim()
    .toLowerCase()
    .startsWith("http://");

  const savePublicUrlMutation = useMutation({
    mutationFn: (nextUrl: string | null) =>
      api.settings.update({ public_url: nextUrl }),
    onSuccess: (updated) => {
      toast.success(m.settings.toastPublicUrlSaved);
      queryClient.setQueryData(["settings"], updated);
    },
    onError: (err) => {
      setPublicUrlError(errorMessage(err));
    },
  });

  const handleSavePublicUrl = (e: FormEvent) => {
    e.preventDefault();
    setPublicUrlError(null);
    if (publicUrlEnvManaged) return;
    const trimmed = publicUrl.trim();
    if (trimmed && !isValidHttpUrl(trimmed)) {
      setPublicUrlError(m.settings.errInvalidPublicUrl);
      return;
    }
    savePublicUrlMutation.mutate(trimmed ? trimmed : null);
  };

  const saveSettingsMutation = useMutation({
    mutationFn: () =>
      api.settings.update({
        outbound_proxy_url: proxyUrl.trim() ? proxyUrl.trim() : null,
        ssrf_allowlist: ssrfAllowlist,
      }),
    onSuccess: () => {
      toast.success(m.settings.toastSettingsSaved);
      void queryClient.invalidateQueries({ queryKey: ["settings"] });
    },
    onError: (err) => {
      setSettingsError(errorMessage(err));
    },
  });

  const saveTimezoneMutation = useMutation({
    mutationFn: (tz: string) => api.settings.update({ timezone: tz }),
    onSuccess: (updated) => {
      toast.success(m.settings.toastTimezoneSaved);
      queryClient.setQueryData(["settings"], updated);
    },
    onError: (err) => {
      setTimezoneError(errorMessage(err));
    },
  });

  const handleSaveTimezone = (e: FormEvent) => {
    e.preventDefault();
    setTimezoneError(null);
    const tz = timezone.trim();
    if (!tz || !timeZones.includes(tz)) {
      setTimezoneError(m.settings.errInvalidTimezone);
      return;
    }
    saveTimezoneMutation.mutate(tz);
  };

  const timezoneNow = timeZones.includes(timezone.trim())
    ? currentTimeIn(timezone.trim())
    : null;
  const timezoneOffset = timeZones.includes(timezone.trim())
    ? utcOffsetLabel(timezone.trim())
    : null;

  const handleSaveSettings = (e: FormEvent) => {
    e.preventDefault();
    setSettingsError(null);
    if (proxyUrl.trim() && !isValidProxyUrl(proxyUrl)) {
      setSettingsError(m.settings.errInvalidProxy);
      return;
    }
    saveSettingsMutation.mutate();
  };

  // --- MCP Tokens (`GET`, `POST`, `DELETE /api/v1/mcp-tokens`) ---
  const tokensQuery = useQuery({
    queryKey: ["mcp-tokens"],
    queryFn: () => api.mcpTokens.list(),
  });

  const [tokenName, setTokenName] = useState("");
  const [tokenScope, setTokenScope] = useState<McpTokenScope>("readonly");
  const [showToolsList, setShowToolsList] = useState(false);
  const [tokenFormError, setTokenFormError] = useState<string | null>(null);
  const [createdTokenModal, setCreatedTokenModal] =
    useState<CreateMcpTokenResponse | null>(null);
  const [clientConfigTab, setClientConfigTab] = useState<
    "claude" | "cursor" | "generic"
  >("claude");
  const [revokingToken, setRevokingToken] = useState<McpTokenDto | null>(null);
  const [copiedField, setCopiedField] = useState<
    "endpoint" | "token" | "json" | null
  >(null);

  const PUBLIC_READONLY_TOOLS = [
    "list_feeds_status",
    "inspect_failed_deliveries",
    "preview_feed_latest",
    "list_plugins",
  ] as const;
  const PUBLIC_ADMIN_EXTRA_TOOLS = [
    "manage_feed",
    "configure_route",
    "reset_and_poll_feed",
    "retry_dead_tasks",
    "manage_outbox_queue",
    "test_push_feed_entry",
  ] as const;
  const visibleScopeTools =
    tokenScope === "admin"
      ? [...PUBLIC_READONLY_TOOLS, ...PUBLIC_ADMIN_EXTRA_TOOLS]
      : [...PUBLIC_READONLY_TOOLS];

  const createTokenMutation = useMutation({
    mutationFn: () =>
      api.mcpTokens.create({
        name: tokenName.trim(),
        scope: tokenScope,
      }),
    onSuccess: (res) => {
      setTokenName("");
      setClientConfigTab("claude");
      setCreatedTokenModal(res);
      toast.success(m.settings.toastTokenIssued);
      void queryClient.invalidateQueries({ queryKey: ["mcp-tokens"] });
    },
    onError: (err) => {
      setTokenFormError(errorMessage(err));
    },
  });

  const revokeTokenMutation = useMutation({
    mutationFn: (id: number) => api.mcpTokens.remove(id),
    onSuccess: () => {
      toast.success(m.settings.toastTokenRevoked);
      setRevokingToken(null);
      void queryClient.invalidateQueries({ queryKey: ["mcp-tokens"] });
    },
    onError: (err) => {
      toast.error(m.settings.toastRevokeFailed, errorMessage(err));
    },
  });

  const handleIssueToken = (e: FormEvent) => {
    e.preventDefault();
    setTokenFormError(null);
    if (!tokenName.trim()) {
      setTokenFormError(m.settings.errEmptyTokenLabel);
      return;
    }
    createTokenMutation.mutate();
  };

  const handleCopy = async (
    kind: "endpoint" | "token" | "json",
    value: string,
  ) => {
    const ok = await copyToClipboard(value);
    if (ok) {
      setCopiedField(kind);
      toast.success(
        kind === "endpoint"
          ? m.settings.toastEndpointCopied
          : kind === "token"
            ? m.settings.toastTokenCopied
            : m.settings.toastJsonCopied,
      );
      window.setTimeout(() => setCopiedField(null), 2000);
    }
  };

  const mcpEndpointUrl =
    typeof window !== "undefined"
      ? `${window.location.origin}/mcp`
      : "http://127.0.0.1:8080/mcp";

  const buildClientConfigSnippet = (
    tab: "claude" | "cursor" | "generic",
    modal: CreateMcpTokenResponse,
  ): string => {
    if (tab === "claude") {
      return JSON.stringify(
        {
          mcpServers: {
            feedcourier: {
              command: "npx",
              args: [
                "-y",
                "mcp-remote",
                mcpEndpointUrl,
                "--header",
                `Authorization: Bearer ${modal.plaintext_token}`,
              ],
            },
          },
        },
        null,
        2,
      );
    }
    if (tab === "cursor") {
      return JSON.stringify(
        {
          mcpServers: {
            feedcourier: {
              url: mcpEndpointUrl,
              headers: {
                Authorization: `Bearer ${modal.plaintext_token}`,
              },
            },
          },
        },
        null,
        2,
      );
    }
    return (
      modal.client_config_json ||
      JSON.stringify(
        {
          mcpServers: {
            feedcourier: {
              type: "streamable-http",
              url: mcpEndpointUrl,
              headers: {
                Authorization: `Bearer ${modal.plaintext_token}`,
              },
            },
          },
        },
        null,
        2,
      )
    );
  };

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-bold tracking-tight">{m.settings.title}</h1>
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          {m.settings.subtitle}
        </p>
      </div>

      {/* Section 0: System Timezone */}
      <Card>
        <CardHeader>
          <div className="flex items-center gap-2">
            <Clock className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
            <CardTitle>{m.settings.timezoneTitle}</CardTitle>
          </div>
          <CardDescription>{m.settings.timezoneDesc}</CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleSaveTimezone} className="space-y-3">
            <div>
              <Label htmlFor="system-timezone">
                {m.settings.timezoneLabel}
              </Label>
              <div className="mt-1.5 flex flex-wrap items-center gap-2">
                <Input
                  id="system-timezone"
                  list="fc-timezone-options"
                  value={timezone}
                  onChange={(e) => {
                    setTimezone(e.target.value);
                    setTimezoneError(null);
                  }}
                  placeholder={m.settings.timezonePlaceholder}
                  autoComplete="off"
                  spellCheck={false}
                  className="max-w-xs font-mono text-xs"
                />
                <datalist id="fc-timezone-options">
                  {timeZones.map((tz) => (
                    <option key={tz} value={tz} />
                  ))}
                </datalist>
                {detectedZone && detectedZone !== timezone && (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => {
                      setTimezone(detectedZone);
                      setTimezoneError(null);
                    }}
                  >
                    {m.settings.timezoneUseBrowser(detectedZone)}
                  </Button>
                )}
                {timezoneNow && (
                  <Badge variant="mono">
                    {m.settings.timezoneNow(timezoneNow, timezoneOffset)}
                  </Badge>
                )}
              </div>
              <FieldHint>{m.settings.timezoneHint}</FieldHint>
            </div>
            <FieldError message={timezoneError} />
            <div className="flex justify-end">
              <Button
                type="submit"
                size="sm"
                loading={saveTimezoneMutation.isPending}
                disabled={
                  !settingsQuery.data ||
                  timezone.trim() === settingsQuery.data.timezone
                }
              >
                <Save className="h-3.5 w-3.5" />
                {m.settings.saveTimezone}
              </Button>
            </div>
          </form>
        </CardContent>
      </Card>

      {/* Section 0b: Public Base URL (PUBLIC_URL) */}
      <Card>
        <CardHeader>
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <Link2 className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
              <CardTitle>{m.settings.publicUrlTitle}</CardTitle>
            </div>
            {publicUrlEnvManaged && (
              <Badge variant="info">{m.settings.publicUrlEnvManagedBadge}</Badge>
            )}
          </div>
          <CardDescription>{m.settings.publicUrlDesc}</CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleSavePublicUrl} className="space-y-3">
            <div>
              <Label htmlFor="system-public-url">
                {m.settings.publicUrlLabel}
              </Label>
              <Input
                id="system-public-url"
                value={publicUrl}
                disabled={publicUrlEnvManaged}
                onChange={(e) => {
                  setPublicUrl(e.target.value);
                  setPublicUrlError(null);
                }}
                placeholder={m.settings.publicUrlPlaceholder}
                autoComplete="off"
                spellCheck={false}
                className="mt-1.5 font-mono text-xs"
              />
              <FieldHint>
                {publicUrlEnvManaged
                  ? m.settings.publicUrlEnvManagedHint
                  : m.settings.publicUrlHint}
              </FieldHint>
            </div>

            {isNonHttpsPublicUrl && (
              <div className="flex items-start gap-2 rounded-lg border border-amber-200 bg-amber-50/80 px-3 py-2 text-xs text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-200">
                <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400" />
                <span>{m.settings.publicUrlHttpWarning}</span>
              </div>
            )}

            <FieldError message={publicUrlError} />

            <div className="flex justify-end">
              <Button
                type="submit"
                size="sm"
                loading={savePublicUrlMutation.isPending}
                disabled={
                  !settingsQuery.data ||
                  publicUrlEnvManaged ||
                  publicUrl.trim() === (settingsQuery.data.public_url ?? "")
                }
              >
                <Save className="h-3.5 w-3.5" />
                {m.settings.savePublicUrl}
              </Button>
            </div>
          </form>
        </CardContent>
      </Card>

      {/* Section 0c: Account & Security */}
      <AccountSecurityCard />

      {/* Section 1: Outbound Proxy & SSRF Guardrails */}
      <Card>
        <CardHeader>
          <div className="flex items-center gap-2">
            <Globe className="h-4 w-4 text-zinc-700 dark:text-zinc-300" />
            <CardTitle>{m.settings.networkTitle}</CardTitle>
          </div>
          <CardDescription>{m.settings.networkDesc}</CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleSaveSettings} className="space-y-4">
            <div>
              <Label htmlFor="outbound-proxy">{m.settings.proxyLabel}</Label>
              <Input
                id="outbound-proxy"
                value={proxyUrl}
                onChange={(e) => {
                  setProxyUrl(e.target.value);
                  setSettingsError(null);
                }}
                placeholder={m.settings.proxyPlaceholder}
                className="mt-1.5 font-mono text-xs"
              />
              <FieldHint>{m.settings.proxyHint}</FieldHint>
            </div>

            <div>
              <Label>{m.settings.ssrfLabel}</Label>
              <div className="mt-1.5">
                <TagInput
                  values={ssrfAllowlist}
                  onChange={setSsrfAllowlist}
                  mono
                  placeholder={m.settings.ssrfPlaceholder}
                />
              </div>
              <FieldHint>{m.settings.ssrfHint}</FieldHint>
            </div>

            <FieldError message={settingsError} />

            <div className="flex justify-end">
              <Button
                type="submit"
                size="sm"
                loading={saveSettingsMutation.isPending}
              >
                <Save className="h-3.5 w-3.5" />
                {m.settings.saveNetwork}
              </Button>
            </div>
          </form>
        </CardContent>
      </Card>

      {/* Section 2: MCP Server & Bearer Tokens */}
      <Card>
        <CardHeader>
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div className="min-w-0 space-y-1">
              <div className="flex items-center gap-2">
                <Bot className="h-4 w-4 shrink-0 text-emerald-600 dark:text-emerald-400" />
                <CardTitle>{m.settings.mcpTitle}</CardTitle>
              </div>
              <CardDescription>{m.settings.mcpDesc}</CardDescription>
            </div>
            <button
              type="button"
              onClick={() => void handleCopy("endpoint", mcpEndpointUrl)}
              title={m.settings.copyEndpoint}
              className="inline-flex max-w-full cursor-pointer items-center gap-1.5 rounded-full border border-zinc-200 bg-zinc-50 px-3 py-1 font-mono text-[11px] text-zinc-700 transition-colors hover:border-zinc-300 hover:bg-zinc-100 dark:border-zinc-700 dark:bg-zinc-900 dark:text-zinc-300 dark:hover:border-zinc-600 dark:hover:bg-zinc-800"
            >
              <span className="truncate">{mcpEndpointUrl}</span>
              {copiedField === "endpoint" ? (
                <Check className="h-3 w-3 shrink-0 text-emerald-600 dark:text-emerald-400" />
              ) : (
                <Copy className="h-3 w-3 shrink-0 text-zinc-400" />
              )}
            </button>
          </div>
        </CardHeader>

        <CardContent className="space-y-4">
          {/* Compact Single-Row Issue Token Form */}
          <form
            onSubmit={handleIssueToken}
            className="space-y-2.5 rounded-xl border border-zinc-200 bg-zinc-50/60 p-3.5 dark:border-zinc-800 dark:bg-zinc-950/40"
          >
            <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
              <Input
                id="mcp-token-name"
                value={tokenName}
                onChange={(e) => {
                  setTokenName(e.target.value);
                  setTokenFormError(null);
                }}
                placeholder={m.settings.clientPlaceholder}
                aria-label={m.settings.clientLabel}
                className="min-w-0 flex-1"
              />

              <div
                role="radiogroup"
                aria-label={m.settings.scopeLabel}
                className="inline-flex shrink-0 rounded-lg border border-zinc-200 bg-white p-0.5 dark:border-zinc-800 dark:bg-zinc-900"
              >
                <button
                  type="button"
                  role="radio"
                  aria-checked={tokenScope === "readonly"}
                  onClick={() => setTokenScope("readonly")}
                  className={`inline-flex cursor-pointer items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors ${
                    tokenScope === "readonly"
                      ? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
                      : "text-zinc-600 hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-zinc-200"
                  }`}
                >
                  <ShieldCheck className="h-3.5 w-3.5" />
                  {m.settings.scopeReadonlyShort}
                </button>
                <button
                  type="button"
                  role="radio"
                  aria-checked={tokenScope === "admin"}
                  onClick={() => setTokenScope("admin")}
                  className={`inline-flex cursor-pointer items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors ${
                    tokenScope === "admin"
                      ? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
                      : "text-zinc-600 hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-zinc-200"
                  }`}
                >
                  <KeyRound className="h-3.5 w-3.5" />
                  {m.settings.scopeAdminShort}
                </button>
              </div>

              <Button
                type="submit"
                size="sm"
                loading={createTokenMutation.isPending}
                className="shrink-0"
              >
                <Plus className="h-3.5 w-3.5" />
                {m.settings.issueToken}
              </Button>
            </div>

            <div className="flex flex-wrap items-center justify-between gap-2 text-[11px] text-zinc-500 dark:text-zinc-400">
              <span>
                {tokenScope === "admin"
                  ? m.settings.scopeAdminHint
                  : m.settings.scopeReadonlyHint}
              </span>
              <button
                type="button"
                onClick={() => setShowToolsList((prev) => !prev)}
                className="inline-flex shrink-0 cursor-pointer items-center gap-1 font-medium text-zinc-700 hover:text-zinc-900 dark:text-zinc-300 dark:hover:text-zinc-100"
              >
                {showToolsList
                  ? m.settings.hideTools
                  : m.settings.viewTools(visibleScopeTools.length)}
                <ChevronDown
                  className={`h-3 w-3 transition-transform ${showToolsList ? "rotate-180" : ""}`}
                />
              </button>
            </div>

            {showToolsList && (
              <div className="flex flex-wrap gap-1.5 border-t border-zinc-200/80 pt-2.5 dark:border-zinc-800/80">
                {visibleScopeTools.map((tool) => (
                  <span
                    key={tool}
                    className="rounded-md border border-zinc-200 bg-white px-2 py-0.5 font-mono text-[11px] text-zinc-700 dark:border-zinc-700 dark:bg-zinc-900 dark:text-zinc-300"
                  >
                    {tool}
                  </span>
                ))}
              </div>
            )}

            <FieldError message={tokenFormError} />
          </form>

          {/* Compact Issued Tokens List */}
          {tokensQuery.isLoading ? (
            <div className="py-4 text-center text-xs text-zinc-500">
              {m.settings.loadingTokens}
            </div>
          ) : (tokensQuery.data?.length ?? 0) === 0 ? (
            <div className="flex items-center gap-2.5 rounded-lg border border-zinc-200/80 bg-zinc-50/40 px-3.5 py-2.5 text-xs text-zinc-500 dark:border-zinc-800/80 dark:bg-zinc-950/30 dark:text-zinc-400">
              <KeyRound className="h-3.5 w-3.5 shrink-0 text-zinc-400" />
              <span>{m.settings.emptyTokensInline}</span>
            </div>
          ) : (
            <div className="divide-y divide-zinc-100 rounded-lg border border-zinc-200 dark:divide-zinc-800/80 dark:border-zinc-800">
              {(tokensQuery.data ?? []).map((tok) => (
                <div
                  key={tok.id}
                  className="flex flex-wrap items-center justify-between gap-2 px-3.5 py-2.5 text-xs hover:bg-zinc-50/60 dark:hover:bg-zinc-950/40"
                >
                  <div className="flex min-w-0 flex-wrap items-center gap-2">
                    <span className="font-medium text-zinc-900 dark:text-zinc-100">
                      {tok.name}
                    </span>
                    <span className="font-mono text-[11px] text-zinc-400">
                      {tok.token_prefix}••••
                    </span>
                    <Badge
                      variant={tok.scope === "admin" ? "warning" : "info"}
                    >
                      {tok.scope}
                    </Badge>
                  </div>

                  <div className="flex items-center gap-3 text-[11px] text-zinc-500">
                    <span title={formatDateTime(tok.created_at)}>
                      {m.settings.colCreated}: {formatRelativeTime(tok.created_at)}
                    </span>
                    <span
                      title={formatDateTime(tok.last_used_at)}
                      className="hidden sm:inline"
                    >
                      {m.settings.colLastUsed}:{" "}
                      {tok.last_used_at
                        ? formatRelativeTime(tok.last_used_at)
                        : m.common.never}
                    </span>
                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => setRevokingToken(tok)}
                      className="h-7 w-7 text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-950/40"
                      title={m.settings.revokeTitle}
                      aria-label={m.settings.revokeTitle}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </CardContent>
      </Card>

      {/* One-Time Plaintext Token & Multi-Client Config Reveal Modal */}
      {createdTokenModal && (
        <Modal
          open={createdTokenModal !== null}
          onClose={() => setCreatedTokenModal(null)}
          title={m.settings.revealTitle}
          description={m.settings.revealDesc}
          size="lg"
          footer={
            <Button onClick={() => setCreatedTokenModal(null)}>
              {m.settings.revealConfirm}
            </Button>
          }
        >
          <div className="space-y-4">
            <div>
              <div className="flex items-center justify-between">
                <Label>
                  {m.settings.plaintextLabel(createdTokenModal.token.scope)}
                </Label>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() =>
                    void handleCopy("token", createdTokenModal.plaintext_token)
                  }
                >
                  {copiedField === "token" ? (
                    <Check className="h-3.5 w-3.5 text-emerald-600" />
                  ) : (
                    <Copy className="h-3.5 w-3.5" />
                  )}
                  {m.settings.copyToken}
                </Button>
              </div>
              <Input
                readOnly
                value={createdTokenModal.plaintext_token}
                onFocus={(e) => e.currentTarget.select()}
                className="mt-1 font-mono text-xs"
              />
            </div>

            <div className="space-y-2">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="flex items-center gap-1 rounded-lg border border-zinc-200 bg-zinc-50 p-0.5 dark:border-zinc-800 dark:bg-zinc-900">
                  {(
                    [
                      { id: "claude", label: m.settings.clientTabClaude },
                      { id: "cursor", label: m.settings.clientTabCursor },
                      { id: "generic", label: m.settings.clientTabGeneric },
                    ] as const
                  ).map((tab) => (
                    <button
                      key={tab.id}
                      type="button"
                      onClick={() => setClientConfigTab(tab.id)}
                      className={`cursor-pointer rounded-md px-2.5 py-1 text-xs font-medium transition-colors ${
                        clientConfigTab === tab.id
                          ? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
                          : "text-zinc-600 hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-zinc-200"
                      }`}
                    >
                      {tab.label}
                    </button>
                  ))}
                </div>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() =>
                    void handleCopy(
                      "json",
                      buildClientConfigSnippet(
                        clientConfigTab,
                        createdTokenModal,
                      ),
                    )
                  }
                >
                  {copiedField === "json" ? (
                    <Check className="h-3.5 w-3.5 text-emerald-600" />
                  ) : (
                    <Copy className="h-3.5 w-3.5" />
                  )}
                  {m.settings.copyJson}
                </Button>
              </div>
              <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
                {clientConfigTab === "claude"
                  ? m.settings.clientTabClaudeHint
                  : clientConfigTab === "cursor"
                    ? m.settings.clientTabCursorHint
                    : m.settings.clientTabGenericHint}
              </p>
              <pre className="overflow-x-auto rounded-lg bg-zinc-900 p-3.5 font-mono text-xs text-zinc-100">
                {buildClientConfigSnippet(clientConfigTab, createdTokenModal)}
              </pre>
            </div>
          </div>
        </Modal>
      )}

      {/* Confirm Revoke */}
      <ConfirmDialog
        open={revokingToken !== null}
        onClose={() => setRevokingToken(null)}
        onConfirm={() => {
          if (revokingToken) revokeTokenMutation.mutate(revokingToken.id);
        }}
        title={m.settings.revokeModalTitle}
        description={m.settings.revokeModalDesc(
          revokingToken?.name ?? "",
          revokingToken?.token_prefix ?? "",
        )}
        confirmLabel={m.settings.revokeModalConfirm}
        loading={revokeTokenMutation.isPending}
      />
    </div>
  );
}
