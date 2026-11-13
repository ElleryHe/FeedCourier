import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Bot,
  Check,
  Copy,
  Globe,
  KeyRound,
  Plus,
  Save,
  ShieldCheck,
  Trash2,
} from "lucide-react";
import { type FormEvent, useEffect, useState } from "react";
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
import {
  copyToClipboard,
  errorMessage,
  formatDateTime,
  formatRelativeTime,
  isValidProxyUrl,
} from "@/lib/utils";

export function SettingsView() {
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

  useEffect(() => {
    if (settingsQuery.data) {
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
      toast.success("System network settings saved");
      void queryClient.invalidateQueries({ queryKey: ["settings"] });
    },
    onError: (err) => {
      setSettingsError(errorMessage(err));
    },
  });

  const handleSaveSettings = (e: FormEvent) => {
    e.preventDefault();
    setSettingsError(null);
    if (proxyUrl.trim() && !isValidProxyUrl(proxyUrl)) {
      setSettingsError(
        "Outbound proxy URL must use http://, https://, socks5://, or socks5h://.",
      );
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
      toast.success("MCP Bearer token issued");
      void queryClient.invalidateQueries({ queryKey: ["mcp-tokens"] });
    },
    onError: (err) => {
      setTokenFormError(errorMessage(err));
    },
  });

  const revokeTokenMutation = useMutation({
    mutationFn: (id: number) => api.mcpTokens.remove(id),
    onSuccess: () => {
      toast.success("MCP token revoked");
      setRevokingToken(null);
      void queryClient.invalidateQueries({ queryKey: ["mcp-tokens"] });
    },
    onError: (err) => {
      toast.error("Failed to revoke token", errorMessage(err));
    },
  });

  const handleIssueToken = (e: FormEvent) => {
    e.preventDefault();
    setTokenFormError(null);
    if (!tokenName.trim()) {
      setTokenFormError("Enter a descriptive token label (e.g. Claude Desktop).");
      return;
    }
    createTokenMutation.mutate();
  };

  const handleCopy = async (kind: "token" | "json", value: string) => {
    const ok = await copyToClipboard(value);
    if (ok) {
      setCopiedField(kind);
      toast.success(
        kind === "token" ? "Bearer token copied" : "MCP client JSON copied",
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
        <h1 className="text-lg font-bold tracking-tight">
          System Network & Native MCP Server
        </h1>
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          Configure outbound HTTP/SOCKS5 proxy routing, connect-time SSRF allowlists, and dual-tier Model Context Protocol tokens.
        </p>
      </div>

      {/* Section 1: Outbound Proxy & SSRF Guardrails */}
      <Card>
        <CardHeader>
          <div className="flex items-center gap-2">
            <Globe className="h-4 w-4 text-zinc-700 dark:text-zinc-300" />
            <CardTitle>Guarded HTTP Client · Proxy & SSRF Allowlist</CardTitle>
          </div>
          <CardDescription>
            FeedCourier blocks loopback, link-local, cloud metadata, and RFC1918 private IPs at DNS connect time across every hop. Add internal CIDRs or hostnames below only for trusted self-hosted services (e.g. RSSHub or Local Bot API Server).
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleSaveSettings} className="space-y-4">
            <div>
              <Label htmlFor="outbound-proxy">
                Global Outbound Proxy URL (HTTP / HTTPS / SOCKS5)
              </Label>
              <Input
                id="outbound-proxy"
                value={proxyUrl}
                onChange={(e) => {
                  setProxyUrl(e.target.value);
                  setSettingsError(null);
                }}
                placeholder="socks5h://127.0.0.1:1080 or http://proxy.lan:7890 (leave empty to disable)"
                className="mt-1.5 font-mono text-xs"
              />
              <FieldHint>
                Applied selectively to subscriptions and channels that enable <code className="font-mono">use_proxy</code>.
              </FieldHint>
            </div>

            <div>
              <Label>SSRF Private-Network Allowlist (CIDRs & Hostnames)</Label>
              <div className="mt-1.5">
                <TagInput
                  values={ssrfAllowlist}
                  onChange={setSsrfAllowlist}
                  mono
                  placeholder="e.g. 10.0.0.5/32, rsshub.internal, fd00::/8…"
                />
              </div>
              <FieldHint>
                Press Enter after each CIDR block or internal hostname. Never disables SSRF protection globally.
              </FieldHint>
            </div>

            <FieldError message={settingsError} />

            <div className="flex justify-end">
              <Button
                type="submit"
                size="sm"
                loading={saveSettingsMutation.isPending}
              >
                <Save className="h-3.5 w-3.5" />
                Save Network Settings
              </Button>
            </div>
          </form>
        </CardContent>
      </Card>

      {/* Section 2: Native MCP Server & Dual-Tier Tokens */}
      <Card>
        <CardHeader>
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <Bot className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
              <CardTitle>
                Native MCP Server (<code className="font-mono">/mcp</code>) · Dual-Tier Bearer Tokens
              </CardTitle>
            </div>
            <Badge variant="mono">{mcpEndpointUrl}</Badge>
          </div>
          <CardDescription>
            Streamable HTTP MCP endpoint powered by <code className="font-mono">rmcp</code>. Channel secrets and recipient destinations remain strictly black-boxed inside WebUI; external feed content is wrapped with untrusted-content markers against prompt injection.
          </CardDescription>
        </CardHeader>

        <CardContent className="space-y-5">
          {/* Issue Token Form */}
          <form
            onSubmit={handleIssueToken}
            className="space-y-3.5 rounded-xl border border-zinc-200 bg-zinc-50/50 p-4 dark:border-zinc-800 dark:bg-zinc-950/50"
          >
            <div className="grid gap-3 sm:grid-cols-3">
              <div className="sm:col-span-1">
                <Label htmlFor="mcp-token-name">Client / Agent Label</Label>
                <Input
                  id="mcp-token-name"
                  value={tokenName}
                  onChange={(e) => {
                    setTokenName(e.target.value);
                    setTokenFormError(null);
                  }}
                  placeholder="Claude Desktop / Cursor"
                  className="mt-1.5"
                />
              </div>

              <div className="sm:col-span-2">
                <Label>Permission Scope</Label>
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
                        Readonly Inspection
                      </span>
                      <ShieldCheck className="h-3.5 w-3.5" />
                    </div>
                    <p
                      className={`mt-1 text-[11px] ${
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
                        Admin Orchestration
                      </span>
                      <KeyRound className="h-3.5 w-3.5" />
                    </div>
                    <p
                      className={`mt-1 text-[11px] ${
                        tokenScope === "admin"
                          ? "text-zinc-300 dark:text-zinc-600"
                          : "text-zinc-500"
                      }`}
                    >
                      + manage_feed, configure_route, reset_and_poll_feed, retry_dead_tasks
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
                Issue MCP Token
              </Button>
            </div>
          </form>

          {/* Issued Tokens Table */}
          {tokensQuery.isLoading ? (
            <div className="py-6 text-center text-xs text-zinc-500">
              Loading MCP tokens…
            </div>
          ) : (tokensQuery.data?.length ?? 0) === 0 ? (
            <EmptyState
              icon={KeyRound}
              title="No MCP tokens issued yet"
              description="Issue a readonly or admin Bearer token above to connect Claude Desktop, Cursor, or any Streamable HTTP MCP client."
            />
          ) : (
            <div className="overflow-x-auto rounded-lg border border-zinc-200 dark:border-zinc-800">
              <table className="w-full text-left text-xs">
                <thead className="border-b border-zinc-200 bg-zinc-50 text-[11px] text-zinc-500 uppercase dark:border-zinc-800 dark:bg-zinc-950">
                  <tr>
                    <th className="px-3.5 py-2.5 font-medium">Name</th>
                    <th className="px-3.5 py-2.5 font-medium">Token Prefix</th>
                    <th className="px-3.5 py-2.5 font-medium">Scope</th>
                    <th className="px-3.5 py-2.5 font-medium">Last Used</th>
                    <th className="px-3.5 py-2.5 font-medium">Created</th>
                    <th className="px-3.5 py-2.5 text-right font-medium">
                      Revoke
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
                          variant={
                            tok.scope === "admin" ? "warning" : "info"
                          }
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
                          : "Never"}
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
                          title="Revoke token"
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
          title="Copy Your New MCP Bearer Token"
          description="Only the SHA-256 hash is persisted in SQLite. This plaintext secret will never be shown again."
          size="lg"
          footer={
            <Button onClick={() => setCreatedTokenModal(null)}>
              I Have Saved This Token
            </Button>
          }
        >
          <div className="space-y-4">
            <div>
              <div className="flex items-center justify-between">
                <Label>Plaintext Bearer Token ({createdTokenModal.token.scope})</Label>
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
                  Copy Token
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
                <Label>Ready-to-Copy MCP Client JSON Config</Label>
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
                  Copy JSON
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
        title="Revoke MCP Token"
        description={`Immediately revoke "${revokingToken?.name}" (${revokingToken?.token_prefix}…)? Connected MCP clients using this token will receive 401 Unauthorized.`}
        confirmLabel="Revoke Token"
        loading={revokeTokenMutation.isPending}
      />
    </div>
  );
}
