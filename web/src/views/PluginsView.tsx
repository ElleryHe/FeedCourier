import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  ArrowLeft,
  CheckCircle2,
  Database,
  Eraser,
  ExternalLink,
  FlaskConical,
  Globe,
  Info,
  Lock,
  Pencil,
  Plus,
  Puzzle,
  RefreshCw,
  Search,
  Settings2,
  Store,
  Trash2,
} from "lucide-react";
import { type FormEvent, useMemo, useState } from "react";
import {
  api,
  getPluginDeleteConflict,
  getReferencingRoutesConflict,
} from "@/api/client";
import type {
  BridgeManifestPreviewResponse,
  ChannelReferenceDto,
  MarketPluginItemDto,
  PluginDto,
  PluginInstanceDto,
  RouteReferenceDto,
  UpdatePluginRequest,
  YourlsAuthMode,
  YourlsProviderConfigView,
} from "@/api/types";
import { PluginIcon } from "@/components/plugins/PluginIcon";
import {
  buildValidatedSchemaConfig,
  formatSchemaConfigSummary,
  initSchemaFieldValues,
  parseObjectSchema,
  type SchemaFieldValues,
  SchemaFormFields,
} from "@/components/plugins/SchemaFormFields";
import {
  Badge,
  Button,
  Card,
  ConfirmDialog,
  EmptyState,
  FieldError,
  FieldHint,
  Input,
  Label,
  Modal,
  Select,
  Switch,
  Textarea,
} from "@/components/ui/primitives";
import { useToast } from "@/components/ui/toast";
import { useI18n } from "@/i18n";
import type { Messages } from "@/i18n/en";
import { errorMessage, isValidHttpUrl } from "@/lib/utils";

type PluginsTab = "installed" | "marketplace";

