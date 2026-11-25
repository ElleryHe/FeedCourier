import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  ArrowLeft,
  ArrowUpCircle,
  CheckCircle2,
  Code2,
  Copy,
  Database,
  Download,
  Eraser,
  ExternalLink,
  FileCode2,
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
  Shield,
  Store,
  Trash2,
  Upload,
} from "lucide-react";
import {
  type ChangeEvent,
  type FormEvent,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  api,
  getPluginDeleteConflict,
  getReferencingRoutesConflict,
} from "@/api/client";
import type {
  ChannelReferenceDto,
  JsPluginPreviewResponse,
  MarketPluginItemDto,
  PluginDto,
  PluginInstanceDto,
  RouteReferenceDto,
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

function resolveLocalizedDescription(
  description: string,
  descriptionI18n: Record<string, string> | undefined,
  locale: string,
): string {
  if (!descriptionI18n) return description;
  const exact = descriptionI18n[locale];
  if (exact && exact.trim()) return exact;
  const langPrefix = locale.split("-")[0];
  if (langPrefix && descriptionI18n[langPrefix]?.trim()) {
    return descriptionI18n[langPrefix];
  }
  return description;
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
  const { m, locale } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();

  const [activeTab, setActiveTab] = useState<PluginsTab>("installed");
  const [selectedPluginId, setSelectedPluginId] = useState<string | null>(null);

  const [marketSearch, setMarketSearch] = useState("");
  const [marketCategory, setMarketCategory] = useState<string>("all");
  const [enablingMarketId, setEnablingMarketId] = useState<string | null>(null);

  const [uploadModalOpen, setUploadModalOpen] = useState(false);
  const [marketInstallTarget, setMarketInstallTarget] = useState<{
    item: MarketPluginItemDto;
    initialVersion?: string;
  } | null>(null);
  const [viewingSourcePlugin, setViewingSourcePlugin] =
    useState<PluginDto | null>(null);
  const [uninstallingPlugin, setUninstallingPlugin] =
    useState<PluginDto | null>(null);
  const [uninstallConflict, setUninstallConflict] = useState<{
    plugin: PluginDto;
    channels: ChannelReferenceDto[];
    routes: RouteReferenceDto[];
  } | null>(null);

  const pluginsQuery = useQuery({
    queryKey: ["plugins"],
    queryFn: () => api.plugins.list(),
  });

  const marketQuery = useQuery({
    queryKey: ["plugins", "market"],
    queryFn: () => api.plugins.market(false),
    staleTime: 60_000,
    retry: 1,
  });

  const refreshMarketMutation = useMutation({
    mutationFn: () => api.plugins.market(true),
    onSuccess: (res) => {
      queryClient.setQueryData(["plugins", "market"], res);
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
      void queryClient.invalidateQueries({ queryKey: ["plugins", "market"] });
      void queryClient.invalidateQueries({ queryKey: ["tasks"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
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
      void queryClient.invalidateQueries({ queryKey: ["plugins", "market"] });
      void queryClient.invalidateQueries({ queryKey: ["tasks"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
      setActiveTab("installed");
      setSelectedPluginId(updated.id);
    },
    onError: (err) => {
      toast.error(m.plugins.detail.toastPluginToggleFailed, errorMessage(err));
    },
    onSettled: () => setEnablingMarketId(null),
  });

  const uninstallPluginMutation = useMutation({
    mutationFn: ({ plugin, cascade }: { plugin: PluginDto; cascade: boolean }) =>
      api.plugins.remove(plugin.id, cascade),
    onSuccess: (_res, { plugin }) => {
      toast.success(m.plugins.js.toastDeleted);
      setUninstallingPlugin(null);
      setUninstallConflict(null);
      if (selectedPluginId === plugin.id) {
        setSelectedPluginId(null);
      }
      void queryClient.invalidateQueries({ queryKey: ["plugins"] });
      void queryClient.invalidateQueries({ queryKey: ["plugins", "market"] });
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
      void queryClient.invalidateQueries({ queryKey: ["routes"] });
      void queryClient.invalidateQueries({ queryKey: ["tasks"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
    },
    onError: (err, { plugin, cascade }) => {
      if (!cascade) {
        const conflict = getPluginDeleteConflict(err);
        if (conflict) {
          setUninstallingPlugin(null);
          setUninstallConflict({
            plugin,
            channels: conflict.channels,
            routes: conflict.routes,
          });
          return;
        }
      }
      toast.error(m.plugins.js.toastDeleteFailed, errorMessage(err));
    },
  });

  const plugins = pluginsQuery.data ?? [];
  const selectedPlugin = useMemo(
    () => plugins.find((p) => p.id === selectedPluginId) ?? null,
    [plugins, selectedPluginId],
  );

  const marketPlugins = marketQuery.data?.plugins ?? [];
  const marketById = useMemo(
    () => new Map(marketPlugins.map((item) => [item.id, item])),
    [marketPlugins],
  );

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
      const localizedDesc = resolveLocalizedDescription(
        item.description,
        item.description_i18n,
        locale,
      );
      return (
        item.name.toLowerCase().includes(q) ||
        item.id.toLowerCase().includes(q) ||
        item.description.toLowerCase().includes(q) ||
        localizedDesc.toLowerCase().includes(q) ||
        item.category.toLowerCase().includes(q) ||
        (item.allowed_hosts ?? []).some((h) => h.toLowerCase().includes(q))
      );
    });
  }, [marketPlugins, marketCategory, marketSearch, locale]);

  const sharedLifecycleModals = (
    <>
      {uploadModalOpen && (
        <UploadJsPluginModal
          open={uploadModalOpen}
          onClose={() => setUploadModalOpen(false)}
          onInstalled={(installed) => {
            setUploadModalOpen(false);
            setActiveTab("installed");
            setSelectedPluginId(installed.id);
          }}
        />
      )}

      {marketInstallTarget && (
        <InstallMarketPluginModal
          key={`${marketInstallTarget.item.id}:${marketInstallTarget.initialVersion ?? ""}`}
          open={marketInstallTarget !== null}
          item={marketInstallTarget.item}
          initialVersion={marketInstallTarget.initialVersion}
          onClose={() => setMarketInstallTarget(null)}
          onInstalled={(installed) => {
            setMarketInstallTarget(null);
            setActiveTab("installed");
            setSelectedPluginId(installed.id);
          }}
        />
      )}

      {viewingSourcePlugin && (
        <ViewPluginSourceModal
          key={viewingSourcePlugin.id}
          open={viewingSourcePlugin !== null}
          plugin={viewingSourcePlugin}
          onClose={() => setViewingSourcePlugin(null)}
        />
      )}

      <ConfirmDialog
        open={uninstallingPlugin !== null}
        onClose={() => setUninstallingPlugin(null)}
        onConfirm={() => {
          if (uninstallingPlugin) {
            uninstallPluginMutation.mutate({
              plugin: uninstallingPlugin,
              cascade: false,
            });
          }
        }}
        title={m.plugins.js.deleteConfirmTitle}
        description={m.plugins.js.deleteConfirmDesc(
          uninstallingPlugin?.manifest.name ?? "",
        )}
        confirmLabel={m.plugins.js.deleteConfirmBtn}
        loading={uninstallPluginMutation.isPending}
      />

      <Modal
        open={uninstallConflict !== null}
        onClose={() => setUninstallConflict(null)}
        title={m.plugins.js.deleteCascadeTitle}
        size="sm"
        footer={
          <>
            <Button
              variant="outline"
              onClick={() => setUninstallConflict(null)}
              disabled={uninstallPluginMutation.isPending}
            >
              {m.common.cancel}
            </Button>
            <Button
              variant="destructive"
              loading={uninstallPluginMutation.isPending}
              onClick={() => {
                if (uninstallConflict) {
                  uninstallPluginMutation.mutate({
                    plugin: uninstallConflict.plugin,
                    cascade: true,
                  });
                }
              }}
            >
              {m.plugins.js.deleteCascadeBtn}
            </Button>
          </>
        }
      >
        {uninstallConflict && (
          <div className="space-y-3 text-xs">
            <p className="text-zinc-600 dark:text-zinc-300">
              {m.plugins.js.deleteCascadeDesc(
                uninstallConflict.plugin.manifest.name,
                uninstallConflict.channels.length,
                uninstallConflict.routes.length,
              )}
            </p>
            {uninstallConflict.channels.length > 0 && (
              <div>
                <div className="mb-1 text-[11px] font-semibold text-zinc-500 uppercase">
                  {m.plugins.js.conflictChannelsLabel}
                </div>
                <ul className="space-y-1 rounded-lg border border-zinc-200 bg-zinc-50/70 p-2.5 dark:border-zinc-800 dark:bg-zinc-950/60">
                  {uninstallConflict.channels.map((ch) => (
                    <li key={ch.id} className="font-medium">
                      #{ch.id} · {ch.name}
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {uninstallConflict.routes.length > 0 && (
              <div>
                <div className="mb-1 text-[11px] font-semibold text-zinc-500 uppercase">
                  {m.plugins.detail.conflictRoutesLabel}
                </div>
                <ul className="space-y-1 rounded-lg border border-zinc-200 bg-zinc-50/70 p-2.5 dark:border-zinc-800 dark:bg-zinc-950/60">
                  {uninstallConflict.routes.map((rt) => (
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

  if (selectedPlugin) {
    const isJsPlugin = selectedPlugin.runtime === "js";
    const matchingMarket = marketById.get(selectedPlugin.id);
    const updateVersion =
      matchingMarket?.update_available
        ? matchingMarket.latest || matchingMarket.version
        : null;

    return (
      <>
        <PluginDetailView
          plugin={selectedPlugin}
          updateVersion={updateVersion}
          onBack={() => setSelectedPluginId(null)}
          onToggleEnabled={(enabled) =>
            togglePluginMutation.mutate({ id: selectedPlugin.id, enabled })
          }
          togglingPlugin={togglePluginMutation.isPending}
          onUpdateFromMarket={
            matchingMarket && updateVersion
              ? () =>
                  setMarketInstallTarget({
                    item: matchingMarket,
                    initialVersion: updateVersion,
                  })
              : undefined
          }
          onViewSource={
            isJsPlugin
              ? () => setViewingSourcePlugin(selectedPlugin)
              : undefined
          }
          onUninstall={
            isJsPlugin
              ? () => setUninstallingPlugin(selectedPlugin)
              : undefined
          }
        />
        {sharedLifecycleModals}
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
          <Button size="sm" onClick={() => setUploadModalOpen(true)}>
            <Upload className="h-3.5 w-3.5" />
            {m.plugins.js.uploadBtn}
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
              {plugins.map((plugin) => {
                const isJs = plugin.runtime === "js";
                const matchingMarket = marketById.get(plugin.id);
                const updateVersion = matchingMarket?.update_available
                  ? matchingMarket.latest || matchingMarket.version
                  : null;
                const localizedDesc = resolveLocalizedDescription(
                  plugin.manifest.description,
                  plugin.manifest.description_i18n,
                  locale,
                );
                const allowedHosts =
                  plugin.allowed_hosts ?? plugin.manifest.allowed_hosts ?? [];

                return (
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
                              {plugin.disabled_reason ? (
                                <Badge variant="danger">
                                  {m.plugins.autoDisabledLabel}
                                </Badge>
                              ) : (
                                !plugin.enabled && (
                                  <Badge variant="default">
                                    {m.common.disabled}
                                  </Badge>
                                )
                              )}
                            </div>
                            <div className="mt-1 flex flex-wrap items-center gap-1.5">
                              {isJs ? (
                                <Badge
                                  variant={
                                    plugin.kind === "channel"
                                      ? "info"
                                      : "warning"
                                  }
                                >
                                  {plugin.kind === "channel"
                                    ? m.plugins.market.kindJsChannel
                                    : m.plugins.market.kindJsProcessor}
                                </Badge>
                              ) : (
                                <Badge variant="success">
                                  {m.plugins.market.kindBuiltin}
                                </Badge>
                              )}
                              {plugin.source === "custom" && (
                                <Badge variant="outline">
                                  {m.plugins.market.customSourceBadge}
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
                                    plugin.instance_count > 0
                                      ? "info"
                                      : "default"
                                  }
                                >
                                  {m.plugins.providerCount(
                                    plugin.instance_count,
                                  )}
                                </Badge>
                              )}
                            </div>
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
                        {localizedDesc}
                      </p>

                      {plugin.disabled_reason && (
                        <div className="flex items-start gap-2 rounded-lg border border-rose-200 bg-rose-50/80 px-3 py-2 text-xs text-rose-800 dark:border-rose-900/70 dark:bg-rose-950/40 dark:text-rose-200">
                          <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-rose-600 dark:text-rose-400" />
                          <div className="min-w-0 leading-relaxed">
                            <span className="font-semibold">
                              {m.plugins.autoDisabledLabel}:
                            </span>{" "}
                            <span className="break-words">
                              {plugin.disabled_reason}
                            </span>
                          </div>
                        </div>
                      )}

                      {allowedHosts.length > 0 && (
                        <div className="flex flex-wrap items-center gap-1">
                          <Globe className="h-3 w-3 shrink-0 text-zinc-400" />
                          {allowedHosts.map((host) => (
                            <Badge
                              key={host}
                              variant="mono"
                              className="text-[10px]"
                            >
                              {host}
                            </Badge>
                          ))}
                        </div>
                      )}
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
                        {matchingMarket && updateVersion && (
                          <Button
                            variant="primary"
                            size="sm"
                            onClick={() =>
                              setMarketInstallTarget({
                                item: matchingMarket,
                                initialVersion: updateVersion,
                              })
                            }
                          >
                            <ArrowUpCircle className="h-3.5 w-3.5" />
                            {m.plugins.market.updateToBtn(updateVersion)}
                          </Button>
                        )}
                        {isJs && (
                          <>
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={() => setViewingSourcePlugin(plugin)}
                            >
                              <Code2 className="h-3.5 w-3.5" />
                              {m.plugins.js.viewSourceBtn}
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon"
                              onClick={() => setUninstallingPlugin(plugin)}
                              title={m.plugins.js.uninstallBtn}
                              aria-label={m.plugins.js.uninstallBtn}
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
                );
              })}
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
                const isJs = item.runtime === "js";
                const isJsChannel = isJs && item.kind === "channel";
                const isJsProcessor = isJs && item.kind !== "channel";
                const displayVersion = item.latest || item.version;
                const localizedDesc = resolveLocalizedDescription(
                  item.description,
                  item.description_i18n,
                  locale,
                );
                const allowedHosts = item.allowed_hosts ?? [];
                const versions = item.versions ?? [];

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
                              <Badge variant="mono">v{displayVersion}</Badge>
                              {isBuiltin ? (
                                <Badge variant="success">
                                  {m.plugins.market.kindBuiltin}
                                </Badge>
                              ) : isJsChannel ? (
                                <Badge variant="info">
                                  {m.plugins.market.kindJsChannel}
                                </Badge>
                              ) : isJsProcessor ? (
                                <Badge variant="warning">
                                  {m.plugins.market.kindJsProcessor}
                                </Badge>
                              ) : (
                                <Badge variant="outline">
                                  {item.kind === "coming_soon"
                                    ? m.plugins.market.kindComingSoon
                                    : m.plugins.market.kindExternal}
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
                                  {item.installed_version
                                    ? ` · v${item.installed_version}`
                                    : ""}
                                </Badge>
                              )}
                            </div>
                          </div>
                        </div>
                      </div>

                      <p className="text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">
                        {localizedDesc}
                      </p>

                      {allowedHosts.length > 0 && (
                        <div className="flex flex-wrap items-center gap-1">
                          <span className="inline-flex items-center gap-1 text-[11px] text-zinc-500 dark:text-zinc-400">
                            <Shield className="h-3 w-3 shrink-0" />
                            {m.plugins.market.allowedHostsLabel}:
                          </span>
                          {allowedHosts.map((host) => (
                            <Badge
                              key={host}
                              variant="mono"
                              className="text-[10px]"
                            >
                              {host}
                            </Badge>
                          ))}
                        </div>
                      )}

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

                      <div className="flex flex-wrap items-center gap-1.5">
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
                        {isJs && !item.installed && (
                          <Button
                            size="sm"
                            variant="primary"
                            disabled={!item.compatible}
                            onClick={() =>
                              setMarketInstallTarget({
                                item,
                                initialVersion: displayVersion,
                              })
                            }
                          >
                            <Download className="h-3.5 w-3.5" />
                            {m.plugins.market.installBtn}
                          </Button>
                        )}
                        {isJs && item.installed && item.update_available && (
                          <>
                            <Button
                              size="sm"
                              variant="outline"
                              onClick={() => {
                                setActiveTab("installed");
                                setSelectedPluginId(item.id);
                              }}
                            >
                              <Settings2 className="h-3.5 w-3.5" />
                              {m.plugins.market.configureBtn}
                            </Button>
                            <Button
                              size="sm"
                              variant="primary"
                              disabled={!item.compatible}
                              onClick={() =>
                                setMarketInstallTarget({
                                  item,
                                  initialVersion: displayVersion,
                                })
                              }
                            >
                              <ArrowUpCircle className="h-3.5 w-3.5" />
                              {m.plugins.market.updateToBtn(displayVersion)}
                            </Button>
                          </>
                        )}
                        {isJs && item.installed && !item.update_available && (
                          <>
                            {versions.length > 1 && (
                              <Button
                                size="sm"
                                variant="ghost"
                                onClick={() =>
                                  setMarketInstallTarget({
                                    item,
                                    initialVersion:
                                      item.installed_version || displayVersion,
                                  })
                                }
                              >
                                {m.plugins.market.switchVersionBtn}
                              </Button>
                            )}
                            <Button
                              size="sm"
                              variant="outline"
                              onClick={() => {
                                setActiveTab("installed");
                                setSelectedPluginId(item.id);
                              }}
                            >
                              <Settings2 className="h-3.5 w-3.5" />
                              {m.plugins.market.configureBtn}
                            </Button>
                          </>
                        )}
                      </div>
                    </div>
                  </Card>
                );
              })}
            </div>
          )}
        </div>
      )}

      {sharedLifecycleModals}
    </div>
  );
}

// ---------------------------------------------------------------------------
// One-Click Marketplace JS Plugin Install / Update Modal
// ---------------------------------------------------------------------------

function InstallMarketPluginModal({
  open,
  item,
  initialVersion,
  onClose,
  onInstalled,
}: {
  open: boolean;
  item: MarketPluginItemDto;
  initialVersion?: string;
  onClose: () => void;
  onInstalled: (plugin: PluginDto) => void;
}) {
  const { m, locale } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();

  const versions = item.versions ?? [];
  const defaultVersion =
    initialVersion || item.latest || versions[0]?.version || item.version;

  const [selectedVersion, setSelectedVersion] =
    useState<string>(defaultVersion);
  const [enabled, setEnabled] = useState(true);
  const [replaceCustom, setReplaceCustom] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  const previewQuery = useQuery({
    queryKey: ["plugins", "market-preview", item.id, selectedVersion],
    queryFn: () =>
      api.plugins.previewMarketInstall({
        plugin_id: item.id,
        version: selectedVersion || undefined,
        enabled,
      }),
    enabled: open,
    staleTime: 0,
    gcTime: 0,
    retry: false,
  });

  const installMutation = useMutation({
    mutationFn: () =>
      api.plugins.installFromMarket({
        plugin_id: item.id,
        version: selectedVersion || undefined,
        enabled,
        replace_custom: replaceCustom,
      }),
    onSuccess: (installed) => {
      toast.success(
        item.installed ? m.plugins.js.toastUpdated : m.plugins.js.toastInstalled,
      );
      void queryClient.invalidateQueries({ queryKey: ["plugins"] });
      void queryClient.invalidateQueries({ queryKey: ["plugins", "market"] });
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
      void queryClient.invalidateQueries({ queryKey: ["routes"] });
      void queryClient.invalidateQueries({ queryKey: ["tasks"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
      onInstalled(installed);
    },
    onError: (err) => {
      setSubmitError(errorMessage(err));
    },
  });

  const preview = previewQuery.data ?? null;
  const selectedVersionMeta = versions.find(
    (v) => v.version === selectedVersion,
  );
  const changelog = preview?.changelog || selectedVersionMeta?.changelog || null;
  const allowedHosts = preview?.allowed_hosts ?? item.allowed_hosts ?? [];
  const isUpdate = Boolean(item.installed);
  const isReplacingCustom = Boolean(
    preview?.already_installed && preview.installed_source === "custom",
  );

  const copySha256 = async (sha: string) => {
    try {
      await navigator.clipboard.writeText(sha);
      toast.success(m.plugins.js.toastSha256Copied);
    } catch {
      // Ignore clipboard failures in insecure contexts
    }
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={
        isUpdate
          ? m.plugins.js.updateModalTitle(item.name)
          : m.plugins.js.installModalTitle(item.name)
      }
      description={m.plugins.js.installModalDesc}
      size="lg"
      footer={
        <>
          <Button
            variant="outline"
            onClick={onClose}
            disabled={installMutation.isPending}
          >
            {m.common.cancel}
          </Button>
          <Button
            disabled={
              !preview ||
              !preview.compatible ||
              previewQuery.isFetching ||
              (isReplacingCustom && !replaceCustom)
            }
            loading={installMutation.isPending}
            onClick={() => {
              setSubmitError(null);
              installMutation.mutate();
            }}
          >
            {isUpdate ? (
              <>
                <ArrowUpCircle className="h-3.5 w-3.5" />
                {m.plugins.js.updateConfirmBtn}
              </>
            ) : (
              <>
                <Download className="h-3.5 w-3.5" />
                {m.plugins.js.installConfirmBtn}
              </>
            )}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        {versions.length > 1 && (
          <div>
            <Label htmlFor="market-plugin-version">
              {m.plugins.js.versionLabel}
            </Label>
            <Select
              id="market-plugin-version"
              value={selectedVersion}
              onChange={(e) => {
                setSelectedVersion(e.target.value);
                setSubmitError(null);
              }}
              className="mt-1 font-mono text-xs"
            >
              {versions.map((v) => (
                <option key={v.version} value={v.version}>
                  v{v.version}
                  {item.installed_version === v.version
                    ? m.plugins.js.versionInstalledSuffix
                    : ""}
                  {v.min_core ? ` (core >= ${v.min_core})` : ""}
                </option>
              ))}
            </Select>
          </div>
        )}

        {previewQuery.isPending ? (
          <div className="rounded-xl border border-zinc-200 bg-zinc-50/70 p-6 text-center text-xs text-zinc-500 dark:border-zinc-800 dark:bg-zinc-950/60 dark:text-zinc-400">
            {m.plugins.js.previewLoading}
          </div>
        ) : previewQuery.isError ? (
          <div className="space-y-3 rounded-xl border border-rose-200 bg-rose-50/70 p-4 text-xs text-rose-800 dark:border-rose-900/60 dark:bg-rose-950/30 dark:text-rose-200">
            <div className="flex items-start gap-2">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-rose-600 dark:text-rose-400" />
              <span>
                {m.plugins.js.previewErrorTitle}{" "}
                {errorMessage(previewQuery.error)}
              </span>
            </div>
            <div>
              <Button
                variant="outline"
                size="sm"
                onClick={() => void previewQuery.refetch()}
              >
                <RefreshCw className="h-3.5 w-3.5" />
                {m.common.retry}
              </Button>
            </div>
          </div>
        ) : preview ? (
          <>
            <div className="space-y-3 rounded-xl border border-zinc-200 bg-zinc-50/70 p-4 text-xs dark:border-zinc-800 dark:bg-zinc-950/60">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="flex flex-wrap items-center gap-1.5">
                  <span className="font-semibold text-zinc-900 dark:text-zinc-100">
                    {preview.manifest.name}
                  </span>
                  <Badge variant="mono">{preview.manifest.id}</Badge>
                  <Badge variant="mono">v{preview.manifest.version}</Badge>
                </div>
                <Badge
                  variant={
                    preview.manifest.kind === "channel" ? "info" : "warning"
                  }
                >
                  {preview.manifest.kind === "channel"
                    ? m.plugins.market.kindJsChannel
                    : m.plugins.market.kindJsProcessor}
                </Badge>
              </div>

              {!preview.compatible && (
                <div className="flex items-start gap-2 rounded-lg border border-rose-200 bg-rose-50/80 px-3 py-2 text-rose-800 dark:border-rose-900/60 dark:bg-rose-950/40 dark:text-rose-200">
                  <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-rose-600 dark:text-rose-400" />
                  <span>
                    {preview.compatibility_reason ||
                      m.plugins.market.incompatibleBadge}
                  </span>
                </div>
              )}

              <p className="leading-relaxed text-zinc-600 dark:text-zinc-300">
                {resolveLocalizedDescription(
                  preview.manifest.description,
                  preview.manifest.description_i18n,
                  locale,
                )}
              </p>

              {/* Verified SHA-256 */}
              <div className="space-y-1">
                <div className="flex items-center justify-between gap-2">
                  <span className="text-[11px] font-semibold tracking-wider text-zinc-500 uppercase">
                    {m.plugins.js.sha256Label}
                  </span>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-6 px-2 text-[11px]"
                    onClick={() => void copySha256(preview.script_sha256)}
                  >
                    <Copy className="h-3 w-3" />
                    {m.plugins.js.copySha256Btn}
                  </Button>
                </div>
                <div className="rounded-md border border-zinc-200/80 bg-white px-2.5 py-1.5 font-mono text-[11px] break-all text-zinc-700 dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-300">
                  {preview.script_sha256}
                </div>
              </div>

              {/* Allowed Hosts */}
              <div className="space-y-1.5">
                <div className="text-[11px] font-semibold tracking-wider text-zinc-500 uppercase">
                  {m.plugins.js.allowedHostsHeading}
                </div>
                {allowedHosts.length === 0 ? (
                  <p className="text-zinc-400">
                    {m.plugins.js.allowedHostsNone}
                  </p>
                ) : (
                  <div className="flex flex-wrap gap-1.5">
                    {allowedHosts.map((host) => (
                      <Badge key={host} variant="mono" className="text-[11px]">
                        <Globe className="h-3 w-3" />
                        {host}
                      </Badge>
                    ))}
                  </div>
                )}
              </div>
            </div>

            {isReplacingCustom ? (
              <div className="space-y-2.5 rounded-lg border border-rose-200 bg-rose-50/80 px-3 py-2.5 text-xs text-rose-900 dark:border-rose-900/70 dark:bg-rose-950/40 dark:text-rose-200">
                <div className="flex items-start gap-2">
                  <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-rose-600 dark:text-rose-400" />
                  <span className="leading-relaxed">
                    {m.plugins.js.replaceCustomWarning(
                      preview.manifest.id,
                      preview.installed_version,
                    )}
                  </span>
                </div>
                <label className="inline-flex cursor-pointer items-start gap-2 pl-6 font-medium text-rose-900 dark:text-rose-100">
                  <input
                    type="checkbox"
                    checked={replaceCustom}
                    onChange={(e) => setReplaceCustom(e.target.checked)}
                    className="mt-0.5 h-3.5 w-3.5 rounded border-rose-400 accent-rose-600 dark:border-rose-700 dark:accent-rose-500"
                  />
                  <span>{m.plugins.js.replaceCustomCheckbox}</span>
                </label>
              </div>
            ) : (
              preview.already_installed && (
                <div className="flex items-start gap-2 rounded-lg border border-sky-200 bg-sky-50/70 px-3 py-2.5 text-xs text-sky-900 dark:border-sky-900/60 dark:bg-sky-950/30 dark:text-sky-200">
                  <Info className="mt-0.5 h-4 w-4 shrink-0 text-sky-600 dark:text-sky-400" />
                  <span className="leading-relaxed">
                    {preview.installed_version &&
                    preview.installed_version !== preview.manifest.version
                      ? m.plugins.js.alreadyInstalledUpgradeNotice(
                          preview.installed_version,
                          preview.manifest.version,
                        )
                      : m.plugins.js.alreadyInstalledSameNotice(
                          preview.manifest.version,
                        )}
                  </span>
                </div>
              )
            )}

            {changelog && (
              <div className="space-y-1.5">
                <div className="text-[11px] font-semibold tracking-wider text-zinc-500 uppercase">
                  {m.plugins.js.changelogHeading}
                </div>
                <pre className="max-h-40 overflow-y-auto rounded-lg border border-zinc-200 bg-zinc-50/70 p-3 font-mono text-[11px] leading-relaxed whitespace-pre-wrap text-zinc-700 dark:border-zinc-800 dark:bg-zinc-950/60 dark:text-zinc-300">
                  {changelog}
                </pre>
              </div>
            )}
          </>
        ) : null}

        <div className="flex items-start gap-2 rounded-lg border border-zinc-200 bg-zinc-50/80 px-3 py-2.5 text-xs text-zinc-600 dark:border-zinc-800 dark:bg-zinc-950/60 dark:text-zinc-400">
          <Shield className="mt-0.5 h-4 w-4 shrink-0 text-emerald-600 dark:text-emerald-400" />
          <span className="leading-relaxed">{m.plugins.js.securityNotice}</span>
        </div>

        <div className="rounded-lg border border-zinc-200/80 bg-zinc-50/60 p-3 dark:border-zinc-800 dark:bg-zinc-950/60">
          <Switch
            id="market-plugin-enabled"
            checked={enabled}
            onCheckedChange={setEnabled}
            label={m.plugins.js.enabledLabel}
          />
        </div>

        <FieldError message={submitError} />
      </div>
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// Import Custom JS Plugin Modal (.js file upload or paste)
// ---------------------------------------------------------------------------

function UploadJsPluginModal({
  open,
  onClose,
  onInstalled,
}: {
  open: boolean;
  onClose: () => void;
  onInstalled: (plugin: PluginDto) => void;
}) {
  const { m, locale } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const fileInputRef = useRef<HTMLInputElement | null>(null);

  const [fileName, setFileName] = useState<string | null>(null);
  const [scriptJs, setScriptJs] = useState("");
  const [enabled, setEnabled] = useState(true);
  const [allowOverwrite, setAllowOverwrite] = useState(false);
  const [preview, setPreview] = useState<JsPluginPreviewResponse | null>(null);
  const [formError, setFormError] = useState<string | null>(null);

  const previewMutation = useMutation({
    mutationFn: () =>
      api.plugins.previewUpload({
        script_js: scriptJs,
        enabled,
        allow_overwrite: allowOverwrite,
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

  const uploadMutation = useMutation({
    mutationFn: () =>
      api.plugins.upload({
        script_js: scriptJs,
        enabled,
        allow_overwrite: allowOverwrite,
      }),
    onSuccess: (installed) => {
      toast.success(
        preview?.already_installed
          ? m.plugins.js.toastUpdated
          : m.plugins.js.toastInstalled,
      );
      void queryClient.invalidateQueries({ queryKey: ["plugins"] });
      void queryClient.invalidateQueries({ queryKey: ["plugins", "market"] });
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
      void queryClient.invalidateQueries({ queryKey: ["routes"] });
      void queryClient.invalidateQueries({ queryKey: ["tasks"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
      onInstalled(installed);
    },
    onError: (err) => {
      setFormError(errorMessage(err));
    },
  });

  const handleFileChange = async (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    try {
      const content = await file.text();
      setFileName(file.name);
      setScriptJs(content);
      setPreview(null);
      setFormError(null);
    } catch (err) {
      setFormError(errorMessage(err));
    }
  };

  const handlePreview = (e: FormEvent) => {
    e.preventDefault();
    if (!scriptJs.trim()) {
      setFormError(m.plugins.js.errSourceRequired);
      return;
    }
    setFormError(null);
    previewMutation.mutate();
  };

  const canConfirm = Boolean(
    preview &&
      preview.compatible &&
      (!preview.already_installed || allowOverwrite),
  );

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={m.plugins.js.uploadModalTitle}
      description={m.plugins.js.uploadModalDesc}
      size="xl"
      footer={
        <>
          <Button
            variant="outline"
            onClick={onClose}
            disabled={uploadMutation.isPending}
          >
            {m.common.cancel}
          </Button>
          <Button
            variant="outline"
            onClick={handlePreview}
            loading={previewMutation.isPending}
          >
            <FileCode2 className="h-3.5 w-3.5" />
            {m.plugins.js.previewBtn}
          </Button>
          <Button
            disabled={!canConfirm}
            loading={uploadMutation.isPending}
            onClick={() => {
              if (!scriptJs.trim()) {
                setFormError(m.plugins.js.errSourceRequired);
                return;
              }
              setFormError(null);
              uploadMutation.mutate();
            }}
          >
            <Upload className="h-3.5 w-3.5" />
            {preview?.already_installed
              ? m.plugins.js.updateConfirmBtn
              : m.plugins.js.installConfirmBtn}
          </Button>
        </>
      }
    >
      <form onSubmit={handlePreview} className="space-y-4">
        <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-dashed border-zinc-300 bg-zinc-50/70 px-3.5 py-3 dark:border-zinc-700 dark:bg-zinc-950/60">
          <div className="min-w-0 text-xs">
            <div className="font-medium text-zinc-800 dark:text-zinc-200">
              {m.plugins.js.fileInputLabel}
            </div>
            {fileName && (
              <div className="mt-0.5 truncate font-mono text-[11px] text-zinc-500">
                {m.plugins.js.fileLoadedHint(fileName)}
              </div>
            )}
          </div>
          <input
            ref={fileInputRef}
            type="file"
            accept=".js,.mjs,text/javascript"
            onChange={(e) => void handleFileChange(e)}
            className="hidden"
          />
          <Button
            variant="outline"
            size="sm"
            onClick={() => fileInputRef.current?.click()}
          >
            <Upload className="h-3.5 w-3.5" />
            {m.plugins.js.fileInputBtn}
          </Button>
        </div>

        <div>
          <Label htmlFor="upload-plugin-js">{m.plugins.js.sourceLabel}</Label>
          <Textarea
            id="upload-plugin-js"
            value={scriptJs}
            onChange={(e) => {
              setScriptJs(e.target.value);
              setPreview(null);
              setFormError(null);
            }}
            placeholder={m.plugins.js.sourcePlaceholder}
            className="mt-1 min-h-[200px] font-mono text-xs leading-relaxed"
            spellCheck={false}
          />
          <FieldHint>{m.plugins.js.sourceHint}</FieldHint>
        </div>

        <div className="space-y-2.5 rounded-lg border border-zinc-200/80 bg-zinc-50/60 p-3 dark:border-zinc-800 dark:bg-zinc-950/60">
          <Switch
            id="upload-plugin-enabled"
            checked={enabled}
            onCheckedChange={setEnabled}
            label={m.plugins.js.enabledLabel}
          />
          <label className="inline-flex cursor-pointer items-center gap-2 text-xs text-zinc-700 dark:text-zinc-300">
            <input
              type="checkbox"
              checked={allowOverwrite}
              onChange={(e) => setAllowOverwrite(e.target.checked)}
              className="h-3.5 w-3.5 rounded border-zinc-300 accent-zinc-900 dark:border-zinc-700 dark:accent-zinc-100"
            />
            <span>{m.plugins.js.overwriteLabel}</span>
          </label>
        </div>

        {preview && (
          <div className="space-y-3 rounded-xl border border-zinc-200 bg-zinc-50/70 p-4 text-xs dark:border-zinc-800 dark:bg-zinc-950/60">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <span className="text-[11px] font-semibold tracking-wider text-zinc-500 uppercase">
                {m.plugins.js.previewHeading}
              </span>
              <Badge
                variant={
                  preview.manifest.kind === "channel" ? "info" : "warning"
                }
              >
                {preview.manifest.kind === "channel"
                  ? m.plugins.market.kindJsChannel
                  : m.plugins.market.kindJsProcessor}
              </Badge>
            </div>
            <div className="flex flex-wrap items-center gap-1.5">
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
              <p className="text-zinc-600 dark:text-zinc-300">
                {resolveLocalizedDescription(
                  preview.manifest.description,
                  preview.manifest.description_i18n,
                  locale,
                )}
              </p>
            )}
            <div className="space-y-1">
              <div className="text-[11px] font-semibold tracking-wider text-zinc-500 uppercase">
                {m.plugins.js.sha256Label}
              </div>
              <div className="rounded-md border border-zinc-200/80 bg-white px-2.5 py-1.5 font-mono text-[11px] break-all text-zinc-700 dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-300">
                {preview.script_sha256}
              </div>
            </div>
            {preview.allowed_hosts.length > 0 && (
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="text-[11px] text-zinc-500">
                  {m.plugins.market.allowedHostsLabel}:
                </span>
                {preview.allowed_hosts.map((host) => (
                  <Badge key={host} variant="mono" className="text-[10px]">
                    <Globe className="h-3 w-3" />
                    {host}
                  </Badge>
                ))}
              </div>
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
            {preview.already_installed && !allowOverwrite && (
              <div className="flex items-start gap-2 rounded-lg border border-amber-200 bg-amber-50/80 px-3 py-2 text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-200">
                <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400" />
                <span className="leading-relaxed">
                  {m.plugins.js.overwriteRequiredHint(
                    preview.manifest.id,
                    preview.installed_version || preview.manifest.version,
                  )}
                </span>
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
// View JS Plugin Source Modal
// ---------------------------------------------------------------------------

function ViewPluginSourceModal({
  open,
  plugin,
  onClose,
}: {
  open: boolean;
  plugin: PluginDto;
  onClose: () => void;
}) {
  const { m } = useI18n();
  const toast = useToast();

  const sourceQuery = useQuery({
    queryKey: ["plugins", "source", plugin.id],
    queryFn: () => api.plugins.getSource(plugin.id),
    enabled: open,
  });

  const copyText = async (text: string, toastMsg: string) => {
    try {
      await navigator.clipboard.writeText(text);
      toast.success(toastMsg);
    } catch {
      // Ignore clipboard failures in insecure contexts
    }
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={m.plugins.js.sourceModalTitle(plugin.manifest.name)}
      size="2xl"
      footer={
        <>
          {sourceQuery.data && (
            <Button
              variant="outline"
              onClick={() =>
                void copyText(
                  sourceQuery.data.script_js,
                  m.plugins.js.toastSourceCopied,
                )
              }
            >
              <Copy className="h-3.5 w-3.5" />
              {m.plugins.js.copySourceBtn}
            </Button>
          )}
          <Button onClick={onClose}>{m.common.close}</Button>
        </>
      }
    >
      {sourceQuery.isPending ? (
        <div className="p-8 text-center text-xs text-zinc-500">
          {m.plugins.js.sourceModalLoading}
        </div>
      ) : sourceQuery.isError ? (
        <div className="flex items-start gap-2 rounded-lg border border-rose-200 bg-rose-50/70 p-3 text-xs text-rose-800 dark:border-rose-900/60 dark:bg-rose-950/30 dark:text-rose-200">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
          <span>
            {m.plugins.js.sourceModalError} {errorMessage(sourceQuery.error)}
          </span>
        </div>
      ) : sourceQuery.data ? (
        <div className="space-y-3 text-xs">
          <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-zinc-200 bg-zinc-50/70 px-3 py-2 dark:border-zinc-800 dark:bg-zinc-950/60">
            <div className="flex flex-wrap items-center gap-1.5">
              <Badge variant="mono">{sourceQuery.data.id}</Badge>
              <Badge variant="mono">v{sourceQuery.data.version}</Badge>
              <Badge variant="outline">{sourceQuery.data.source}</Badge>
            </div>
            <div className="flex items-center gap-1.5 font-mono text-[11px] text-zinc-500">
              <span className="max-w-[260px] truncate" title={sourceQuery.data.script_sha256}>
                sha256:{sourceQuery.data.script_sha256}
              </span>
              <Button
                variant="ghost"
                size="icon"
                className="h-6 w-6"
                title={m.plugins.js.copySha256Btn}
                aria-label={m.plugins.js.copySha256Btn}
                onClick={() =>
                  void copyText(
                    sourceQuery.data.script_sha256,
                    m.plugins.js.toastSha256Copied,
                  )
                }
              >
                <Copy className="h-3 w-3" />
              </Button>
            </div>
          </div>

          <pre className="max-h-[55vh] overflow-auto rounded-lg border border-zinc-200 bg-zinc-950 p-4 font-mono text-[11px] leading-relaxed text-zinc-100 dark:border-zinc-800">
            <code>{sourceQuery.data.script_js}</code>
          </pre>
        </div>
      ) : null}
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// Plugin Detail View (Providers Management)
// ---------------------------------------------------------------------------

function PluginDetailView({
  plugin,
  updateVersion,
  onBack,
  onToggleEnabled,
  togglingPlugin,
  onUpdateFromMarket,
  onViewSource,
  onUninstall,
}: {
  plugin: PluginDto;
  updateVersion: string | null;
  onBack: () => void;
  onToggleEnabled: (enabled: boolean) => void;
  togglingPlugin: boolean;
  onUpdateFromMarket?: () => void;
  onViewSource?: () => void;
  onUninstall?: () => void;
}) {
  const { m, locale } = useI18n();
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

  const localizedDesc = resolveLocalizedDescription(
    plugin.manifest.description,
    plugin.manifest.description_i18n,
    locale,
  );
  const allowedHosts =
    plugin.allowed_hosts ?? plugin.manifest.allowed_hosts ?? [];

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
      {plugin.disabled_reason && (
        <div className="flex items-start gap-2.5 rounded-xl border border-rose-200 bg-rose-50/80 px-3.5 py-2.5 text-xs text-rose-800 dark:border-rose-900/70 dark:bg-rose-950/40 dark:text-rose-200">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-rose-600 dark:text-rose-400" />
          <div className="min-w-0 leading-relaxed">
            <span className="font-semibold">
              {m.plugins.autoDisabledLabel}:
            </span>{" "}
            <span className="break-words">{plugin.disabled_reason}</span>
          </div>
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
                {plugin.runtime === "js" ? (
                  <Badge
                    variant={plugin.kind === "channel" ? "info" : "warning"}
                  >
                    {plugin.kind === "channel"
                      ? m.plugins.market.kindJsChannel
                      : m.plugins.market.kindJsProcessor}
                  </Badge>
                ) : (
                  <Badge variant="success">
                    {m.plugins.market.kindBuiltin}
                  </Badge>
                )}
                {plugin.source === "custom" && (
                  <Badge variant="outline">
                    {m.plugins.market.customSourceBadge}
                  </Badge>
                )}
                <Badge variant="outline">{plugin.manifest.category}</Badge>
                {!plugin.compatible && (
                  <Badge variant="danger">
                    {plugin.compatibility_reason ||
                      m.plugins.market.incompatibleBadge}
                  </Badge>
                )}
                {plugin.disabled_reason ? (
                  <Badge variant="danger">{m.plugins.autoDisabledLabel}</Badge>
                ) : (
                  !plugin.enabled && (
                    <Badge variant="default">{m.common.disabled}</Badge>
                  )
                )}
              </div>
              <p className="max-w-2xl text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">
                {localizedDesc}
              </p>
              <div className="flex flex-wrap items-center gap-2 pt-0.5 text-[11px] text-zinc-500">
                {plugin.manifest.hooks.length > 0 && (
                  <span className="inline-flex items-center gap-1">
                    {m.plugins.hooksLabel}:
                    {plugin.manifest.hooks.map((h) => (
                      <Badge key={h} variant="mono" className="text-[10px]">
                        {h}
                      </Badge>
                    ))}
                  </span>
                )}
                {plugin.manifest.permissions.length > 0 && (
                  <span className="inline-flex items-center gap-1">
                    {m.plugins.permissionsLabel}:
                    {plugin.manifest.permissions.map((perm) => (
                      <Badge
                        key={perm}
                        variant="outline"
                        className="text-[10px]"
                      >
                        {perm}
                      </Badge>
                    ))}
                  </span>
                )}
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
            {onUpdateFromMarket && updateVersion && (
              <Button variant="primary" size="sm" onClick={onUpdateFromMarket}>
                <ArrowUpCircle className="h-3.5 w-3.5" />
                {m.plugins.market.updateToBtn(updateVersion)}
              </Button>
            )}
            {onViewSource && (
              <Button variant="outline" size="sm" onClick={onViewSource}>
                <Code2 className="h-3.5 w-3.5" />
                {m.plugins.js.viewSourceBtn}
              </Button>
            )}
            {onUninstall && (
              <Button
                variant="outline"
                size="sm"
                onClick={onUninstall}
                className="text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-950/40"
              >
                <Trash2 className="h-3.5 w-3.5" />
                {m.plugins.js.uninstallBtn}
              </Button>
            )}
            <Switch
              checked={plugin.enabled}
              disabled={togglingPlugin}
              onCheckedChange={onToggleEnabled}
            />
          </div>
        </div>

        {(allowedHosts.length > 0 || plugin.script_sha256) && (
          <div className="mt-3 flex flex-wrap items-center gap-2 border-t border-zinc-100 pt-3 text-xs text-zinc-500 dark:border-zinc-800/80">
            {allowedHosts.length > 0 && (
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="inline-flex items-center gap-1 text-[11px]">
                  <Shield className="h-3.5 w-3.5 shrink-0" />
                  {m.plugins.market.allowedHostsLabel}:
                </span>
                {allowedHosts.map((host) => (
                  <Badge key={host} variant="mono" className="text-[10px]">
                    {host}
                  </Badge>
                ))}
              </div>
            )}
            {plugin.script_sha256 && (
              <Badge
                variant="mono"
                className="text-[10px]"
                title={plugin.script_sha256}
              >
                sha256:{plugin.script_sha256.slice(0, 12)}…
              </Badge>
            )}
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
              {m.plugins.js.channelSummaryTitle}
            </h2>
            <p className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
              {m.plugins.js.channelSummaryDesc}
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
                      {m.plugins.js.accountSchemaFields(acctProps.length)}
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
                      {m.plugins.js.destSchemaFields(destProps.length)}
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
