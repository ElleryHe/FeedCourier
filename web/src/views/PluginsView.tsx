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
import { api, getReferencingRoutesConflict } from "@/api/client";
import type {
  MarketPluginItemDto,
  PluginDto,
  PluginInstanceDto,
  RouteReferenceDto,
  YourlsAuthMode,
  YourlsProviderConfigView,
} from "@/api/types";
import { PluginIcon } from "@/components/plugins/PluginIcon";
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

interface InstanceSchemaProperty {
  type?: string;
  title?: string;
  description?: string;
  placeholder?: string;
  enum?: Array<string | number>;
  format?: string;
  default?: unknown;
  items?: { type?: string };
}

interface ParsedInstanceSchema {
  properties: Array<[string, InstanceSchemaProperty]>;
  required: ReadonlySet<string>;
}

function isPlainRecord(val: unknown): val is Record<string, unknown> {
  return typeof val === "object" && val !== null && !Array.isArray(val);
}

function parseInstanceSchema(
  raw: Record<string, unknown> | null | undefined,
): ParsedInstanceSchema {
  if (!isPlainRecord(raw)) {
    return { properties: [], required: new Set() };
  }
  const required = new Set<string>(
    Array.isArray(raw.required)
      ? raw.required.filter((x): x is string => typeof x === "string")
      : [],
  );
  const properties: Array<[string, InstanceSchemaProperty]> = [];
  if (isPlainRecord(raw.properties)) {
    for (const [key, propVal] of Object.entries(raw.properties)) {
      if (!isPlainRecord(propVal)) continue;
      properties.push([key, propVal as InstanceSchemaProperty]);
    }
  }
  return { properties, required };
}