function isSafeHttpUrl(raw?: string | null): boolean {
  if (!raw) return false;
  const trimmed = raw.trim();
  if (!trimmed) return false;
  try {
    const parsed = new URL(trimmed);
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

function parseYourlsConfig(
  raw: Record<string, unknown>,
): YourlsProviderConfigView {
  const authModeRaw = raw.auth_mode;
  const auth_mode: YourlsAuthMode =
    authModeRaw === "timed_signature" || authModeRaw === "password"
      ? authModeRaw
      : "signature";
  return {
    api_url: typeof raw.api_url === "string" ? raw.api_url : "",
    auth_mode,
    has_signature: Boolean(raw.has_signature),
    signature_hint:
      typeof raw.signature_hint === "string" ? raw.signature_hint : null,
    username: typeof raw.username === "string" ? raw.username : null,
    has_password: Boolean(raw.has_password),
    exclude_domains: Array.isArray(raw.exclude_domains)
      ? raw.exclude_domains.filter((x): x is string => typeof x === "string")
      : [],
    strict_mode: Boolean(raw.strict_mode),
    pass_title: raw.pass_title !== false,
    use_proxy: Boolean(raw.use_proxy),
  };
}

function authModeLabel(mode: YourlsAuthMode, m: Messages): string {
  return m.plugins.detail.authModes[mode];
}

export function PluginsView() {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();

  const [activeTab, setActiveTab] = useState<PluginsTab>("installed");
  const [selectedPluginId, setSelectedPluginId] = useState<string | null>(null);

  const [marketSearch, setMarketSearch] = useState("");
  const [marketCategory, setMarketCategory] = useState<string>("all");
  const [enablingMarketId, setEnablingMarketId] = useState<string | null>(null);
  const [bridgeModalOpen, setBridgeModalOpen] = useState(false);
  const [editingBridge, setEditingBridge] = useState<PluginDto | null>(null);
  const [refreshingManifestId, setRefreshingManifestId] = useState<
    string | null
  >(null);
  const [deletingBridge, setDeletingBridge] = useState<PluginDto | null>(null);
  const [bridgeDeleteConflict, setBridgeDeleteConflict] = useState<{
    plugin: PluginDto;
    channels: ChannelReferenceDto[];
    routes: RouteReferenceDto[];
  } | null>(null);

  const pluginsQuery = useQuery({
    queryKey: ["plugins"],
    queryFn: () => api.plugins.list(),
  });

  const marketQuery = useQuery({
    queryKey: ["plugins-market"],
    queryFn: () => api.plugins.market(false),
    enabled: activeTab === "marketplace",
  });

  const refreshMarketMutation = useMutation({
    mutationFn: () => api.plugins.market(true),
    onSuccess: (res) => {
      queryClient.setQueryData(["plugins-market"], res);
      if (res.source === "snapshot") {
        toast.warning(
          m.plugins.market.toastRefreshSnapshotTitle,
          m.plugins.market.toastRefreshSnapshotDesc,
        );
      } else {
        toast.success(m.plugins.market.toastRefreshed);
      }
    },
    onError: (err) => {
      toast.error(m.plugins.market.toastRefreshFailed, errorMessage(err));
    },
  });

  const togglePluginMutation = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) =>
      api.plugins.update(id, { enabled }),
    onSuccess: (updated) => {
      queryClient.setQueryData<PluginDto[]>(["plugins"], (prev) =>
        prev ? prev.map((p) => (p.id === updated.id ? updated : p)) : [updated],
      );
      toast.success(
        updated.enabled
          ? m.plugins.detail.toastPluginEnabled
          : m.plugins.detail.toastPluginDisabled,
      );
      void queryClient.invalidateQueries({ queryKey: ["plugins"] });
      void queryClient.invalidateQueries({ queryKey: ["plugins-market"] });
    },
    onError: (err) => {
      toast.error(m.plugins.detail.toastPluginToggleFailed, errorMessage(err));
    },
  });

  const enableFromMarketMutation = useMutation({
    mutationFn: async (item: MarketPluginItemDto) => {
      setEnablingMarketId(item.id);
      return api.plugins.update(item.id, { enabled: true });
    },
    onSuccess: (updated) => {
      queryClient.setQueryData<PluginDto[]>(["plugins"], (prev) =>
        prev
          ? prev.some((p) => p.id === updated.id)
            ? prev.map((p) => (p.id === updated.id ? updated : p))
            : [...prev, updated]
          : [updated],
      );
      toast.success(m.plugins.detail.toastPluginEnabled);
      void queryClient.invalidateQueries({ queryKey: ["plugins"] });
      void queryClient.invalidateQueries({ queryKey: ["plugins-market"] });
      setActiveTab("installed");
      setSelectedPluginId(updated.id);
    },
    onError: (err) => {
      toast.error(m.plugins.detail.toastPluginToggleFailed, errorMessage(err));
    },
    onSettled: () => setEnablingMarketId(null),
  });

  const refreshManifestMutation = useMutation({
    mutationFn: (id: string) => {
      setRefreshingManifestId(id);
      return api.plugins.refreshManifest(id);
    },
    onSuccess: (updated) => {
      queryClient.setQueryData<PluginDto[]>(["plugins"], (prev) =>
        prev ? prev.map((p) => (p.id === updated.id ? updated : p)) : [updated],
      );
      toast.success(m.plugins.bridge.toastManifestRefreshed);
      void queryClient.invalidateQueries({ queryKey: ["plugins"] });
    },
    onError: (err) => {
      toast.error(
        m.plugins.bridge.toastManifestRefreshFailed,
        errorMessage(err),
      );
    },
    onSettled: () => setRefreshingManifestId(null),
  });

  const deleteBridgeMutation = useMutation({
    mutationFn: ({ plugin, cascade }: { plugin: PluginDto; cascade: boolean }) =>
      api.plugins.deleteBridge(plugin.id, cascade),
    onSuccess: (_res, { plugin }) => {
      toast.success(m.plugins.bridge.toastDeleted);
      setDeletingBridge(null);
      setBridgeDeleteConflict(null);
      if (selectedPluginId === plugin.id) {
        setSelectedPluginId(null);
      }
      void queryClient.invalidateQueries({ queryKey: ["plugins"] });
      void queryClient.invalidateQueries({ queryKey: ["plugins-market"] });
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
      void queryClient.invalidateQueries({ queryKey: ["routes"] });
    },
    onError: (err, { plugin, cascade }) => {
      if (!cascade) {
        const conflict = getPluginDeleteConflict(err);
        if (conflict) {
          setDeletingBridge(null);
          setBridgeDeleteConflict({
            plugin,
            channels: conflict.channels,
            routes: conflict.routes,
          });
          return;
        }
      }
      toast.error(m.plugins.bridge.toastDeleteFailed, errorMessage(err));
    },
  });

  const plugins = pluginsQuery.data ?? [];
  const selectedPlugin = useMemo(
    () => plugins.find((p) => p.id === selectedPluginId) ?? null,
    [plugins, selectedPluginId],
  );

  const marketPlugins = marketQuery.data?.plugins ?? [];
  const marketCategories = useMemo(() => {
    const set = new Set<string>();
    for (const p of marketPlugins) {
      if (p.category.trim()) set.add(p.category.trim());
    }
    return Array.from(set).sort();
  }, [marketPlugins]);

  const filteredMarketPlugins = useMemo(() => {
    const q = marketSearch.trim().toLowerCase();
    return marketPlugins.filter((item) => {
      if (marketCategory !== "all" && item.category !== marketCategory) {
        return false;
      }
      if (!q) return true;
      return (
        item.name.toLowerCase().includes(q) ||
        item.id.toLowerCase().includes(q) ||
        item.description.toLowerCase().includes(q) ||
        item.category.toLowerCase().includes(q)
      );
    });
  }, [marketPlugins, marketCategory, marketSearch]);

  if (selectedPlugin) {
    const isBridgePlugin =
      selectedPlugin.source === "bridge" || selectedPlugin.runtime === "bridge";
    return (
      <>
        <PluginDetailView
          plugin={selectedPlugin}
          onBack={() => setSelectedPluginId(null)}
          onToggleEnabled={(enabled) =>
            togglePluginMutation.mutate({ id: selectedPlugin.id, enabled })
          }
          togglingPlugin={togglePluginMutation.isPending}
          onEditBridge={
            isBridgePlugin ? () => setEditingBridge(selectedPlugin) : undefined
          }
          onRefreshManifest={
            isBridgePlugin
              ? () => refreshManifestMutation.mutate(selectedPlugin.id)
              : undefined
          }
          refreshingManifest={refreshingManifestId === selectedPlugin.id}
          onDeleteBridge={
            isBridgePlugin ? () => setDeletingBridge(selectedPlugin) : undefined
          }
        />

        {editingBridge && (
          <EditBridgeConnectionModal
            key={editingBridge.id}
            open={editingBridge !== null}
            plugin={editingBridge}
            onClose={() => setEditingBridge(null)}
          />
        )}

        <ConfirmDialog
          open={deletingBridge !== null}
          onClose={() => setDeletingBridge(null)}
          onConfirm={() => {
            if (deletingBridge) {
              deleteBridgeMutation.mutate({
                plugin: deletingBridge,
                cascade: false,
              });
            }
          }}
          title={m.plugins.bridge.deleteConfirmTitle}
          description={m.plugins.bridge.deleteConfirmDesc(
            deletingBridge?.manifest.name ?? "",
          )}
          confirmLabel={m.plugins.bridge.deleteConfirmBtn}
          loading={deleteBridgeMutation.isPending}
        />

        <Modal
          open={bridgeDeleteConflict !== null}
          onClose={() => setBridgeDeleteConflict(null)}
          title={m.plugins.bridge.deleteCascadeTitle}
          size="sm"
          footer={
            <>
              <Button
                variant="outline"
                onClick={() => setBridgeDeleteConflict(null)}
                disabled={deleteBridgeMutation.isPending}
              >
                {m.common.cancel}
              </Button>
              <Button
                variant="destructive"
                loading={deleteBridgeMutation.isPending}
                onClick={() => {
                  if (bridgeDeleteConflict) {
                    deleteBridgeMutation.mutate({
                      plugin: bridgeDeleteConflict.plugin,
                      cascade: true,
                    });
                  }
                }}
              >
                {m.plugins.bridge.deleteCascadeBtn}
              </Button>
            </>
          }
        >
          {bridgeDeleteConflict && (
            <div className="space-y-3 text-xs">
              <p className="text-zinc-600 dark:text-zinc-300">
                {m.plugins.bridge.deleteCascadeDesc(
                  bridgeDeleteConflict.plugin.manifest.name,
                  bridgeDeleteConflict.channels.length,
                  bridgeDeleteConflict.routes.length,
                )}
              </p>
              {bridgeDeleteConflict.channels.length > 0 && (
                <div>
                  <div className="mb-1 text-[11px] font-semibold text-zinc-500 uppercase">
                    {m.plugins.bridge.conflictChannelsLabel}
                  </div>
                  <ul className="space-y-1 rounded-lg border border-zinc-200 bg-zinc-50/70 p-2.5 dark:border-zinc-800 dark:bg-zinc-950/60">
                    {bridgeDeleteConflict.channels.map((ch) => (
                      <li key={ch.id} className="font-medium">
                        #{ch.id} · {ch.name}
                      </li>
                    ))}
                  </ul>
                </div>
              )}
              {bridgeDeleteConflict.routes.length > 0 && (
                <div>
                  <div className="mb-1 text-[11px] font-semibold text-zinc-500 uppercase">
                    {m.plugins.detail.conflictRoutesLabel}
                  </div>
                  <ul className="space-y-1 rounded-lg border border-zinc-200 bg-zinc-50/70 p-2.5 dark:border-zinc-800 dark:bg-zinc-950/60">
                    {bridgeDeleteConflict.routes.map((rt) => (
                      <li key={rt.id} className="font-medium">
                        #{rt.id} · {rt.name}
                      </li>
                    ))}
                  </ul>
                </div>
              )}
            </div>
          )}
        </Modal>
      </>
    );
  }

  return (
    <div className="space-y-5">
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-lg font-bold tracking-tight">
            {m.plugins.title}
          </h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400">
            {m.plugins.subtitle}
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <Button size="sm" onClick={() => setBridgeModalOpen(true)}>
            <Plus className="h-3.5 w-3.5" />
            {m.plugins.bridge.addBtn}
          </Button>

          {/* Installed / Marketplace Tabs */}
          <div className="flex items-center gap-1 rounded-lg border border-zinc-200 bg-white p-1 dark:border-zinc-800 dark:bg-zinc-900">
          <button
            type="button"
            onClick={() => setActiveTab("installed")}
            className={`inline-flex cursor-pointer items-center gap-1.5 rounded-md px-3 py-1 text-xs font-medium transition-colors ${
              activeTab === "installed"
                ? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
                : "text-zinc-600 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:bg-zinc-800"
            }`}
          >
            <Puzzle className="h-3.5 w-3.5" />
            {m.plugins.tabs.installed}
          </button>
          <button
            type="button"
            onClick={() => setActiveTab("marketplace")}
            className={`inline-flex cursor-pointer items-center gap-1.5 rounded-md px-3 py-1 text-xs font-medium transition-colors ${
              activeTab === "marketplace"
                ? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
                : "text-zinc-600 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:bg-zinc-800"
            }`}
          >
            <Store className="h-3.5 w-3.5" />
            {m.plugins.tabs.marketplace}
          </button>
        </div>
        </div>
      </div>

      {/* Installed Tab */}
      {activeTab === "installed" && (
        <>
          {pluginsQuery.isLoading ? (
            <Card className="p-8 text-center text-xs text-zinc-500">
              {m.plugins.loading}
            </Card>
          ) : pluginsQuery.isError ? (
            <Card className="border-rose-200 bg-rose-50/50 p-4 dark:border-rose-900/60 dark:bg-rose-950/20">
              <div className="flex flex-wrap items-center justify-between gap-3">
                <div className="flex items-center gap-2.5 text-xs text-rose-700 dark:text-rose-300">
                  <AlertTriangle className="h-4 w-4 shrink-0" />
                  <span>
                    {m.plugins.loadError} {errorMessage(pluginsQuery.error)}
                  </span>
                </div>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => void pluginsQuery.refetch()}
                  loading={pluginsQuery.isFetching}
                >
                  <RefreshCw className="h-3.5 w-3.5" />
                  {m.common.retry}
                </Button>
              </div>
            </Card>
          ) : plugins.length === 0 ? (
            <EmptyState
              icon={Puzzle}
              title={m.plugins.emptyInstalledTitle}
              description={m.plugins.emptyInstalledDesc}
            />
          ) : (
            <div className="grid gap-4 md:grid-cols-2">
              {plugins.map((plugin) => (
                <Card
                  key={plugin.id}
                  onClick={() => setSelectedPluginId(plugin.id)}
                  className="group flex cursor-pointer flex-col justify-between p-4 transition-colors hover:border-zinc-300 dark:hover:border-zinc-700"
                >
                  <div className="space-y-3">
                    <div className="flex items-start justify-between gap-3">
                      <div className="flex min-w-0 items-start gap-3">
                        <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border border-zinc-200 bg-zinc-50 text-zinc-700 dark:border-zinc-800 dark:bg-zinc-950 dark:text-zinc-200">
                          <PluginIcon
                            icon={plugin.manifest.icon}
                            className="h-4 w-4"
                          />
                        </div>
                        <div className="min-w-0">
                          <div className="flex flex-wrap items-center gap-1.5">
                            <h3 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
                              {plugin.manifest.name}
                            </h3>
                            <Badge variant="mono">
                              v{plugin.installed_version}
                            </Badge>
                            {!plugin.compatible && (
                              <Badge variant="danger">
                                {plugin.compatibility_reason ||
                                  m.plugins.market.incompatibleBadge}
                              </Badge>
                            )}
                            {!plugin.enabled && (
                              <Badge variant="default">
                                {m.common.disabled}
                              </Badge>
                            )}
                          </div>
                          <div className="mt-1 flex flex-wrap items-center gap-1.5">
                            {plugin.runtime === "bridge" ? (
                              <Badge
                                variant={
                                  plugin.kind === "channel" ? "info" : "warning"
                                }
                              >
                                {plugin.kind === "channel"
                                  ? m.plugins.market.kindBridgeChannel
                                  : m.plugins.market.kindBridgeProcessor}
                              </Badge>
                            ) : (
                              <Badge variant="success">
                                {m.plugins.market.kindBuiltin}
                              </Badge>
                            )}
                            {plugin.manifest.category && (
                              <Badge variant="outline">
                                {plugin.manifest.category}
                              </Badge>
                            )}
                            {plugin.kind === "processor" && (
                              <Badge
                                variant={
                                  plugin.instance_count > 0 ? "info" : "default"
                                }
                              >
                                {m.plugins.providerCount(plugin.instance_count)}
                              </Badge>
                            )}
                          </div>
                          {plugin.bridge_url && (
                            <p className="mt-1 truncate font-mono text-[11px] text-zinc-400">
                              {plugin.bridge_url}
                            </p>
                          )}
                        </div>
                      </div>

                      <div
                        onClick={(e) => e.stopPropagation()}
                        onKeyDown={(e) => e.stopPropagation()}
                        className="shrink-0"
                      >
                        <Switch
                          checked={plugin.enabled}
                          disabled={togglePluginMutation.isPending}
                          onCheckedChange={(enabled) =>
                            togglePluginMutation.mutate({
                              id: plugin.id,
                              enabled,
                            })
                          }
                        />
                      </div>
                    </div>

                    <p className="text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">
                      {plugin.manifest.description}
                    </p>
                  </div>

                  <div className="mt-4 flex flex-wrap items-center justify-between gap-2 border-t border-zinc-100 pt-3 dark:border-zinc-800/80">
                    <div className="flex flex-wrap items-center gap-1">
                      {plugin.manifest.hooks.map((hook) => (
                        <Badge
                          key={hook}
                          variant="mono"
                          className="text-[10px]"
                        >
                          {hook}
                        </Badge>
                      ))}
                      {plugin.manifest.permissions.map((perm) => (
                        <Badge
                          key={perm}
                          variant="outline"
                          className="text-[10px]"
                        >
                          {perm}
                        </Badge>
                      ))}
                    </div>

                    <div
                      className="flex flex-wrap items-center gap-1.5"
                      onClick={(e) => e.stopPropagation()}
                      onKeyDown={(e) => e.stopPropagation()}
                    >
                      {(plugin.source === "bridge" ||
                        plugin.runtime === "bridge") && (
                        <>
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() => setEditingBridge(plugin)}
                          >
                            <Pencil className="h-3.5 w-3.5" />
                            {m.plugins.bridge.editConnectionBtn}
                          </Button>
                          <Button
                            variant="ghost"
                            size="icon"
                            loading={refreshingManifestId === plugin.id}
                            onClick={() =>
                              refreshManifestMutation.mutate(plugin.id)
                            }
                            title={m.plugins.bridge.refreshManifestTitle}
                            aria-label={m.plugins.bridge.refreshManifestTitle}
                          >
                            <RefreshCw className="h-3.5 w-3.5" />
                          </Button>
                          <Button
                            variant="ghost"
                            size="icon"
                            onClick={() => setDeletingBridge(plugin)}
                            title={m.plugins.bridge.deleteBtn}
                            aria-label={m.plugins.bridge.deleteBtn}
                            className="text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-950/40"
                          >
                            <Trash2 className="h-3.5 w-3.5" />
                          </Button>
                        </>
                      )}
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => setSelectedPluginId(plugin.id)}
                      >
                        <Settings2 className="h-3.5 w-3.5" />
                        {m.plugins.configureBtn}
                      </Button>
                    </div>
                  </div>
                </Card>
              ))}
            </div>
          )}
        </>
      )}

      {/* Marketplace Tab */}
      {activeTab === "marketplace" && (
        <div className="space-y-4">
          {/* Search + Category Pills + Source Badge + Refresh */}
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <div className="relative max-w-sm flex-1">
              <Search className="pointer-events-none absolute top-1/2 left-2.5 h-3.5 w-3.5 -translate-y-1/2 text-zinc-400" />
              <Input
                value={marketSearch}
                onChange={(e) => setMarketSearch(e.target.value)}
                placeholder={m.plugins.market.searchPlaceholder}
                className="h-8 pl-8 text-xs"
              />
            </div>

            <div className="flex flex-wrap items-center gap-2">
              {marketQuery.data && (
                <Badge
                  variant={
                    marketQuery.data.source === "remote" ? "success" : "warning"
                  }
                  title={m.plugins.market.sourceTooltip(
                    marketQuery.data.index_url,
                    marketQuery.data.generated_at,
                  )}
                >
                  <Database className="h-3 w-3" />
                  {marketQuery.data.source === "remote"
                    ? m.plugins.market.sourceRemote
                    : m.plugins.market.sourceSnapshot}
                </Badge>
              )}
              <Button
                variant="outline"
                size="sm"
                onClick={() => refreshMarketMutation.mutate()}
                loading={refreshMarketMutation.isPending}
              >
                <RefreshCw className="h-3.5 w-3.5" />
                {m.plugins.market.refreshBtn}
              </Button>
            </div>
          </div>

          {marketCategories.length > 0 && (
            <div className="flex flex-wrap items-center gap-1.5">
              <button
                type="button"
                onClick={() => setMarketCategory("all")}
                className={`cursor-pointer rounded-md border px-2.5 py-1 text-xs font-medium transition-colors ${
                  marketCategory === "all"
                    ? "border-zinc-900 bg-zinc-900 text-white dark:border-zinc-100 dark:bg-zinc-100 dark:text-zinc-900"
                    : "border-zinc-200 bg-white text-zinc-600 hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-400 dark:hover:bg-zinc-800"
                }`}
              >
                {m.plugins.market.allCategories}
              </button>
              {marketCategories.map((cat) => (
                <button
                  key={cat}
                  type="button"
                  onClick={() => setMarketCategory(cat)}
                  className={`cursor-pointer rounded-md border px-2.5 py-1 text-xs font-medium transition-colors ${
                    marketCategory === cat
                      ? "border-zinc-900 bg-zinc-900 text-white dark:border-zinc-100 dark:bg-zinc-100 dark:text-zinc-900"
                      : "border-zinc-200 bg-white text-zinc-600 hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-400 dark:hover:bg-zinc-800"
                  }`}
                >
                  {cat}
                </button>
              ))}
            </div>
          )}

          {marketQuery.isLoading ? (
            <Card className="p-8 text-center text-xs text-zinc-500">
              {m.plugins.market.loading}
            </Card>
          ) : marketQuery.isError ? (
            <Card className="border-rose-200 bg-rose-50/50 p-4 dark:border-rose-900/60 dark:bg-rose-950/20">
              <div className="flex flex-wrap items-center justify-between gap-3">
                <div className="flex items-center gap-2.5 text-xs text-rose-700 dark:text-rose-300">
                  <AlertTriangle className="h-4 w-4 shrink-0" />
                  <span>
                    {m.plugins.market.loadError}{" "}
                    {errorMessage(marketQuery.error)}
                  </span>
                </div>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => void marketQuery.refetch()}
                  loading={marketQuery.isFetching}
                >
                  <RefreshCw className="h-3.5 w-3.5" />
                  {m.common.retry}
                </Button>
              </div>
            </Card>
          ) : filteredMarketPlugins.length === 0 ? (
            <EmptyState
              icon={Store}
              title={m.plugins.market.emptyTitle}
              description={m.plugins.market.emptyDesc}
            />
          ) : (
            <div className="grid gap-4 md:grid-cols-2">
              {filteredMarketPlugins.map((item) => {
                const isBuiltin = item.kind === "builtin";
                const isBridge = item.runtime === "bridge";
                const isBridgeChannel = isBridge && item.kind === "channel";
                const isBridgeProcessor = isBridge && item.kind !== "channel";
                return (
                  <Card
                    key={item.id}
                    className="flex flex-col justify-between p-4"
                  >
                    <div className="space-y-3">
                      <div className="flex items-start justify-between gap-3">
                        <div className="flex min-w-0 items-start gap-3">
                          <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border border-zinc-200 bg-zinc-50 text-zinc-700 dark:border-zinc-800 dark:bg-zinc-950 dark:text-zinc-200">
                            <PluginIcon icon={item.icon} className="h-4 w-4" />
                          </div>
                          <div className="min-w-0">
                            <div className="flex flex-wrap items-center gap-1.5">
                              <h3 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
                                {item.name}
                              </h3>
                              <Badge variant="mono">v{item.version}</Badge>
                              {isBuiltin ? (
                                <Badge variant="success">
                                  {m.plugins.market.kindBuiltin}
                                </Badge>
                              ) : isBridgeChannel ? (
                                <Badge variant="info">
                                  {m.plugins.market.kindBridgeChannel}
                                </Badge>
                              ) : isBridgeProcessor ? (
                                <Badge variant="warning">
                                  {m.plugins.market.kindBridgeProcessor}
                                </Badge>
                              ) : (
                                <Badge variant="warning">
                                  {m.plugins.market.wasmUpcomingBadge}
                                </Badge>
                              )}
                              {!item.compatible && (
                                <Badge
                                  variant="danger"
                                  title={item.compatibility_reason ?? undefined}
                                >
                                  <AlertTriangle className="h-3 w-3" />
                                  {item.compatibility_reason ||
                                    m.plugins.market.incompatibleBadge}
                                </Badge>
                              )}
                            </div>
                            <div className="mt-1 flex flex-wrap items-center gap-1.5">
                              {item.category && (
                                <Badge variant="outline">{item.category}</Badge>
                              )}
                              {item.installed && (
                                <Badge variant="info">
                                  <CheckCircle2 className="h-3 w-3" />
                                  {item.enabled
                                    ? m.plugins.market.enabledBadge
                                    : m.plugins.market.installedBadge}
                                </Badge>
                              )}
                            </div>
                          </div>
                        </div>
                      </div>

                      <p className="text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">
                        {item.description}
                      </p>

                      {(item.hooks.length > 0 ||
                        item.permissions.length > 0) && (
                        <div className="flex flex-wrap items-center gap-1">
                          {item.hooks.map((hook) => (
                            <Badge
                              key={hook}
                              variant="mono"
                              className="text-[10px]"
                            >
                              {hook}
                            </Badge>
                          ))}
                          {item.permissions.map((perm) => (
                            <Badge
                              key={perm}
                              variant="outline"
                              className="text-[10px]"
                            >
                              {perm}
                            </Badge>
                          ))}
                        </div>
                      )}

                      {(item.image || item.compose_snippet) && (
                        <details className="rounded-lg border border-zinc-200/80 bg-zinc-50/70 px-3 py-2 text-[11px] dark:border-zinc-800 dark:bg-zinc-950/50">
                          <summary className="cursor-pointer font-medium text-zinc-600 dark:text-zinc-300">
                            {m.plugins.market.composeSnippetToggle}
                          </summary>
                          <div className="mt-2 space-y-1.5">
                            {item.image && (
                              <div className="font-mono text-[10px] text-zinc-500">
                                {item.image}
                              </div>
                            )}
                            {item.compose_snippet && (
                              <pre className="overflow-x-auto rounded bg-zinc-900 p-2 font-mono text-[10px] text-zinc-100">
                                {item.compose_snippet}
                              </pre>
                            )}
                          </div>
                        </details>
                      )}
                    </div>

                    <div className="mt-4 flex flex-wrap items-center justify-between gap-2 border-t border-zinc-100 pt-3 dark:border-zinc-800/80">
                      <div className="flex flex-wrap items-center gap-2">
                        {isSafeHttpUrl(item.homepage) && item.homepage && (
                          <a
                            href={item.homepage}
                            target="_blank"
                            rel="noopener noreferrer"
                            className="inline-flex items-center gap-1 text-xs font-medium text-zinc-600 hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-zinc-100"
                          >
                            <ExternalLink className="h-3 w-3" />
                            {m.plugins.market.homepageBtn}
                          </a>
                        )}
                        {isSafeHttpUrl(item.repository) && item.repository && (
                          <a
                            href={item.repository}
                            target="_blank"
                            rel="noopener noreferrer"
                            className="inline-flex items-center gap-1 text-xs font-medium text-zinc-600 hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-zinc-100"
                          >
                            <ExternalLink className="h-3 w-3" />
                            {m.plugins.market.repositoryBtn}
                          </a>
                        )}
                      </div>

                      {isBuiltin && (
                        <Button
                          size="sm"
                          variant={item.enabled ? "outline" : "primary"}
                          disabled={!item.compatible}
                          loading={enablingMarketId === item.id}
                          onClick={() => {
                            if (item.enabled) {
                              setActiveTab("installed");
                              setSelectedPluginId(item.id);
                            } else {
                              enableFromMarketMutation.mutate(item);
                            }
                          }}
                        >
                          <Settings2 className="h-3.5 w-3.5" />
                          {item.enabled
                            ? m.plugins.market.configureBtn
                            : m.plugins.market.enableAndConfigureBtn}
                        </Button>
                      )}
                      {isBridge && (
                        <Button
                          size="sm"
                          variant={item.installed ? "outline" : "primary"}
                          disabled={!item.compatible && !item.installed}
                          onClick={() => {
                            if (item.installed) {
                              setActiveTab("installed");
                              setSelectedPluginId(item.id);
                            } else {
                              setBridgeModalOpen(true);
                            }
                          }}
                        >
                          {item.installed ? (
                            <>
                              <Settings2 className="h-3.5 w-3.5" />
                              {m.plugins.market.configureBtn}
                            </>
                          ) : (
                            <>
                              <Plus className="h-3.5 w-3.5" />
                              {m.plugins.market.registerBridgeBtn}
                            </>
                          )}
                        </Button>
                      )}
                    </div>
                  </Card>
                );
              })}
            </div>
          )}
        </div>
      )}

      {bridgeModalOpen && (
        <RegisterBridgeModal
          open={bridgeModalOpen}
          onClose={() => setBridgeModalOpen(false)}
          onRegistered={(created) => {
            setBridgeModalOpen(false);
            setActiveTab("installed");
            setSelectedPluginId(created.id);
          }}
        />
      )}

      {editingBridge && (
        <EditBridgeConnectionModal
          key={editingBridge.id}
          open={editingBridge !== null}
          plugin={editingBridge}
          onClose={() => setEditingBridge(null)}
        />
      )}

      <ConfirmDialog
        open={deletingBridge !== null}
        onClose={() => setDeletingBridge(null)}
        onConfirm={() => {
          if (deletingBridge) {
            deleteBridgeMutation.mutate({
              plugin: deletingBridge,
              cascade: false,
            });
          }
        }}
        title={m.plugins.bridge.deleteConfirmTitle}
        description={m.plugins.bridge.deleteConfirmDesc(
          deletingBridge?.manifest.name ?? "",
        )}
        confirmLabel={m.plugins.bridge.deleteConfirmBtn}
        loading={deleteBridgeMutation.isPending}
      />

      <Modal
        open={bridgeDeleteConflict !== null}
        onClose={() => setBridgeDeleteConflict(null)}
        title={m.plugins.bridge.deleteCascadeTitle}
        size="sm"
        footer={
          <>
            <Button
              variant="outline"
              onClick={() => setBridgeDeleteConflict(null)}
              disabled={deleteBridgeMutation.isPending}
            >
              {m.common.cancel}
            </Button>
            <Button
              variant="destructive"
              loading={deleteBridgeMutation.isPending}
              onClick={() => {
                if (bridgeDeleteConflict) {
                  deleteBridgeMutation.mutate({
                    plugin: bridgeDeleteConflict.plugin,
                    cascade: true,
                  });
                }
              }}
            >
              {m.plugins.bridge.deleteCascadeBtn}
            </Button>
          </>
        }
      >
        {bridgeDeleteConflict && (
          <div className="space-y-3 text-xs">
            <p className="text-zinc-600 dark:text-zinc-300">
              {m.plugins.bridge.deleteCascadeDesc(
                bridgeDeleteConflict.plugin.manifest.name,
                bridgeDeleteConflict.channels.length,
                bridgeDeleteConflict.routes.length,
              )}
            </p>
            {bridgeDeleteConflict.channels.length > 0 && (
              <div>
                <div className="mb-1 text-[11px] font-semibold text-zinc-500 uppercase">
                  {m.plugins.bridge.conflictChannelsLabel}
                </div>
                <ul className="space-y-1 rounded-lg border border-zinc-200 bg-zinc-50/70 p-2.5 dark:border-zinc-800 dark:bg-zinc-950/60">
                  {bridgeDeleteConflict.channels.map((ch) => (
                    <li key={ch.id} className="font-medium">
                      #{ch.id} · {ch.name}
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {bridgeDeleteConflict.routes.length > 0 && (
              <div>
                <div className="mb-1 text-[11px] font-semibold text-zinc-500 uppercase">
                  {m.plugins.detail.conflictRoutesLabel}
                </div>
                <ul className="space-y-1 rounded-lg border border-zinc-200 bg-zinc-50/70 p-2.5 dark:border-zinc-800 dark:bg-zinc-950/60">
                  {bridgeDeleteConflict.routes.map((rt) => (
                    <li key={rt.id} className="font-medium">
                      #{rt.id} · {rt.name}
                    </li>
                  ))}
                </ul>
              </div>
            )}
          </div>
        )}
      </Modal>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Register HTTP Bridge Plugin Modal
// ---------------------------------------------------------------------------

function RegisterBridgeModal({
  open,
  onClose,
  onRegistered,
}: {
  open: boolean;
  onClose: () => void;
  onRegistered: (plugin: PluginDto) => void;
}) {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();

  const [bridgeUrl, setBridgeUrl] = useState("");
  const [bridgeSecret, setBridgeSecret] = useState("");
  const [enabled, setEnabled] = useState(true);
  const [preview, setPreview] = useState<BridgeManifestPreviewResponse | null>(
    null,
  );
  const [formError, setFormError] = useState<string | null>(null);

  const previewMutation = useMutation({
    mutationFn: () =>
      api.plugins.previewBridge({
        bridge_url: bridgeUrl.trim(),
        bridge_secret: bridgeSecret.trim(),
        enabled,
      }),
    onSuccess: (res) => {
      setPreview(res);
      setFormError(null);
    },
    onError: (err) => {
      setPreview(null);
      setFormError(errorMessage(err));
    },
  });

  const registerMutation = useMutation({
    mutationFn: () =>
      api.plugins.registerBridge({
        bridge_url: bridgeUrl.trim(),
        bridge_secret: bridgeSecret.trim(),
        enabled,
      }),
    onSuccess: (created) => {
      toast.success(m.plugins.bridge.toastRegistered);
      void queryClient.invalidateQueries({ queryKey: ["plugins"] });
      void queryClient.invalidateQueries({ queryKey: ["plugins-market"] });
      onRegistered(created);
    },
    onError: (err) => {
      setFormError(errorMessage(err));
    },
  });

  const validateInputs = (): boolean => {
    if (!isValidHttpUrl(bridgeUrl.trim())) {
      setFormError(m.plugins.bridge.errUrlRequired);
      return false;
    }
    if (!bridgeSecret.trim()) {
      setFormError(m.plugins.bridge.errSecretRequired);
      return false;
    }
    setFormError(null);
    return true;
  };

  const handlePreview = (e: FormEvent) => {
    e.preventDefault();
    if (!validateInputs()) return;
    previewMutation.mutate();
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={m.plugins.bridge.modalTitle}
      description={m.plugins.bridge.modalDesc}
      size="lg"
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            {m.common.cancel}
          </Button>
          <Button
            variant="outline"
            onClick={handlePreview}
            loading={previewMutation.isPending}
          >
            <RefreshCw className="h-3.5 w-3.5" />
            {m.plugins.bridge.previewBtn}
          </Button>
          <Button
            disabled={!preview || !preview.compatible}
            loading={registerMutation.isPending}
            onClick={() => {
              if (!validateInputs()) return;
              registerMutation.mutate();
            }}
          >
            <Plus className="h-3.5 w-3.5" />
            {m.plugins.bridge.registerBtn}
          </Button>
        </>
      }
    >
      <form onSubmit={handlePreview} className="space-y-4">
        <div>
          <Label htmlFor="bridge-url">{m.plugins.bridge.urlLabel}</Label>
          <Input
            id="bridge-url"
            type="url"
            value={bridgeUrl}
            onChange={(e) => {
              setBridgeUrl(e.target.value);
              setPreview(null);
            }}
            placeholder={m.plugins.bridge.urlPlaceholder}
            className="mt-1 font-mono text-xs"
          />
          <FieldHint>{m.plugins.bridge.urlHint}</FieldHint>
        </div>

        <div>
          <Label htmlFor="bridge-secret">{m.plugins.bridge.secretLabel}</Label>
          <Input
            id="bridge-secret"
            type="password"
            value={bridgeSecret}
            onChange={(e) => {
              setBridgeSecret(e.target.value);
              setPreview(null);
            }}
            placeholder={m.plugins.bridge.secretPlaceholder}
            className="mt-1 font-mono text-xs"
          />
          <FieldHint>{m.plugins.bridge.secretHint}</FieldHint>
        </div>

        <div className="rounded-lg border border-zinc-200/80 bg-zinc-50/60 p-3 dark:border-zinc-800 dark:bg-zinc-950/60">
          <Switch
            id="bridge-enabled"
            checked={enabled}
            onCheckedChange={setEnabled}
            label={m.plugins.bridge.enabledLabel}
          />
        </div>

        {preview && (
          <div className="space-y-2.5 rounded-xl border border-zinc-200 bg-zinc-50/70 p-3.5 text-xs dark:border-zinc-800 dark:bg-zinc-950/60">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <span className="text-[11px] font-semibold tracking-wider text-zinc-500 uppercase">
                {m.plugins.bridge.previewHeading}
              </span>
              <Badge
                variant={
                  preview.manifest.kind === "channel" ? "info" : "warning"
                }
              >
                {preview.manifest.kind === "channel"
                  ? m.plugins.market.kindBridgeChannel
                  : m.plugins.market.kindBridgeProcessor}
              </Badge>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-semibold text-zinc-900 dark:text-zinc-100">
                {preview.manifest.name}
              </span>
              <Badge variant="mono">{preview.manifest.id}</Badge>
              <Badge variant="mono">v{preview.manifest.version}</Badge>
              {!preview.compatible && (
                <Badge variant="danger">
                  {preview.compatibility_reason ||
                    m.plugins.market.incompatibleBadge}
                </Badge>
              )}
            </div>
            {preview.manifest.description && (
              <p className="text-zinc-500 dark:text-zinc-400">
                {preview.manifest.description}
              </p>
            )}
            {preview.manifest.hooks.length > 0 && (
              <div className="flex flex-wrap items-center gap-1">
                {preview.manifest.hooks.map((h) => (
                  <Badge key={h} variant="mono" className="text-[10px]">
                    {h}
                  </Badge>
                ))}
              </div>
            )}
          </div>
        )}

        <FieldError message={formError} />
      </form>
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// Edit HTTP Bridge Connection Modal
// ---------------------------------------------------------------------------

function EditBridgeConnectionModal({
  open,
  onClose,
  plugin,
}: {
  open: boolean;
  onClose: () => void;
  plugin: PluginDto;
}) {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();

  const [bridgeUrl, setBridgeUrl] = useState(plugin.bridge_url ?? "");
  const [bridgeSecret, setBridgeSecret] = useState("");
  const [formError, setFormError] = useState<string | null>(null);

  const updateMutation = useMutation({
    mutationFn: () => {
      const payload: UpdatePluginRequest = {
        bridge_url: bridgeUrl.trim(),
        ...(bridgeSecret.trim() ? { bridge_secret: bridgeSecret.trim() } : {}),
      };
      return api.plugins.update(plugin.id, payload);
    },
    onSuccess: (updated) => {
      queryClient.setQueryData<PluginDto[]>(["plugins"], (prev) =>
        prev ? prev.map((p) => (p.id === updated.id ? updated : p)) : [updated],
      );
      toast.success(m.plugins.bridge.toastConnectionUpdated);
      void queryClient.invalidateQueries({ queryKey: ["plugins"] });
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
      onClose();
    },
    onError: (err) => {
      setFormError(errorMessage(err));
    },
  });

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    setFormError(null);
    if (!isValidHttpUrl(bridgeUrl.trim())) {
      setFormError(m.plugins.bridge.errUrlRequired);
      return;
    }
    updateMutation.mutate();
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={m.plugins.bridge.editModalTitle(plugin.manifest.name)}
      description={m.plugins.bridge.editModalDesc}
      size="lg"
      footer={
        <>
          <Button
            variant="outline"
            onClick={onClose}
            disabled={updateMutation.isPending}
          >
            {m.common.cancel}
          </Button>
          <Button onClick={handleSubmit} loading={updateMutation.isPending}>
            {m.plugins.bridge.editSaveBtn}
          </Button>
        </>
      }
    >
      <form onSubmit={handleSubmit} className="space-y-4">
        <div>
          <Label htmlFor="edit-bridge-url">{m.plugins.bridge.urlLabel}</Label>
          <Input
            id="edit-bridge-url"
            type="url"
            value={bridgeUrl}
            onChange={(e) => {
              setBridgeUrl(e.target.value);
              setFormError(null);
            }}
            placeholder={m.plugins.bridge.urlPlaceholder}
            className="mt-1 font-mono text-xs"
          />
          <FieldHint>{m.plugins.bridge.urlHint}</FieldHint>
        </div>

        <div>
          <Label htmlFor="edit-bridge-secret">
            {m.plugins.bridge.secretLabel}
          </Label>
          <Input
            id="edit-bridge-secret"
            type="password"
            value={bridgeSecret}
            onChange={(e) => {
              setBridgeSecret(e.target.value);
              setFormError(null);
            }}
            placeholder={m.plugins.bridge.editSecretPlaceholder}
            className="mt-1 font-mono text-xs"
          />
          <FieldHint>{m.plugins.bridge.editSecretHint}</FieldHint>
        </div>

        <div className="flex items-start gap-2 rounded-lg border border-zinc-200 bg-zinc-50/80 px-3 py-2.5 text-xs text-zinc-600 dark:border-zinc-800 dark:bg-zinc-950/60 dark:text-zinc-400">
          <Info className="mt-0.5 h-4 w-4 shrink-0 text-sky-600 dark:text-sky-400" />
          <span className="leading-relaxed">
            {m.plugins.bridge.editProbeHint}
          </span>
        </div>

        <FieldError message={formError} />
      </form>
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// Plugin Detail View (Providers Management)
// ---------------------------------------------------------------------------

function PluginDetailView({
  plugin,
  onBack,
  onToggleEnabled,
  togglingPlugin,
  onEditBridge,
  onRefreshManifest,
  refreshingManifest = false,
  onDeleteBridge,
}: {
  plugin: PluginDto;
  onBack: () => void;
  onToggleEnabled: (enabled: boolean) => void;
  togglingPlugin: boolean;
  onEditBridge?: () => void;
  onRefreshManifest?: () => void;
  refreshingManifest?: boolean;
  onDeleteBridge?: () => void;
}) {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();

  const [providerModalOpen, setProviderModalOpen] = useState(false);
  const [editingInstance, setEditingInstance] =
    useState<PluginInstanceDto | null>(null);
  const [testingInstanceId, setTestingInstanceId] = useState<number | null>(
    null,
  );
  const [clearingCacheInstance, setClearingCacheInstance] =
    useState<PluginInstanceDto | null>(null);
  const [deletingInstance, setDeletingInstance] =
    useState<PluginInstanceDto | null>(null);
  const [deleteConflict, setDeleteConflict] = useState<{
    instance: PluginInstanceDto;
    routes: RouteReferenceDto[];
  } | null>(null);

  const toggleInstanceMutation = useMutation({
    mutationFn: ({ iid, enabled }: { iid: number; enabled: boolean }) =>
      api.plugins.updateInstance(plugin.id, iid, { enabled }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["plugins"] });
    },
    onError: (err) => {
      toast.error(
        m.plugins.detail.toastProviderToggleFailed,
        errorMessage(err),
      );
    },
  });

  const testInstanceMutation = useMutation({
    mutationFn: (iid: number) => {
      setTestingInstanceId(iid);
      return api.plugins.testInstance(plugin.id, iid);
    },
    onSuccess: (res) => {
      if (res.ok) {
        toast.success(m.plugins.detail.toastTestOk, res.message);
      } else {
        toast.error(m.plugins.detail.toastTestFailed, res.message);
      }
    },
    onError: (err) => {
      toast.error(m.plugins.detail.toastTestFailed, errorMessage(err));
    },
    onSettled: () => setTestingInstanceId(null),
  });

  const clearCacheMutation = useMutation({
    mutationFn: (iid: number) => api.plugins.clearInstanceCache(plugin.id, iid),
    onSuccess: (res) => {
      toast.success(
        m.plugins.detail.toastCacheClearedTitle,
        m.plugins.detail.toastCacheClearedDesc(res.cleared_entries),
      );
      setClearingCacheInstance(null);
      void queryClient.invalidateQueries({ queryKey: ["plugins"] });
    },
    onError: (err) => {
      toast.error(m.plugins.detail.toastCacheClearFailed, errorMessage(err));
    },
  });

  const deleteInstanceMutation = useMutation({
    mutationFn: (inst: PluginInstanceDto) =>
      api.plugins.deleteInstance(plugin.id, inst.id),
    onSuccess: () => {
      toast.success(m.plugins.detail.toastProviderDeleted);
      setDeletingInstance(null);
      void queryClient.invalidateQueries({ queryKey: ["plugins"] });
      void queryClient.invalidateQueries({ queryKey: ["routes"] });
    },
    onError: (err, inst) => {
      const refs = getReferencingRoutesConflict(err);
      if (refs) {
        setDeletingInstance(null);
        setDeleteConflict({ instance: inst, routes: refs });
        return;
      }
      toast.error(
        m.plugins.detail.toastProviderDeleteFailed,
        errorMessage(err),
      );
    },
  });

  return (
    <div className="space-y-5">
      {/* Back bar */}
      <div>
        <Button variant="ghost" size="sm" onClick={onBack} className="-ml-2">
          <ArrowLeft className="h-3.5 w-3.5" />
          {m.plugins.backToPlugins}
        </Button>
      </div>

      {/* Plugin Header Card */}
      {!plugin.compatible && (
        <div className="flex items-start gap-2.5 rounded-xl border border-rose-200 bg-rose-50/80 px-3.5 py-2.5 text-xs text-rose-800 dark:border-rose-900/70 dark:bg-rose-950/40 dark:text-rose-200">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-rose-600 dark:text-rose-400" />
          <span className="leading-relaxed">
            {m.plugins.market.incompatibleNotice(
              plugin.compatibility_reason || m.plugins.market.incompatibleBadge,
            )}
          </span>
        </div>
      )}
      <Card className="p-4">
        <div className="flex flex-wrap items-start justify-between gap-4">
          <div className="flex min-w-0 items-start gap-3">
            <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg border border-zinc-200 bg-zinc-50 text-zinc-700 dark:border-zinc-800 dark:bg-zinc-950 dark:text-zinc-200">
              <PluginIcon icon={plugin.manifest.icon} className="h-5 w-5" />
            </div>
            <div className="min-w-0 space-y-1.5">
              <div className="flex flex-wrap items-center gap-2">
                <h1 className="text-base font-bold tracking-tight">
                  {plugin.manifest.name}
                </h1>
                <Badge variant="mono">v{plugin.installed_version}</Badge>
                <Badge variant="outline">{plugin.manifest.category}</Badge>
                {!plugin.compatible && (
                  <Badge variant="danger">
                    {plugin.compatibility_reason ||
                      m.plugins.market.incompatibleBadge}
                  </Badge>
                )}
                {!plugin.enabled && (
                  <Badge variant="default">{m.common.disabled}</Badge>
                )}
              </div>
              <p className="max-w-2xl text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">
                {plugin.manifest.description}
              </p>
              <div className="flex flex-wrap items-center gap-2 pt-0.5 text-[11px] text-zinc-500">
                <span className="inline-flex items-center gap-1">
                  {m.plugins.hooksLabel}:
                  {plugin.manifest.hooks.map((h) => (
                    <Badge key={h} variant="mono" className="text-[10px]">
                      {h}
                    </Badge>
                  ))}
                </span>
                <span className="inline-flex items-center gap-1">
                  {m.plugins.permissionsLabel}:
                  {plugin.manifest.permissions.map((perm) => (
                    <Badge key={perm} variant="outline" className="text-[10px]">
                      {perm}
                    </Badge>
                  ))}
                </span>
                {isSafeHttpUrl(plugin.manifest.homepage) &&
                  plugin.manifest.homepage && (
                    <a
                      href={plugin.manifest.homepage}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="inline-flex items-center gap-1 text-zinc-600 hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-zinc-100"
                    >
                      <ExternalLink className="h-3 w-3" />
                      {m.plugins.homepageLink}
                    </a>
                  )}
              </div>
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            {onEditBridge && (
              <Button variant="outline" size="sm" onClick={onEditBridge}>
                <Pencil className="h-3.5 w-3.5" />
                {m.plugins.bridge.editConnectionBtn}
              </Button>
            )}
            {onRefreshManifest && (
              <Button
                variant="outline"
                size="sm"
                loading={refreshingManifest}
                onClick={onRefreshManifest}
              >
                <RefreshCw className="h-3.5 w-3.5" />
                {m.plugins.bridge.refreshManifestTitle}
              </Button>
            )}
            {onDeleteBridge && (
              <Button
                variant="outline"
                size="sm"
                onClick={onDeleteBridge}
                className="text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-950/40"
              >
                <Trash2 className="h-3.5 w-3.5" />
                {m.plugins.bridge.deleteBtn}
              </Button>
            )}
            <Switch
              checked={plugin.enabled}
              disabled={togglingPlugin}
              onCheckedChange={onToggleEnabled}
            />
          </div>
        </div>

        {plugin.bridge_url && (
          <div className="mt-3 flex items-center gap-2 text-xs text-zinc-500">
            <Globe className="h-3.5 w-3.5 shrink-0" />
            <span className="font-mono">{plugin.bridge_url}</span>
          </div>
        )}

        {!plugin.enabled && (
          <div className="mt-3.5 flex items-start gap-2 rounded-lg border border-amber-200 bg-amber-50/80 px-3 py-2 text-xs text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-200">
            <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400" />
            <span>{m.plugins.globallyDisabledNotice}</span>
          </div>
        )}
      </Card>

      {/* Channel Plugin Summary vs Processor Provider Instances */}
      {plugin.kind === "channel" ? (
        <Card className="space-y-4 p-4">
          <div>
            <h2 className="text-sm font-semibold tracking-tight">
              {m.plugins.bridge.channelSummaryTitle}
            </h2>
            <p className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
              {m.plugins.bridge.channelSummaryDesc}
            </p>
          </div>
          <div className="grid gap-3 sm:grid-cols-2">
            {(() => {
              const acctProps = parseObjectSchema(
                plugin.manifest.account_schema,
              ).properties;
              const destProps = parseObjectSchema(
                plugin.manifest.destination_schema,
              ).properties;
              return (
                <>
                  <div className="rounded-lg border border-zinc-200/80 bg-zinc-50/50 p-3 dark:border-zinc-800 dark:bg-zinc-950/50">
                    <div className="mb-2 text-[11px] font-semibold tracking-wider text-zinc-500 uppercase">
                      {m.plugins.bridge.accountSchemaFields(acctProps.length)}
                    </div>
                    <div className="flex flex-wrap gap-1.5">
                      {acctProps.length === 0 ? (
                        <span className="text-xs text-zinc-400">—</span>
                      ) : (
                        acctProps.map(([key, prop]) => (
                          <Badge key={key} variant="mono" className="text-[10px]">
                            {prop.title || key}
                          </Badge>
                        ))
                      )}
                    </div>
                  </div>
                  <div className="rounded-lg border border-zinc-200/80 bg-zinc-50/50 p-3 dark:border-zinc-800 dark:bg-zinc-950/50">
                    <div className="mb-2 text-[11px] font-semibold tracking-wider text-zinc-500 uppercase">
                      {m.plugins.bridge.destSchemaFields(destProps.length)}
                    </div>
                    <div className="flex flex-wrap gap-1.5">
                      {destProps.length === 0 ? (
                        <span className="text-xs text-zinc-400">—</span>
                      ) : (
                        destProps.map(([key, prop]) => (
                          <Badge key={key} variant="mono" className="text-[10px]">
                            {prop.title || key}
                          </Badge>
                        ))
                      )}
                    </div>
                  </div>
                </>
              );
            })()}
          </div>
        </Card>
      ) : (
      <div className="space-y-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 className="text-sm font-semibold tracking-tight">
            {m.plugins.detail.providersHeading(plugin.instances.length)}
          </h2>
          <Button
            size="sm"
            onClick={() => {
              setEditingInstance(null);
              setProviderModalOpen(true);
            }}
          >
            <Plus className="h-3.5 w-3.5" />
            {m.plugins.detail.addProvider}
          </Button>
        </div>

        {plugin.instances.length === 0 ? (
          <EmptyState
            icon={Puzzle}
            title={m.plugins.detail.emptyProvidersTitle}
            description={
              plugin.id === "link-rewrite"
                ? m.plugins.detail.emptyProvidersDesc
                : m.plugins.detail.emptyProvidersGenericDesc
            }
            action={
              <Button
                size="sm"
                onClick={() => {
                  setEditingInstance(null);
                  setProviderModalOpen(true);
                }}
              >
                <Plus className="h-3.5 w-3.5" />
                {m.plugins.detail.addProvider}
              </Button>
            }
          />
        ) : plugin.id === "link-rewrite" ? (
          <Card className="overflow-hidden">
            <div className="overflow-x-auto">
              <table className="w-full text-left text-xs">
                <thead className="border-b border-zinc-200 bg-zinc-50/80 text-[11px] text-zinc-500 uppercase dark:border-zinc-800 dark:bg-zinc-950/60">
                  <tr>
                    <th className="px-3.5 py-2.5 font-medium">
                      {m.plugins.detail.colName}
                    </th>
                    <th className="px-3.5 py-2.5 font-medium">
                      {m.plugins.detail.colApiUrl}
                    </th>
                    <th className="px-3.5 py-2.5 font-medium">
                      {m.plugins.detail.colAuth}
                    </th>
                    <th className="px-3.5 py-2.5 font-medium">
                      {m.plugins.detail.colBehavior}
                    </th>
                    <th className="px-3.5 py-2.5 font-medium">
                      {m.plugins.detail.colCache}
                    </th>
                    <th className="px-3.5 py-2.5 text-right font-medium">
                      {m.plugins.detail.colActions}
                    </th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-zinc-100 dark:divide-zinc-800/80">
                  {plugin.instances.map((inst) => {
                    const cfg = parseYourlsConfig(inst.config);
                    return (
                      <tr
                        key={inst.id}
                        className="hover:bg-zinc-50/70 dark:hover:bg-zinc-950/40"
                      >
                        <td className="px-3.5 py-3">
                          <div className="flex flex-wrap items-center gap-1.5">
                            <span className="font-semibold text-zinc-900 dark:text-zinc-100">
                              {inst.name}
                            </span>
                            {!inst.enabled && (
                              <Badge variant="default">
                                {m.common.disabled}
                              </Badge>
                            )}
                          </div>
                        </td>
                        <td className="max-w-[240px] px-3.5 py-3">
                          <div
                            className="truncate font-mono text-[11px] text-zinc-700 dark:text-zinc-300"
                            title={cfg.api_url}
                          >
                            {cfg.api_url}
                          </div>
                        </td>
                        <td className="px-3.5 py-3">
                          <div className="flex flex-col gap-0.5">
                            <div>
                              <Badge variant="info">
                                {authModeLabel(cfg.auth_mode, m)}
                              </Badge>
                            </div>
                            <span className="inline-flex items-center gap-1 font-mono text-[10px] text-zinc-500">
                              <Lock className="h-2.5 w-2.5" />
                              {cfg.auth_mode === "password"
                                ? cfg.username || "—"
                                : cfg.signature_hint || "••••••••"}
                            </span>
                          </div>
                        </td>
                        <td className="px-3.5 py-3">
                          <div className="flex flex-wrap items-center gap-1">
                            <Badge
                              variant={cfg.strict_mode ? "warning" : "outline"}
                            >
                              {cfg.strict_mode
                                ? m.plugins.detail.strictBadge
                                : m.plugins.detail.gracefulBadge}
                            </Badge>
                            {cfg.pass_title && (
                              <Badge variant="outline">
                                {m.plugins.detail.passTitleBadge}
                              </Badge>
                            )}
                            {cfg.use_proxy && (
                              <Badge variant="outline">
                                <Globe className="h-3 w-3" />
                                {m.common.proxy}
                              </Badge>
                            )}
                            {cfg.exclude_domains.length > 0 && (
                              <Badge
                                variant="mono"
                                title={cfg.exclude_domains.join(", ")}
                              >
                                {m.plugins.detail.excludedDomainsBadge(
                                  cfg.exclude_domains.length,
                                )}
                              </Badge>
                            )}
                          </div>
                        </td>
                        <td className="px-3.5 py-3">
                          <Badge variant="mono">
                            {m.plugins.detail.cachedLinksCount(
                              inst.cache_entries,
                            )}
                          </Badge>
                        </td>
                        <td className="px-3.5 py-3 text-right">
                          <div className="inline-flex flex-wrap items-center justify-end gap-1.5">
                            <Switch
                              checked={inst.enabled}
                              onCheckedChange={(enabled) =>
                                toggleInstanceMutation.mutate({
                                  iid: inst.id,
                                  enabled,
                                })
                              }
                            />
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={() =>
                                testInstanceMutation.mutate(inst.id)
                              }
                              loading={testingInstanceId === inst.id}
                            >
                              <FlaskConical className="h-3 w-3" />
                              {m.plugins.detail.testBtn}
                            </Button>
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={() => setClearingCacheInstance(inst)}
                              disabled={inst.cache_entries === 0}
                            >
                              <Eraser className="h-3 w-3" />
                              {m.plugins.detail.clearCacheBtn}
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon"
                              onClick={() => {
                                setEditingInstance(inst);
                                setProviderModalOpen(true);
                              }}
                              title={m.plugins.detail.editTitle}
                              aria-label={m.plugins.detail.editTitle}
                            >
                              <Pencil className="h-3.5 w-3.5" />
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon"
                              onClick={() => setDeletingInstance(inst)}
                              className="text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-950/40"
                              title={m.plugins.detail.deleteTitle}
                              aria-label={m.plugins.detail.deleteTitle}
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                            </Button>
                          </div>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </Card>
        ) : (
          <Card className="overflow-hidden">
            <div className="overflow-x-auto">
              <table className="w-full text-left text-xs">
                <thead className="border-b border-zinc-200 bg-zinc-50/80 text-[11px] text-zinc-500 uppercase dark:border-zinc-800 dark:bg-zinc-950/60">
                  <tr>
                    <th className="px-3.5 py-2.5 font-medium">
                      {m.plugins.detail.colName}
                    </th>
                    <th className="px-3.5 py-2.5 font-medium">
                      {m.plugins.detail.colConfig}
                    </th>
                    <th className="px-3.5 py-2.5 font-medium">
                      {m.plugins.detail.colCache}
                    </th>
                    <th className="px-3.5 py-2.5 text-right font-medium">
                      {m.plugins.detail.colActions}
                    </th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-zinc-100 dark:divide-zinc-800/80">
                  {plugin.instances.map((inst) => {
                    const summaryEntries = formatSchemaConfigSummary(
                      inst.config,
                    );
                    return (
                      <tr
                        key={inst.id}
                        className="hover:bg-zinc-50/70 dark:hover:bg-zinc-950/40"
                      >
                        <td className="px-3.5 py-3">
                          <div className="flex flex-wrap items-center gap-1.5">
                            <span className="font-semibold text-zinc-900 dark:text-zinc-100">
                              {inst.name}
                            </span>
                            {!inst.enabled && (
                              <Badge variant="default">
                                {m.common.disabled}
                              </Badge>
                            )}
                          </div>
                        </td>
                        <td className="max-w-md px-3.5 py-3">
                          {summaryEntries.length === 0 ? (
                            <span className="text-zinc-400">
                              {m.plugins.detail.noConfigSummary}
                            </span>
                          ) : (
                            <div className="flex flex-wrap items-center gap-1">
                              {summaryEntries.slice(0, 5).map((entry) => (
                                <Badge
                                  key={entry.key}
                                  variant="outline"
                                  title={`${entry.key}: ${entry.value}`}
                                  className="max-w-[220px] truncate font-mono text-[10px]"
                                >
                                  {entry.isSecretHint && (
                                    <Lock className="h-2.5 w-2.5 shrink-0" />
                                  )}
                                  <span className="truncate">
                                    {entry.key}: {entry.value}
                                  </span>
                                </Badge>
                              ))}
                            </div>
                          )}
                        </td>
                        <td className="px-3.5 py-3">
                          <Badge variant="mono">
                            {m.plugins.detail.cachedLinksCount(
                              inst.cache_entries,
                            )}
                          </Badge>
                        </td>
                        <td className="px-3.5 py-3 text-right">
                          <div className="inline-flex flex-wrap items-center justify-end gap-1.5">
                            <Switch
                              checked={inst.enabled}
                              onCheckedChange={(enabled) =>
                                toggleInstanceMutation.mutate({
                                  iid: inst.id,
                                  enabled,
                                })
                              }
                            />
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={() =>
                                testInstanceMutation.mutate(inst.id)
                              }
                              loading={testingInstanceId === inst.id}
                            >
                              <FlaskConical className="h-3 w-3" />
                              {m.plugins.detail.testBtn}
                            </Button>
                            {inst.cache_entries > 0 && (
                              <Button
                                variant="outline"
                                size="sm"
                                onClick={() => setClearingCacheInstance(inst)}
                              >
                                <Eraser className="h-3 w-3" />
                                {m.plugins.detail.clearCacheBtn}
                              </Button>
                            )}
                            <Button
                              variant="ghost"
                              size="icon"
                              onClick={() => {
                                setEditingInstance(inst);
                                setProviderModalOpen(true);
                              }}
                              title={m.plugins.detail.editTitle}
                              aria-label={m.plugins.detail.editTitle}
                            >
                              <Pencil className="h-3.5 w-3.5" />
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon"
                              onClick={() => setDeletingInstance(inst)}
                              className="text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-950/40"
                              title={m.plugins.detail.deleteTitle}
                              aria-label={m.plugins.detail.deleteTitle}
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                            </Button>
                          </div>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </Card>
        )}
      </div>
      )}

      {/* Add / Edit Provider Modal */}
      {providerModalOpen &&
        (plugin.id === "link-rewrite" ? (
          <YourlsProviderModal
            open={providerModalOpen}
            onClose={() => setProviderModalOpen(false)}
            pluginId={plugin.id}
            instance={editingInstance}
          />
        ) : (
          <SchemaProviderModal
            open={providerModalOpen}
            onClose={() => setProviderModalOpen(false)}
            plugin={plugin}
            instance={editingInstance}
          />
        ))}

      {/* Clear Cache Confirmation */}
      <ConfirmDialog
        open={clearingCacheInstance !== null}
        onClose={() => setClearingCacheInstance(null)}
        onConfirm={() => {
          if (clearingCacheInstance) {
            clearCacheMutation.mutate(clearingCacheInstance.id);
          }
        }}
        title={m.plugins.detail.clearCacheConfirmTitle}
        description={m.plugins.detail.clearCacheConfirmDesc(
          clearingCacheInstance?.name ?? "",
          clearingCacheInstance?.cache_entries ?? 0,
        )}
        confirmLabel={m.plugins.detail.clearCacheConfirmBtn}
        destructive={false}
        loading={clearCacheMutation.isPending}
      />

      {/* Delete Provider Confirmation */}
      <ConfirmDialog
        open={deletingInstance !== null}
        onClose={() => setDeletingInstance(null)}
        onConfirm={() => {
          if (deletingInstance) {
            deleteInstanceMutation.mutate(deletingInstance);
          }
        }}
        title={m.plugins.detail.deleteConfirmTitle}
        description={m.plugins.detail.deleteConfirmDesc(
          deletingInstance?.name ?? "",
        )}
        confirmLabel={m.plugins.detail.deleteConfirmBtn}
        loading={deleteInstanceMutation.isPending}
      />

      {/* 409 Conflict Dialog listing referencing routes */}
      <Modal
        open={deleteConflict !== null}
        onClose={() => setDeleteConflict(null)}
        title={m.plugins.detail.conflictModalTitle}
        size="sm"
        footer={
          <Button variant="primary" onClick={() => setDeleteConflict(null)}>
            {m.common.close}
          </Button>
        }
      >
        {deleteConflict && (
          <div className="space-y-3 text-xs">
            <div className="flex items-start gap-2 rounded-lg border border-amber-200 bg-amber-50/80 p-3 text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-200">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400" />
              <p className="leading-relaxed">
                {m.plugins.detail.conflictModalDesc(
                  deleteConflict.instance.name,
                  deleteConflict.routes.length,
                )}
              </p>
            </div>
            <div>
              <div className="mb-1.5 font-semibold text-zinc-700 dark:text-zinc-300">
                {m.plugins.detail.conflictRoutesLabel}
              </div>
              <ul className="divide-y divide-zinc-100 rounded-lg border border-zinc-200 bg-zinc-50/50 dark:divide-zinc-800 dark:border-zinc-800 dark:bg-zinc-950/60">
                {deleteConflict.routes.map((r) => (
                  <li
                    key={r.id}
                    className="flex items-center justify-between px-3 py-2"
                  >
                    <span className="font-medium text-zinc-900 dark:text-zinc-100">
                      {r.name}
                    </span>
                    <Badge variant="mono">#{r.id}</Badge>
                  </li>
                ))}
              </ul>
            </div>
          </div>
        )}
      </Modal>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Add / Edit Provider Modal (`link-rewrite` YOURLS)
// ---------------------------------------------------------------------------

function YourlsProviderModal({
  open,
  onClose,
  pluginId,
  instance,
}: {
  open: boolean;
  onClose: () => void;
  pluginId: string;
  instance: PluginInstanceDto | null;
}) {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const isEdit = instance !== null;

  const initialCfg = useMemo(
    () => (instance ? parseYourlsConfig(instance.config) : null),
    [instance],
  );

  const [name, setName] = useState(instance?.name ?? "");
  const [apiUrl, setApiUrl] = useState(initialCfg?.api_url ?? "");
  const [authMode, setAuthMode] = useState<YourlsAuthMode>(
    initialCfg?.auth_mode ?? "signature",
  );
  const [signature, setSignature] = useState("");
  const [username, setUsername] = useState(initialCfg?.username ?? "");
  const [password, setPassword] = useState("");
  const [excludeDomainsText, setExcludeDomainsText] = useState(
    (initialCfg?.exclude_domains ?? []).join("\n"),
  );
  const [strictMode, setStrictMode] = useState(
    initialCfg?.strict_mode ?? false,
  );
  const [passTitle, setPassTitle] = useState(initialCfg?.pass_title ?? true);
  const [useProxy, setUseProxy] = useState(initialCfg?.use_proxy ?? false);
  const [enabled, setEnabled] = useState(instance?.enabled ?? true);
  const [formError, setFormError] = useState<string | null>(null);

  const saveMutation = useMutation({
    mutationFn: async () => {
      const excludeDomains = excludeDomainsText
        .split(/[\n,]+/)
        .map((s) => s.trim())
        .filter((s) => s.length > 0);

      const config: Record<string, unknown> = {
        api_url: apiUrl.trim(),
        auth_mode: authMode,
        exclude_domains: excludeDomains,
        strict_mode: strictMode,
        pass_title: passTitle,
        use_proxy: useProxy,
      };

      if (authMode === "signature" || authMode === "timed_signature") {
        if (signature.trim()) {
          config.signature = signature.trim();
        }
      } else {
        config.username = username.trim() || null;
        if (password) {
          config.password = password;
        }
      }

      if (isEdit && instance) {
        return api.plugins.updateInstance(pluginId, instance.id, {
          name: name.trim(),
          config,
          enabled,
        });
      }

      return api.plugins.createInstance(pluginId, {
        name: name.trim(),
        config,
        enabled,
      });
    },
    onSuccess: () => {
      toast.success(
        isEdit
          ? m.plugins.detail.toastProviderUpdated
          : m.plugins.detail.toastProviderCreated,
      );
      void queryClient.invalidateQueries({ queryKey: ["plugins"] });
      onClose();
    },
    onError: (err) => {
      setFormError(errorMessage(err));
    },
  });

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    setFormError(null);

    if (!name.trim()) {
      setFormError(m.plugins.providerModal.errEmptyName);
      return;
    }
    if (!isValidHttpUrl(apiUrl)) {
      setFormError(m.plugins.providerModal.errInvalidApiUrl);
      return;
    }
    if (authMode === "signature" || authMode === "timed_signature") {
      const hasExistingSig = Boolean(isEdit && initialCfg?.has_signature);
      if (!signature.trim() && !hasExistingSig) {
        setFormError(m.plugins.providerModal.errSignatureRequired);
        return;
      }
    } else {
      const hasExistingPw = Boolean(isEdit && initialCfg?.has_password);
      if (!username.trim() || (!password && !hasExistingPw)) {
        setFormError(m.plugins.providerModal.errPasswordAuthRequired);
        return;
      }
    }

    saveMutation.mutate();
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={
        isEdit
          ? m.plugins.providerModal.editTitle(instance?.name ?? "")
          : m.plugins.providerModal.addTitle
      }
      description={m.plugins.providerModal.desc}
      size="lg"
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            {m.common.cancel}
          </Button>
          <Button onClick={handleSubmit} loading={saveMutation.isPending}>
            {isEdit
              ? m.plugins.providerModal.saveBtn
              : m.plugins.providerModal.createBtn}
          </Button>
        </>
      }
    >
      <form onSubmit={handleSubmit} className="space-y-4">
        <div className="grid gap-3 sm:grid-cols-2">
          <div>
            <Label htmlFor="provider-name">
              {m.plugins.providerModal.nameLabel}
            </Label>
            <Input
              id="provider-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={m.plugins.providerModal.namePlaceholder}
              className="mt-1"
            />
          </div>
          <div>
            <Label htmlFor="provider-auth-mode">
              {m.plugins.providerModal.authModeLabel}
            </Label>
            <Select
              id="provider-auth-mode"
              value={authMode}
              onChange={(e) => {
                setAuthMode(e.target.value as YourlsAuthMode);
                setFormError(null);
              }}
              className="mt-1"
            >
              <option value="signature">
                {m.plugins.detail.authModes.signature}
              </option>
              <option value="timed_signature">
                {m.plugins.detail.authModes.timed_signature}
              </option>
              <option value="password">
                {m.plugins.detail.authModes.password}
              </option>
            </Select>
          </div>
        </div>

        <div>
          <Label htmlFor="provider-api-url">
            {m.plugins.providerModal.apiUrlLabel}
          </Label>
          <Input
            id="provider-api-url"
            value={apiUrl}
            onChange={(e) => setApiUrl(e.target.value)}
            placeholder={m.plugins.providerModal.apiUrlPlaceholder}
            className="mt-1 font-mono text-xs"
          />
          <FieldHint>{m.plugins.providerModal.authModeHints[authMode]}</FieldHint>
        </div>

        {/* Conditional Auth Fields */}
        {authMode === "signature" || authMode === "timed_signature" ? (
          <div>
            <Label htmlFor="provider-signature">
              {m.plugins.providerModal.signatureLabel}
            </Label>
            <Input
              id="provider-signature"
              type="password"
              value={signature}
              onChange={(e) => setSignature(e.target.value)}
              placeholder={
                isEdit && initialCfg?.has_signature
                  ? m.plugins.providerModal.signatureStoredPlaceholder(
                      initialCfg.signature_hint ?? "••••••••",
                    )
                  : m.plugins.providerModal.signaturePlaceholder
              }
              className="mt-1 font-mono text-xs"
            />
            {isEdit && initialCfg?.has_signature && (
              <FieldHint>{m.plugins.providerModal.secretPreserveHint}</FieldHint>
            )}
          </div>
        ) : (
          <div className="grid gap-3 sm:grid-cols-2">
            <div>
              <Label htmlFor="provider-username">
                {m.plugins.providerModal.usernameLabel}
              </Label>
              <Input
                id="provider-username"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                placeholder={m.plugins.providerModal.usernamePlaceholder}
                className="mt-1 font-mono text-xs"
              />
            </div>
            <div>
              <Label htmlFor="provider-password">
                {m.plugins.providerModal.passwordLabel}
              </Label>
              <Input
                id="provider-password"
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                placeholder={
                  isEdit && initialCfg?.has_password
                    ? m.plugins.providerModal.passwordStoredPlaceholder
                    : m.plugins.providerModal.passwordPlaceholder
                }
                className="mt-1 font-mono text-xs"
              />
              {isEdit && initialCfg?.has_password && (
                <FieldHint>
                  {m.plugins.providerModal.secretPreserveHint}
                </FieldHint>
              )}
            </div>
          </div>
        )}

        {/* Excluded Domains */}
        <div>
          <Label htmlFor="provider-exclude-domains">
            {m.plugins.providerModal.excludeDomainsLabel}
          </Label>
          <Textarea
            id="provider-exclude-domains"
            value={excludeDomainsText}
            onChange={(e) => setExcludeDomainsText(e.target.value)}
            placeholder={m.plugins.providerModal.excludeDomainsPlaceholder}
            className="mt-1 min-h-[72px] font-mono text-xs"
          />
          <FieldHint>{m.plugins.providerModal.excludeDomainsHint}</FieldHint>
        </div>

        {/* Toggles */}
        <div className="space-y-3 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
          <Switch
            id="provider-strict-mode"
            checked={strictMode}
            onCheckedChange={setStrictMode}
            label={m.plugins.providerModal.strictModeLabel}
            description={m.plugins.providerModal.strictModeDesc}
          />
          <Switch
            id="provider-pass-title"
            checked={passTitle}
            onCheckedChange={setPassTitle}
            label={m.plugins.providerModal.passTitleLabel}
            description={m.plugins.providerModal.passTitleDesc}
          />
          <Switch
            id="provider-use-proxy"
            checked={useProxy}
            onCheckedChange={setUseProxy}
            label={m.plugins.providerModal.useProxyLabel}
            description={m.plugins.providerModal.useProxyDesc}
          />
          <Switch
            id="provider-enabled"
            checked={enabled}
            onCheckedChange={setEnabled}
            label={m.plugins.providerModal.enabledLabel}
            description={m.plugins.providerModal.enabledDesc}
          />
        </div>

        {/* SSRF LAN Note */}
        <div className="flex items-start gap-2 rounded-lg border border-zinc-200 bg-zinc-50/80 px-3 py-2.5 text-xs text-zinc-600 dark:border-zinc-800 dark:bg-zinc-950/60 dark:text-zinc-400">
          <Info className="mt-0.5 h-4 w-4 shrink-0 text-sky-600 dark:text-sky-400" />
          <span className="leading-relaxed">
            {m.plugins.providerModal.ssrfLanNotice}
          </span>
        </div>

        <FieldError message={formError} />
      </form>
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// Generic JSON-Schema Driven Provider Modal (Third-Party / Future Plugins)
// ---------------------------------------------------------------------------

function SchemaProviderModal({
  open,
  onClose,
  plugin,
  instance,
}: {
  open: boolean;
  onClose: () => void;
  plugin: PluginDto;
  instance: PluginInstanceDto | null;
}) {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const isEdit = instance !== null;

  const [name, setName] = useState(instance?.name ?? "");
  const [enabled, setEnabled] = useState(instance?.enabled ?? true);
  const [fieldValues, setFieldValues] = useState<SchemaFieldValues>(() =>
    initSchemaFieldValues(
      plugin.manifest.instance_schema,
      isEdit ? instance?.config : undefined,
    ),
  );
  const [formError, setFormError] = useState<string | null>(null);

  const saveMutation = useMutation({
    mutationFn: async (config: Record<string, unknown>) => {
      if (isEdit && instance) {
        return api.plugins.updateInstance(plugin.id, instance.id, {
          name: name.trim(),
          config,
          enabled,
        });
      }
      return api.plugins.createInstance(plugin.id, {
        name: name.trim(),
        config,
        enabled,
      });
    },
    onSuccess: () => {
      toast.success(
        isEdit
          ? m.plugins.detail.toastProviderUpdated
          : m.plugins.detail.toastProviderCreated,
      );
      void queryClient.invalidateQueries({ queryKey: ["plugins"] });
      onClose();
    },
    onError: (err) => {
      setFormError(errorMessage(err));
    },
  });

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    setFormError(null);

    if (!name.trim()) {
      setFormError(m.plugins.providerModal.errEmptyName);
      return;
    }

    const { config, error } = buildValidatedSchemaConfig(
      plugin.manifest.instance_schema,
      fieldValues,
      isEdit,
      instance?.config,
      m,
    );
    if (error || !config) {
      setFormError(error);
      return;
    }

    saveMutation.mutate(config);
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={
        isEdit
          ? m.plugins.providerModal.editTitle(instance?.name ?? "")
          : m.plugins.providerModal.genericAddTitle(plugin.manifest.name)
      }
      description={m.plugins.providerModal.desc}
      size="lg"
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            {m.common.cancel}
          </Button>
          <Button onClick={handleSubmit} loading={saveMutation.isPending}>
            {isEdit
              ? m.plugins.providerModal.saveBtn
              : m.plugins.providerModal.createBtn}
          </Button>
        </>
      }
    >
      <form onSubmit={handleSubmit} className="space-y-4">
        <div>
          <Label htmlFor="schema-provider-name">
            {m.plugins.providerModal.nameLabel}
          </Label>
          <Input
            id="schema-provider-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={m.plugins.providerModal.namePlaceholder}
            className="mt-1"
          />
        </div>

        <SchemaFormFields
          idPrefix="schema-provider-field"
          schema={plugin.manifest.instance_schema}
          values={fieldValues}
          onChange={setFieldValues}
          isEdit={isEdit}
          existingConfig={instance?.config}
        />

        <div className="rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
          <Switch
            id="schema-provider-enabled"
            checked={enabled}
            onCheckedChange={setEnabled}
            label={m.plugins.providerModal.enabledLabel}
            description={m.plugins.providerModal.enabledDesc}
          />
        </div>

        <FieldError message={formError} />
      </form>
    </Modal>
  );
}
