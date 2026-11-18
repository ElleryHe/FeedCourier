import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Bot,
  Check,
  Copy,
  Clock,
  Globe,
  KeyRound,
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
  EmptyState,
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
  const timeZones = useMemo(() => listTimeZones(), []);
  const detectedZone = useMemo(() => browserTimeZone(), []);

  useEffect(() => {
    if (settingsQuery.data) {
      setTimezone(settingsQuery.data.timezone ?? "");
      setProxyUrl(settingsQuery.data.outbound_proxy_url ?? "");
      setSsrfAllowlist(settingsQuery.data.ssrf_allowlist);
    }
  }, [settingsQuery.data]);

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
  const [tokenFormError, setTokenFormError] = useState<string | null>(null);
  const [createdTokenModal, setCreatedTokenModal] =
    useState<CreateMcpTokenResponse | null>(null);
  const [revokingToken, setRevokingToken] = useState<McpTokenDto | null>(null);
  const [copiedField, setCopiedField] = useState<"token" | "json" | null>(null);

  const createTokenMutation = useMutation({
    mutationFn: () =>
      api.mcpTokens.create({
        name: tokenName.trim(),
        scope: tokenScope,
      }),
    onSuccess: (res) => {
      setTokenName("");
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

  const handleCopy = async (kind: "token" | "json", value: string) => {
    const ok = await copyToClipboard(value);
    if (ok) {
      setCopiedField(kind);
      toast.success(
        kind === "token"
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

  const buildFallbackClientJson = (plaintextToken: string): string =>
    JSON.stringify(
      {
        mcpServers: {
          feedcourier: {
            type: "streamable-http",
            url: mcpEndpointUrl,
            headers: {
              Authorization: `Bearer ${plaintextToken}`,
            },
          },
        },
      },
      null,
      2,
    );

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

      {/* Section 0b: Account & Security */}
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
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <Bot className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
              <CardTitle>{m.settings.mcpTitle}</CardTitle>
            </div>
            <Badge variant="mono">{mcpEndpointUrl}</Badge>
          </div>
          <CardDescription>{m.settings.mcpDesc}</CardDescription>
        </CardHeader>

        <CardContent className="space-y-5">
          {/* Issue Token Form */}
          <form
            onSubmit={handleIssueToken}
            className="space-y-3.5 rounded-xl border border-zinc-200 bg-zinc-50/50 p-4 dark:border-zinc-800 dark:bg-zinc-950/50"
          >
            <div className="grid gap-3 sm:grid-cols-3">
              <div className="sm:col-span-1">
                <Label htmlFor="mcp-token-name">{m.settings.clientLabel}</Label>
                <Input
                  id="mcp-token-name"
                  value={tokenName}
                  onChange={(e) => {
                    setTokenName(e.target.value);
                    setTokenFormError(null);
                  }}
                  placeholder={m.settings.clientPlaceholder}
                  className="mt-1.5"
                />
              </div>

              <div className="sm:col-span-2">
                <Label>{m.settings.scopeLabel}</Label>
                <div className="mt-1.5 grid gap-2 sm:grid-cols-2">
                  <button
                    type="button"
                    onClick={() => setTokenScope("readonly")}
                    className={`cursor-pointer rounded-lg border p-2.5 text-left transition-colors ${
                      tokenScope === "readonly"
                        ? "border-zinc-900 bg-zinc-900 text-white dark:border-zinc-100 dark:bg-zinc-100 dark:text-zinc-900"
                        : "border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-900"
                    }`}
                  >
                    <div className="flex items-center justify-between">
                      <span className="text-xs font-semibold">
                        {m.settings.scopeReadonlyTitle}
                      </span>
                      <ShieldCheck className="h-3.5 w-3.5" />
                    </div>
                    <p
                      className={`mt-1 font-mono text-[11px] ${
                        tokenScope === "readonly"
                          ? "text-zinc-300 dark:text-zinc-600"
                          : "text-zinc-500"
                      }`}
                    >
                      list_feeds_status, inspect_failed_deliveries, preview_feed_latest
                    </p>
                  </button>

                  <button
                    type="button"
                    onClick={() => setTokenScope("admin")}
                    className={`cursor-pointer rounded-lg border p-2.5 text-left transition-colors ${
                      tokenScope === "admin"
                        ? "border-zinc-900 bg-zinc-900 text-white dark:border-zinc-100 dark:bg-zinc-100 dark:text-zinc-900"
                        : "border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-900"
                    }`}
                  >
                    <div className="flex items-center justify-between">
                      <span className="text-xs font-semibold">
                        {m.settings.scopeAdminTitle}
                      </span>
                      <KeyRound className="h-3.5 w-3.5" />
                    </div>
                    <p
                      className={`mt-1 font-mono text-[11px] ${
                        tokenScope === "admin"
                          ? "text-zinc-300 dark:text-zinc-600"
                          : "text-zinc-500"
                      }`}
                    >
                      + manage_feed, configure_route, reset_and_poll_feed, retry_dead_tasks, manage_outbox_queue, test_push_feed_entry
                    </p>
                  </button>
                </div>
              </div>
            </div>

            <FieldError message={tokenFormError} />

            <div className="flex justify-end">
              <Button
                type="submit"
                size="sm"
                loading={createTokenMutation.isPending}
              >
                <Plus className="h-3.5 w-3.5" />
                {m.settings.issueToken}
              </Button>
            </div>
          </form>

          {/* Issued Tokens Table */}
          {tokensQuery.isLoading ? (
            <div className="py-6 text-center text-xs text-zinc-500">
              {m.settings.loadingTokens}
            </div>
          ) : (tokensQuery.data?.length ?? 0) === 0 ? (
            <EmptyState
              icon={KeyRound}
              title={m.settings.emptyTokensTitle}
              description={m.settings.emptyTokensDesc}
            />
          ) : (
            <div className="overflow-x-auto rounded-lg border border-zinc-200 dark:border-zinc-800">
              <table className="w-full text-left text-xs">
                <thead className="border-b border-zinc-200 bg-zinc-50 text-[11px] text-zinc-500 uppercase dark:border-zinc-800 dark:bg-zinc-950">
                  <tr>
                    <th className="px-3.5 py-2.5 font-medium">
                      {m.settings.colName}
                    </th>
                    <th className="px-3.5 py-2.5 font-medium">
                      {m.settings.colPrefix}
                    </th>
                    <th className="px-3.5 py-2.5 font-medium">
                      {m.settings.colScope}
                    </th>
                    <th className="px-3.5 py-2.5 font-medium">
                      {m.settings.colLastUsed}
                    </th>
                    <th className="px-3.5 py-2.5 font-medium">
                      {m.settings.colCreated}
                    </th>
                    <th className="px-3.5 py-2.5 text-right font-medium">
                      {m.settings.colRevoke}
                    </th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-zinc-100 dark:divide-zinc-800">
                  {(tokensQuery.data ?? []).map((tok) => (
                    <tr key={tok.id}>
                      <td className="px-3.5 py-2.5 font-medium">{tok.name}</td>
                      <td className="px-3.5 py-2.5 font-mono text-zinc-500">
                        {tok.token_prefix}••••••
                      </td>
                      <td className="px-3.5 py-2.5">
                        <Badge
                          variant={tok.scope === "admin" ? "warning" : "info"}
                        >
                          {tok.scope}
                        </Badge>
                      </td>
                      <td
                        className="px-3.5 py-2.5 text-zinc-500"
                        title={formatDateTime(tok.last_used_at)}
                      >
                        {tok.last_used_at
                          ? formatRelativeTime(tok.last_used_at)
                          : m.common.never}
                      </td>
                      <td
                        className="px-3.5 py-2.5 text-zinc-500"
                        title={formatDateTime(tok.created_at)}
                      >
                        {formatRelativeTime(tok.created_at)}
                      </td>
                      <td className="px-3.5 py-2.5 text-right">
                        <Button
                          variant="ghost"
                          size="icon"
                          onClick={() => setRevokingToken(tok)}
                          className="text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-950/40"
                          title={m.settings.revokeTitle}
                          aria-label={m.settings.revokeTitle}
                        >
                          <Trash2 className="h-4 w-4" />
                        </Button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </CardContent>
      </Card>

      {/* One-Time Plaintext Token & Client JSON Reveal Modal */}
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

            <div>
              <div className="flex items-center justify-between">
                <Label>{m.settings.clientJsonLabel}</Label>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() =>
                    void handleCopy(
                      "json",
                      createdTokenModal.client_config_json ||
                        buildFallbackClientJson(
                          createdTokenModal.plaintext_token,
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
              <pre className="mt-1 overflow-x-auto rounded-lg bg-zinc-900 p-3.5 font-mono text-xs text-zinc-100">
                {createdTokenModal.client_config_json ||
                  buildFallbackClientJson(createdTokenModal.plaintext_token)}
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