function formatGenericConfigEntries(
  config: Record<string, unknown>,
): Array<{ key: string; value: string; isSecretHint?: boolean }> {
  const entries: Array<{ key: string; value: string; isSecretHint?: boolean }> =
    [];
  for (const [key, val] of Object.entries(config)) {
    if (val === null || val === undefined || val === "") continue;
    if (key.startsWith("has_") && typeof val === "boolean") continue;
    if (key.endsWith("_hint") && typeof val === "string") {
      entries.push({
        key: key.slice(0, -5),
        value: val,
        isSecretHint: true,
      });
      continue;
    }
    if (typeof val === "boolean") {
      entries.push({ key, value: val ? "true" : "false" });
    } else if (typeof val === "number") {
      entries.push({ key, value: String(val) });
    } else if (typeof val === "string") {
      entries.push({ key, value: val });
    } else if (Array.isArray(val)) {
      if (val.length > 0) {
        entries.push({ key, value: val.map((v) => String(v)).join(", ") });
      }
    }
  }
  return entries;
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
    return (
      <PluginDetailView
        plugin={selectedPlugin}
        onBack={() => setSelectedPluginId(null)}
        onToggleEnabled={(enabled) =>
          togglePluginMutation.mutate({ id: selectedPlugin.id, enabled })
        }
        togglingPlugin={togglePluginMutation.isPending}
      />
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
                            {!plugin.enabled && (
                              <Badge variant="default">
                                {m.common.disabled}
                              </Badge>
                            )}
                          </div>
                          <div className="mt-1 flex flex-wrap items-center gap-1.5">
                            <Badge variant="outline">
                              {plugin.manifest.category}
                            </Badge>
                            <Badge
                              variant={
                                plugin.instance_count > 0 ? "info" : "default"
                              }
                            >
                              {m.plugins.providerCount(plugin.instance_count)}
                            </Badge>
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

                    <Button
                      variant="outline"
                      size="sm"
                      onClick={(e) => {
                        e.stopPropagation();
                        setSelectedPluginId(plugin.id);
                      }}
                    >
                      <Settings2 className="h-3.5 w-3.5" />
                      {m.plugins.configureBtn}
                    </Button>
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
                              ) : (
                                <Badge variant="warning">
                                  {m.plugins.market.wasmUpcomingBadge}
                                </Badge>
                              )}
                            </div>
                            <div className="mt-1 flex flex-wrap items-center gap-1.5">
                              {item.category && (
                                <Badge variant="outline">{item.category}</Badge>
                              )}
                              {item.enabled && (
                                <Badge variant="info">
                                  <CheckCircle2 className="h-3 w-3" />
                                  {m.plugins.market.enabledBadge}
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
                    </div>
                  </Card>
                );
              })}
            </div>
          )}
        </div>
      )}
    </div>
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
}: {
  plugin: PluginDto;
  onBack: () => void;
  onToggleEnabled: (enabled: boolean) => void;
  togglingPlugin: boolean;
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

          <div className="flex items-center gap-2">
            <Switch
              checked={plugin.enabled}
              disabled={togglingPlugin}
              onCheckedChange={onToggleEnabled}
            />
          </div>
        </div>

        {!plugin.enabled && (
          <div className="mt-3.5 flex items-start gap-2 rounded-lg border border-amber-200 bg-amber-50/80 px-3 py-2 text-xs text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-200">
            <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400" />
            <span>{m.plugins.globallyDisabledNotice}</span>
          </div>
        )}
      </Card>

      {/* Provider Instances Section */}
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
                    const summaryEntries = formatGenericConfigEntries(
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

  const parsedSchema = useMemo(
    () => parseInstanceSchema(plugin.manifest.instance_schema),
    [plugin.manifest.instance_schema],
  );

  const [name, setName] = useState(instance?.name ?? "");
  const [enabled, setEnabled] = useState(instance?.enabled ?? true);
  const [fieldValues, setFieldValues] = useState<
    Record<string, string | boolean>
  >(() => {
    const initial: Record<string, string | boolean> = {};
    for (const [key, prop] of parsedSchema.properties) {
      const isSecret = prop.format === "password";
      if (isSecret) {
        initial[key] = "";
        continue;
      }
      const existingVal =
        isEdit && instance && key in instance.config
          ? instance.config[key]
          : undefined;

      if (existingVal !== undefined && existingVal !== null) {
        if (prop.type === "boolean") {
          initial[key] = Boolean(existingVal);
        } else if (prop.type === "array") {
          initial[key] = Array.isArray(existingVal)
            ? existingVal.map((item) => String(item)).join("\n")
            : String(existingVal);
        } else {
          initial[key] = String(existingVal);
        }
      } else if (prop.default !== undefined && prop.default !== null) {
        if (prop.type === "boolean") {
          initial[key] = Boolean(prop.default);
        } else if (prop.type === "array" && Array.isArray(prop.default)) {
          initial[key] = prop.default.map((item) => String(item)).join("\n");
        } else {
          initial[key] = String(prop.default);
        }
      } else if (
        parsedSchema.required.has(key) &&
        Array.isArray(prop.enum) &&
        prop.enum.length > 0
      ) {
        initial[key] = String(prop.enum[0]);
      } else {
        initial[key] = prop.type === "boolean" ? false : "";
      }
    }
    return initial;
  });
  const [formError, setFormError] = useState<string | null>(null);

  const hasStoredSecretFor = (key: string): boolean => {
    if (!isEdit || !instance) return false;
    return Boolean(
      instance.config[`has_${key}`] ||
        (typeof instance.config[`${key}_hint`] === "string" &&
          instance.config[`${key}_hint`]),
    );
  };

  const secretHintFor = (key: string): string | null => {
    if (!isEdit || !instance) return null;
    const hint = instance.config[`${key}_hint`];
    return typeof hint === "string" && hint ? hint : null;
  };

  const buildValidatedConfig = (): {
    config: Record<string, unknown> | null;
    error: string | null;
  } => {
    const config: Record<string, unknown> = {};

    for (const [key, prop] of parsedSchema.properties) {
      const label = prop.title || key;
      const isRequired = parsedSchema.required.has(key);
      const isSecret = prop.format === "password";
      const raw = fieldValues[key];

      if (prop.type === "boolean") {
        config[key] = Boolean(raw);
        continue;
      }

      const strVal = typeof raw === "string" ? raw : "";

      if (prop.type === "array") {
        const items = strVal
          .split(/[\n,]+/)
          .map((s) => s.trim())
          .filter((s) => s.length > 0);
        if (isRequired && items.length === 0) {
          return {
            config: null,
            error: m.plugins.providerModal.errFieldRequired(label),
          };
        }
        config[key] = items;
        continue;
      }

      if (Array.isArray(prop.enum) && prop.enum.length > 0) {
        if (!strVal) {
          if (isRequired) {
            return {
              config: null,
              error: m.plugins.providerModal.errFieldRequired(label),
            };
          }
          continue;
        }
        const matched = prop.enum.find((opt) => String(opt) === strVal);
        config[key] = matched !== undefined ? matched : strVal;
        continue;
      }

      if (prop.type === "number" || prop.type === "integer") {
        const trimmed = strVal.trim();
        if (!trimmed) {
          if (isRequired) {
            return {
              config: null,
              error: m.plugins.providerModal.errFieldRequired(label),
            };
          }
          continue;
        }
        const num = Number(trimmed);
        if (!Number.isFinite(num)) {
          return {
            config: null,
            error: m.plugins.providerModal.errFieldInvalidNumber(label),
          };
        }
        if (prop.type === "integer" && !Number.isInteger(num)) {
          return {
            config: null,
            error: m.plugins.providerModal.errFieldInvalidInteger(label),
          };
        }
        config[key] = num;
        continue;
      }

      if (isSecret) {
        if (strVal.length > 0) {
          config[key] = strVal;
        } else if (isRequired && !hasStoredSecretFor(key)) {
          return {
            config: null,
            error: m.plugins.providerModal.errFieldRequired(label),
          };
        }
        continue;
      }

      const trimmed = strVal.trim();
      if (!trimmed) {
        if (isRequired) {
          return {
            config: null,
            error: m.plugins.providerModal.errFieldRequired(label),
          };
        }
        continue;
      }
      if (prop.format === "uri" && !isSafeHttpUrl(trimmed)) {
        return {
          config: null,
          error: m.plugins.providerModal.errFieldInvalidUrl(label),
        };
      }
      config[key] = trimmed;
    }

    return { config, error: null };
  };

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

    const { config, error } = buildValidatedConfig();
    if (error || !config) {
      setFormError(error);
      return;
    }

    saveMutation.mutate(config);
  };

  const booleanProps = parsedSchema.properties.filter(
    ([, prop]) => prop.type === "boolean",
  );
  const nonBooleanProps = parsedSchema.properties.filter(
    ([, prop]) => prop.type !== "boolean",
  );

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

        {parsedSchema.properties.length === 0 ? (
          <p className="text-xs text-zinc-500 dark:text-zinc-400">
            {m.plugins.providerModal.genericNoSchemaHint}
          </p>
        ) : (
          <>
            {nonBooleanProps.length > 0 && (
              <div className="space-y-3">
                {nonBooleanProps.map(([key, prop]) => {
                  const fieldId = `schema-provider-field-${key}`;
                  const label = prop.title || key;
                  const isRequired = parsedSchema.required.has(key);
                  const isSecret = prop.format === "password";
                  const storedSecret = hasStoredSecretFor(key);
                  const hint = secretHintFor(key);
                  const strVal =
                    typeof fieldValues[key] === "string"
                      ? fieldValues[key]
                      : "";

                  if (Array.isArray(prop.enum) && prop.enum.length > 0) {
                    return (
                      <div key={key}>
                        <Label htmlFor={fieldId}>
                          {label}
                          {isRequired && (
                            <span className="ml-1 text-rose-500">*</span>
                          )}
                        </Label>
                        <Select
                          id={fieldId}
                          value={strVal}
                          onChange={(e) =>
                            setFieldValues((prev) => ({
                              ...prev,
                              [key]: e.target.value,
                            }))
                          }
                          className="mt-1"
                        >
                          {(!isRequired || !strVal) && (
                            <option value="">
                              {m.plugins.providerModal.genericSelectPlaceholder}
                            </option>
                          )}
                          {prop.enum.map((opt) => (
                            <option key={String(opt)} value={String(opt)}>
                              {String(opt)}
                            </option>
                          ))}
                        </Select>
                        {prop.description && (
                          <FieldHint>{prop.description}</FieldHint>
                        )}
                      </div>
                    );
                  }

                  if (prop.type === "array") {
                    return (
                      <div key={key}>
                        <Label htmlFor={fieldId}>
                          {label}
                          {isRequired && (
                            <span className="ml-1 text-rose-500">*</span>
                          )}
                        </Label>
                        <Textarea
                          id={fieldId}
                          value={strVal}
                          onChange={(e) =>
                            setFieldValues((prev) => ({
                              ...prev,
                              [key]: e.target.value,
                            }))
                          }
                          placeholder={prop.placeholder}
                          className="mt-1 min-h-[72px] font-mono text-xs"
                        />
                        <FieldHint>
                          {prop.description ||
                            m.plugins.providerModal.genericArrayHint}
                        </FieldHint>
                      </div>
                    );
                  }

                  const inputType = isSecret
                    ? "password"
                    : prop.type === "number" || prop.type === "integer"
                      ? "number"
                      : prop.format === "uri"
                        ? "url"
                        : "text";

                  const placeholder = isSecret
                    ? storedSecret
                      ? hint
                        ? m.plugins.providerModal.signatureStoredPlaceholder(
                            hint,
                          )
                        : m.plugins.providerModal.genericSecretStoredPlaceholder
                      : prop.placeholder ||
                        m.plugins.providerModal.genericSecretPlaceholder
                    : prop.placeholder;

                  return (
                    <div key={key}>
                      <Label htmlFor={fieldId}>
                        {label}
                        {isRequired && (
                          <span className="ml-1 text-rose-500">*</span>
                        )}
                      </Label>
                      <Input
                        id={fieldId}
                        type={inputType}
                        step={prop.type === "integer" ? 1 : "any"}
                        value={strVal}
                        onChange={(e) =>
                          setFieldValues((prev) => ({
                            ...prev,
                            [key]: e.target.value,
                          }))
                        }
                        placeholder={placeholder}
                        className="mt-1 font-mono text-xs"
                      />
                      {isSecret && storedSecret ? (
                        <FieldHint>
                          {prop.description
                            ? `${prop.description} ${m.plugins.providerModal.secretPreserveHint}`
                            : m.plugins.providerModal.secretPreserveHint}
                        </FieldHint>
                      ) : (
                        prop.description && (
                          <FieldHint>{prop.description}</FieldHint>
                        )
                      )}
                    </div>
                  );
                })}
              </div>
            )}

            <div className="space-y-3 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
              {booleanProps.map(([key, prop]) => (
                <Switch
                  key={key}
                  id={`schema-provider-bool-${key}`}
                  checked={Boolean(fieldValues[key])}
                  onCheckedChange={(checked) =>
                    setFieldValues((prev) => ({ ...prev, [key]: checked }))
                  }
                  label={prop.title || key}
                  description={prop.description}
                />
              ))}
              <Switch
                id="schema-provider-enabled"
                checked={enabled}
                onCheckedChange={setEnabled}
                label={m.plugins.providerModal.enabledLabel}
                description={m.plugins.providerModal.enabledDesc}
              />
            </div>
          </>
        )}

        {parsedSchema.properties.length === 0 && (
          <div className="rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
            <Switch
              id="schema-provider-enabled-only"
              checked={enabled}
              onCheckedChange={setEnabled}
              label={m.plugins.providerModal.enabledLabel}
              description={m.plugins.providerModal.enabledDesc}
            />
          </div>
        )}

        <FieldError message={formError} />
      </form>
    </Modal>
  );
}
