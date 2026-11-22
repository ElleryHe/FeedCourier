import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  CheckCircle2,
  Database,
  Globe,
  Lock,
  Mail,
  MessageSquare,
  Pencil,
  Plus,
  Radio,
  RefreshCw,
  Send,
  ShieldCheck,
  Sparkles,
  Trash2,
  Wrench,
} from "lucide-react";
import { type FormEvent, useMemo, useState } from "react";
import { api } from "@/api/client";
import {
  type ChannelAccountCreate,
  type ChannelAccountPatch,
  type ChannelDestinationDto,
  type ChannelDto,
  type ChannelKind,
  type CreateChannelRequest,
  type CreateDestinationRequest,
  DEFAULT_NOTION_COLUMN_MAPPING,
  type DestinationConfigCreate,
  type DestinationPatch,
  type NotionColumnMapping,
  type NotionInspectSchemaResponse,
  type NotionStandardFieldKey,
  type SmtpSecurity,
  type UpdateChannelRequest,
  type UpdateDestinationRequest,
} from "@/api/types";
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
  TagInput,
} from "@/components/ui/primitives";
import { useToast } from "@/components/ui/toast";
import { useI18n } from "@/i18n";
import type { Messages } from "@/i18n/en";
import {
  errorMessage,
  formatDateTime,
  formatDurationSecs,
  formatRelativeTime,
} from "@/lib/utils";

const NOTION_STANDARD_COLUMNS: Array<{
  key: NotionStandardFieldKey;
  standardName: string;
  expectedType: string;
  required: boolean;
}> = [
  {
    key: "title",
    standardName: "Title",
    expectedType: "title",
    required: true,
  },
  {
    key: "fc_id",
    standardName: "FC-ID",
    expectedType: "rich_text",
    required: true,
  },
  {
    key: "link",
    standardName: "Link",
    expectedType: "url",
    required: false,
  },
  {
    key: "source",
    standardName: "Source",
    expectedType: "select",
    required: false,
  },
  {
    key: "published",
    standardName: "Published",
    expectedType: "date",
    required: false,
  },
  {
    key: "state",
    standardName: "State",
    expectedType: "select",
    required: false,
  },
  {
    key: "tag",
    standardName: "Tag",
    expectedType: "multi_select",
    required: false,
  },
];

function kindDefaults(kind: ChannelKind) {
  switch (kind) {
    case "smtp":
      return { icon: Mail, defaultInterval: 15, defaultJitter: 10 };
    case "telegram":
      return { icon: MessageSquare, defaultInterval: 3, defaultJitter: 2 };
    case "notion":
      return { icon: Database, defaultInterval: 2, defaultJitter: 2 };
  }
}

function kindMeta(kind: ChannelKind, m: Messages) {
  const base = kindDefaults(kind);
  return {
    ...base,
    label: m.channels.kindLabels[kind],
  };
}

export function ChannelsView() {
  const { m, locale } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();

  const [kindFilter, setKindFilter] = useState<ChannelKind | "all">("all");
  const [accountModalOpen, setAccountModalOpen] = useState(false);
  const [editingChannel, setEditingChannel] = useState<ChannelDto | null>(null);
  const [deletingChannel, setDeletingChannel] = useState<ChannelDto | null>(
    null,
  );
  const [testingChannelId, setTestingChannelId] = useState<number | null>(null);

  const [destModalState, setDestModalState] = useState<{
    channel: ChannelDto;
    destination: ChannelDestinationDto | null;
  } | null>(null);
  const [deletingDest, setDeletingDest] =
    useState<ChannelDestinationDto | null>(null);
  const [testingDestId, setTestingDestId] = useState<number | null>(null);

  const channelsQuery = useQuery({
    queryKey: ["channels"],
    queryFn: () => api.channels.list(),
  });

  const allChannels = channelsQuery.data ?? [];
  const filteredChannels = useMemo(
    () =>
      kindFilter === "all"
        ? allChannels
        : allChannels.filter((c) => c.kind === kindFilter),
    [allChannels, kindFilter],
  );

  const testAccountMutation = useMutation({
    mutationFn: (id: number) => {
      setTestingChannelId(id);
      return api.channels.test(id, { locale });
    },
    onSuccess: (res) => {
      if (res.ok) {
        toast.success(m.channels.toastAccountTestOk, res.message);
      } else {
        toast.error(m.channels.toastAccountTestIssue, res.message);
      }
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
    },
    onError: (err) => {
      toast.error(m.channels.toastAccountTestFailed, errorMessage(err));
    },
    onSettled: () => setTestingChannelId(null),
  });

  const toggleAccountMutation = useMutation({
    mutationFn: ({ id, enabled }: { id: number; enabled: boolean }) =>
      api.channels.update(id, { enabled }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
    },
    onError: (err) => {
      toast.error(m.channels.toastToggleFailed, errorMessage(err));
    },
  });

  const deleteAccountMutation = useMutation({
    mutationFn: (id: number) => api.channels.remove(id),
    onSuccess: () => {
      toast.success(m.channels.toastDeleted);
      setDeletingChannel(null);
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
      void queryClient.invalidateQueries({ queryKey: ["routes"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
    },
    onError: (err) => {
      toast.error(m.channels.toastDeleteFailed, errorMessage(err));
    },
  });

  const testDestMutation = useMutation({
    mutationFn: (destId: number) => {
      setTestingDestId(destId);
      return api.destinations.test(destId, { locale });
    },
    onSuccess: (res) => {
      if (res.ok) {
        toast.success(m.channels.toastDestTestOk, res.message);
      } else {
        toast.error(m.channels.toastDestTestIssue, res.message);
      }
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
    },
    onError: (err) => {
      toast.error(m.channels.toastDestTestFailed, errorMessage(err));
    },
    onSettled: () => setTestingDestId(null),
  });

  const toggleDestMutation = useMutation({
    mutationFn: ({ id, enabled }: { id: number; enabled: boolean }) =>
      api.destinations.update(id, { enabled }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
    },
    onError: (err) => {
      toast.error(m.channels.toastDestToggleFailed, errorMessage(err));
    },
  });

  const deleteDestMutation = useMutation({
    mutationFn: (id: number) => api.destinations.remove(id),
    onSuccess: () => {
      toast.success(m.channels.toastDestDeleted);
      setDeletingDest(null);
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
      void queryClient.invalidateQueries({ queryKey: ["routes"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
    },
    onError: (err) => {
      toast.error(m.channels.toastDestDeleteFailed, errorMessage(err));
    },
  });

  return (
    <div className="space-y-5">
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-lg font-bold tracking-tight">
            {m.channels.title}
          </h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400">
            {m.channels.subtitle}
          </p>
        </div>
        <Button
          size="sm"
          onClick={() => {
            setEditingChannel(null);
            setAccountModalOpen(true);
          }}
        >
          <Plus className="h-3.5 w-3.5" />
          {m.channels.newChannel}
        </Button>
      </div>

      {/* Channel Kind Filter */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-1 rounded-lg border border-zinc-200 bg-white p-1 dark:border-zinc-800 dark:bg-zinc-900">
          {(
            [
              { id: "all", label: m.common.all },
              { id: "telegram", label: m.channels.kindLabels.telegram },
              { id: "smtp", label: m.channels.kindLabels.smtp },
              { id: "notion", label: m.channels.kindLabels.notion },
            ] as const
          ).map((tab) => (
            <button
              key={tab.id}
              type="button"
              onClick={() => setKindFilter(tab.id)}
              className={`cursor-pointer rounded-md px-2.5 py-1 text-xs font-medium transition-colors ${
                kindFilter === tab.id
                  ? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
                  : "text-zinc-600 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:bg-zinc-800"
              }`}
            >
              {tab.label}
            </button>
          ))}
        </div>
      </div>

      {/* Accounts + Nested Destinations List */}
      {channelsQuery.isLoading ? (
        <Card className="p-8 text-center text-xs text-zinc-500">
          {m.channels.loading}
        </Card>
      ) : allChannels.length === 0 ? (
        <EmptyState
          icon={Radio}
          title={m.channels.emptyTitle}
          description={m.channels.emptyDesc}
          action={
            <Button
              size="sm"
              onClick={() => {
                setEditingChannel(null);
                setAccountModalOpen(true);
              }}
            >
              <Plus className="h-3.5 w-3.5" />
              {m.channels.configureFirst}
            </Button>
          }
        />
      ) : filteredChannels.length === 0 ? (
        <EmptyState
          icon={Radio}
          title={m.channels.emptyFilteredTitle}
          description={m.channels.emptyFilteredDesc}
        />
      ) : (
        <div className="space-y-4">
          {filteredChannels.map((ch) => {
            const meta = kindMeta(ch.kind, m);
            const Icon = meta.icon;
            const hasCooldown =
              ch.cooldown_until &&
              new Date(ch.cooldown_until).getTime() > Date.now();

            return (
              <Card key={ch.id} className="overflow-hidden">
                {/* Layer 1: Account Header & Credentials */}
                <div className="space-y-3 p-4">
                  <div className="flex flex-wrap items-start justify-between gap-3">
                    <div className="flex min-w-0 items-start gap-3">
                      <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border border-zinc-200 bg-zinc-50 text-zinc-700 dark:border-zinc-800 dark:bg-zinc-950 dark:text-zinc-200">
                        <Icon className="h-4 w-4" />
                      </div>
                      <div className="min-w-0">
                        <div className="flex flex-wrap items-center gap-1.5">
                          <h3 className="text-sm font-semibold">{ch.name}</h3>
                          <Badge variant="outline">{meta.label}</Badge>
                          {!ch.enabled && (
                            <Badge variant="default">{m.common.disabled}</Badge>
                          )}
                          {ch.use_proxy && (
                            <Badge variant="outline">
                              <Globe className="h-3 w-3" />
                              {m.common.proxy}
                            </Badge>
                          )}
                          {ch.kind === "telegram" &&
                            ch.capabilities.supports_rich_message !==
                              undefined && (
                              <Badge
                                variant={
                                  ch.capabilities.supports_rich_message
                                    ? "success"
                                    : "warning"
                                }
                              >
                                {ch.capabilities.supports_rich_message
                                  ? m.channels.card.richReady
                                  : m.channels.card.richUnsupported}
                              </Badge>
                            )}
                          {hasCooldown && ch.cooldown_until && (
                            <Badge
                              variant="warning"
                              title={formatDateTime(ch.cooldown_until)}
                            >
                              {m.channels.card.cooldownPrefix(
                                formatRelativeTime(ch.cooldown_until),
                              )}
                            </Badge>
                          )}
                        </div>
                        <p className="mt-1 font-mono text-[11px] text-zinc-500">
                          {m.channels.pacingLine(
                            formatDurationSecs(ch.min_interval_secs),
                            formatDurationSecs(ch.jitter_max_secs),
                            ch.hourly_quota,
                            ch.daily_quota,
                          )}
                        </p>
                      </div>
                    </div>

                    {/* Account Actions */}
                    <div className="flex flex-wrap items-center gap-1.5">
                      <Switch
                        checked={ch.enabled}
                        onCheckedChange={(enabled) =>
                          toggleAccountMutation.mutate({ id: ch.id, enabled })
                        }
                      />
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => testAccountMutation.mutate(ch.id)}
                        loading={testingChannelId === ch.id}
                      >
                        <ShieldCheck className="h-3.5 w-3.5" />
                        {m.channels.card.testAccount}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => {
                          setEditingChannel(ch);
                          setAccountModalOpen(true);
                        }}
                      >
                        <Pencil className="h-3.5 w-3.5" />
                        {m.channels.card.editAccount}
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={() => setDeletingChannel(ch)}
                        className="text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-950/40"
                        title={m.channels.card.deleteTitle}
                        aria-label={m.channels.card.deleteTitle}
                      >
                        <Trash2 className="h-4 w-4" />
                      </Button>
                    </div>
                  </div>

                  {/* Masked Account Credential Summary */}
                  <div className="rounded-lg border border-zinc-100 bg-zinc-50/70 px-3 py-2 text-xs dark:border-zinc-800/80 dark:bg-zinc-950/60">
                    {ch.config.kind === "smtp" && (
                      <div className="flex flex-wrap items-center gap-x-5 gap-y-1">
                        <div className="flex items-center gap-1.5">
                          <span className="text-zinc-500">
                            {m.channels.card.server}
                          </span>
                          <span className="font-mono">
                            {ch.config.host}:{ch.config.port} (
                            {ch.config.security})
                          </span>
                        </div>
                        <div className="flex items-center gap-1.5">
                          <span className="text-zinc-500">
                            {m.channels.card.fromAddress}
                          </span>
                          <span className="font-mono">
                            {ch.config.from_address}
                          </span>
                        </div>
                        <div className="flex items-center gap-1.5">
                          <span className="text-zinc-500">
                            {m.channels.card.auth}
                          </span>
                          <span className="inline-flex items-center gap-1 font-mono text-[11px] text-zinc-500">
                            <Lock className="h-3 w-3" />
                            {ch.config.username || m.channels.card.anonymous}{" "}
                            {ch.config.has_password
                              ? "(••••••••)"
                              : m.channels.card.noPassword}
                          </span>
                        </div>
                      </div>
                    )}

                    {ch.config.kind === "telegram" && (
                      <div className="flex flex-wrap items-center gap-x-5 gap-y-1">
                        <div className="flex items-center gap-1.5">
                          <span className="text-zinc-500">
                            {m.channels.card.botToken}
                          </span>
                          <span className="inline-flex items-center gap-1 font-mono text-[11px] text-zinc-500">
                            <Lock className="h-3 w-3" />
                            {ch.config.bot_token_hint}
                            {ch.capabilities.bot_username
                              ? ` (@${ch.capabilities.bot_username})`
                              : ""}
                          </span>
                        </div>
                        <div className="flex items-center gap-1.5">
                          <span className="text-zinc-500">
                            {m.channels.card.botApi}
                          </span>
                          <span className="truncate font-mono text-[11px]">
                            {ch.config.api_base_url ||
                              "https://api.telegram.org"}
                          </span>
                        </div>
                      </div>
                    )}

                    {ch.config.kind === "notion" && (
                      <div className="flex flex-wrap items-center gap-x-5 gap-y-1">
                        <div className="flex items-center gap-1.5">
                          <span className="text-zinc-500">
                            {m.channels.card.secret}
                          </span>
                          <span className="inline-flex items-center gap-1 font-mono text-[11px] text-zinc-500">
                            <Lock className="h-3 w-3" />
                            {ch.config.secret_hint}
                          </span>
                        </div>
                      </div>
                    )}
                  </div>

                  {ch.capabilities.fallback_reason && (
                    <div>
                      <Badge variant="warning">
                        {m.channels.card.fallbackPrefix(
                          ch.capabilities.fallback_reason,
                        )}
                      </Badge>
                    </div>
                  )}
                </div>

                {/* Layer 2: Nested Destinations List */}
                <div className="border-t border-zinc-100 bg-zinc-50/40 px-4 py-3 dark:border-zinc-800/80 dark:bg-zinc-950/30">
                  <div className="mb-2.5 flex flex-wrap items-center justify-between gap-2">
                    <span className="text-[11px] font-semibold tracking-wider text-zinc-500 uppercase dark:text-zinc-400">
                      {m.channels.destinationsHeading(ch.destinations.length)}
                    </span>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() =>
                        setDestModalState({ channel: ch, destination: null })
                      }
                    >
                      <Plus className="h-3.5 w-3.5" />
                      {m.channels.addDestination}
                    </Button>
                  </div>

                  {ch.destinations.length === 0 ? (
                    <div className="rounded-lg border border-dashed border-zinc-200 bg-white/60 px-3 py-3 text-xs text-zinc-500 dark:border-zinc-800 dark:bg-zinc-900/40 dark:text-zinc-400">
                      {m.channels.noDestinationsHint}
                    </div>
                  ) : (
                    <div className="space-y-2">
                      {ch.destinations.map((dest) => (
                        <div
                          key={dest.id}
                          className="flex flex-col justify-between gap-2.5 rounded-lg border border-zinc-200/80 bg-white px-3.5 py-2.5 sm:flex-row sm:items-center dark:border-zinc-800 dark:bg-zinc-900"
                        >
                          <div className="min-w-0 flex-1 space-y-1">
                            <div className="flex flex-wrap items-center gap-1.5">
                              <span className="text-xs font-semibold text-zinc-900 dark:text-zinc-100">
                                {dest.name}
                              </span>
                              {!dest.enabled && (
                                <Badge variant="default">
                                  {m.common.disabled}
                                </Badge>
                              )}
                            </div>

                            <div className="flex flex-wrap items-center gap-x-4 gap-y-0.5 text-[11px] text-zinc-500 dark:text-zinc-400">
                              {dest.config.kind === "telegram" && (
                                <span className="font-mono">
                                  <span className="font-sans text-zinc-400">
                                    {m.channels.card.destChat}{" "}
                                  </span>
                                  {dest.config.chat_id}
                                  {dest.config.message_thread_id !== null
                                    ? m.channels.card.topicSuffix(
                                        dest.config.message_thread_id,
                                      )
                                    : ""}
                                </span>
                              )}
                              {dest.config.kind === "smtp" && (
                                <>
                                  <span className="truncate font-mono">
                                    <span className="font-sans text-zinc-400">
                                      {m.channels.card.destTo}{" "}
                                    </span>
                                    {dest.config.to_addresses.join(", ")}
                                  </span>
                                  {dest.config.reply_to && (
                                    <span className="truncate font-mono">
                                      <span className="font-sans text-zinc-400">
                                        {m.channels.card.destReplyTo}{" "}
                                      </span>
                                      {dest.config.reply_to}
                                    </span>
                                  )}
                                </>
                              )}
                              {dest.config.kind === "notion" && (
                                <>
                                  <span className="truncate font-mono">
                                    <span className="font-sans text-zinc-400">
                                      {m.channels.card.destDataSource}{" "}
                                    </span>
                                    {dest.config.data_source_id}
                                  </span>
                                  <span className="font-mono">
                                    <span className="font-sans text-zinc-400">
                                      {m.channels.card.requiredKeys}{" "}
                                    </span>
                                    {dest.config.column_mapping.title} ·{" "}
                                    {dest.config.column_mapping.fc_id}
                                  </span>
                                </>
                              )}
                            </div>
                          </div>

                          <div className="flex shrink-0 flex-wrap items-center gap-1.5">
                            <Switch
                              checked={dest.enabled}
                              onCheckedChange={(enabled) =>
                                toggleDestMutation.mutate({
                                  id: dest.id,
                                  enabled,
                                })
                              }
                            />
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={() => testDestMutation.mutate(dest.id)}
                              loading={testingDestId === dest.id}
                            >
                              <Send className="h-3 w-3" />
                              {m.channels.card.testDestination}
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon"
                              onClick={() =>
                                setDestModalState({
                                  channel: ch,
                                  destination: dest,
                                })
                              }
                              title={m.channels.card.editDestinationTitle}
                              aria-label={m.channels.card.editDestinationTitle}
                            >
                              <Pencil className="h-3.5 w-3.5" />
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon"
                              onClick={() => setDeletingDest(dest)}
                              className="text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-950/40"
                              title={m.channels.card.deleteDestinationTitle}
                              aria-label={
                                m.channels.card.deleteDestinationTitle
                              }
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                            </Button>
                          </div>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              </Card>
            );
          })}
        </div>
      )}

      {/* Account Create / Edit Modal */}
      {accountModalOpen && (
        <ChannelAccountModal
          open={accountModalOpen}
          onClose={() => setAccountModalOpen(false)}
          channel={editingChannel}
        />
      )}

      {/* Destination Add / Edit Modal */}
      {destModalState && (
        <DestinationModal
          open={destModalState !== null}
          onClose={() => setDestModalState(null)}
          channel={destModalState.channel}
          destination={destModalState.destination}
        />
      )}

      {/* Delete Account Confirmation */}
      <ConfirmDialog
        open={deletingChannel !== null}
        onClose={() => setDeletingChannel(null)}
        onConfirm={() => {
          if (deletingChannel) deleteAccountMutation.mutate(deletingChannel.id);
        }}
        title={m.channels.deleteModalTitle}
        description={m.channels.deleteModalDesc(deletingChannel?.name ?? "")}
        confirmLabel={m.channels.deleteModalConfirm}
        loading={deleteAccountMutation.isPending}
      />

      {/* Delete Destination Confirmation */}
      <ConfirmDialog
        open={deletingDest !== null}
        onClose={() => setDeletingDest(null)}
        onConfirm={() => {
          if (deletingDest) deleteDestMutation.mutate(deletingDest.id);
        }}
        title={m.channels.deleteDestModalTitle}
        description={m.channels.deleteDestModalDesc(deletingDest?.name ?? "")}
        confirmLabel={m.channels.deleteDestModalConfirm}
        loading={deleteDestMutation.isPending}
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Shared Notion Destination Onboarding Sub-Component
// (Used by both Initial Destination on Account Create AND Add/Edit Destination)
// ---------------------------------------------------------------------------

interface NotionDestinationState {
  mode: "init_db" | "bind_existing";
  parentPageId: string;
  dbTitle: string;
  targetInput: string;
  dataSourceId: string;
  databaseId: string | null;
  columnMapping: NotionColumnMapping;
}

function NotionDestinationFields({
  state,
  onChange,
  secret,
  channelId,
  useProxy,
  onError,
}: {
  state: NotionDestinationState;
  onChange: (updater: (prev: NotionDestinationState) => NotionDestinationState) => void;
  /** Plaintext secret when creating a brand-new Notion account; omitted when reusing `channelId`. */
  secret?: string;
  /** Existing Notion account ID to reuse stored encrypted secret & proxy on the server. */
  channelId?: number;
  useProxy: boolean;
  onError: (msg: string | null) => void;
}) {
  const { m } = useI18n();
  const toast = useToast();

  const [schemaInspection, setSchemaInspection] =
    useState<NotionInspectSchemaResponse | null>(null);
  const [autoCreatedIds, setAutoCreatedIds] = useState<{
    databaseId: string;
    dataSourceId: string;
  } | null>(null);

  const hasStoredAccountSecret = channelId !== undefined;

  const initNotionDbMutation = useMutation({
    mutationFn: () =>
      api.channels.notionInitDb({
        secret: secret?.trim() || undefined,
        channel_id: channelId,
        parent_page_id: state.parentPageId.trim(),
        title: state.dbTitle.trim() || undefined,
        use_proxy: useProxy,
      }),
    onSuccess: (res) => {
      onChange((prev) => ({
        ...prev,
        dataSourceId: res.data_source_id,
        targetInput: res.data_source_id,
        databaseId: res.database_id,
        columnMapping: res.column_mapping,
      }));
      setAutoCreatedIds({
        databaseId: res.database_id,
        dataSourceId: res.data_source_id,
      });
      toast.success(
        m.channels.wizard.notion.toastCreatedTitle,
        m.channels.wizard.notion.toastCreatedDesc(res.data_source_id),
      );
    },
    onError: (err) => {
      onError(errorMessage(err));
    },
  });

  const inspectNotionMutation = useMutation({
    mutationFn: (patchMissing: boolean) =>
      api.channels.notionInspectSchema({
        secret: secret?.trim() || undefined,
        channel_id: channelId,
        data_source_or_db_id: state.targetInput.trim(),
        patch_missing: patchMissing,
        use_proxy: useProxy,
      }),
    onSuccess: (res, patchMissing) => {
      setSchemaInspection(res);
      onChange((prev) => ({
        ...prev,
        dataSourceId: res.data_source_id,
        databaseId: res.database_id,
        columnMapping: res.suggested_mapping,
      }));
      if (patchMissing && res.patched_columns.length > 0) {
        toast.success(
          m.channels.wizard.notion.toastPatchedTitle,
          m.channels.wizard.notion.toastPatchedDesc(
            res.patched_columns.join(", "),
          ),
        );
      } else {
        toast.info(
          m.channels.wizard.notion.toastInspectedTitle,
          res.ready
            ? m.channels.wizard.notion.toastInspectedReady
            : m.channels.wizard.notion.toastInspectedReview,
        );
      }
    },
    onError: (err) => {
      onError(errorMessage(err));
    },
  });

  const missingColumnsCount =
    schemaInspection?.columns.filter((c) => c.status === "missing").length ?? 0;

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="text-xs font-medium text-zinc-600 dark:text-zinc-300">
          {m.channels.wizard.notion.heading}
        </div>
        <div className="inline-flex rounded-lg border border-zinc-200 bg-zinc-50 p-0.5 dark:border-zinc-800 dark:bg-zinc-950">
          <button
            type="button"
            onClick={() => onChange((p) => ({ ...p, mode: "init_db" }))}
            className={`cursor-pointer rounded-md px-2.5 py-1 text-xs font-medium ${
              state.mode === "init_db"
                ? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
                : "text-zinc-600 dark:text-zinc-400"
            }`}
          >
            {m.channels.wizard.notion.modeInit}
          </button>
          <button
            type="button"
            onClick={() => onChange((p) => ({ ...p, mode: "bind_existing" }))}
            className={`cursor-pointer rounded-md px-2.5 py-1 text-xs font-medium ${
              state.mode === "bind_existing"
                ? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
                : "text-zinc-600 dark:text-zinc-400"
            }`}
          >
            {m.channels.wizard.notion.modeBind}
          </button>
        </div>
      </div>

      {hasStoredAccountSecret && (
        <p className="flex items-center gap-1.5 text-[11px] text-zinc-500 dark:text-zinc-400">
          <Lock className="h-3 w-3 shrink-0" />
          {m.channels.wizard.notion.usingStoredSecretHint}
        </p>
      )}

      {state.mode === "init_db" ? (
        <div className="space-y-3 rounded-lg border border-zinc-200/80 bg-zinc-50/70 p-3.5 dark:border-zinc-800 dark:bg-zinc-950/60">
          <p className="text-xs text-zinc-600 dark:text-zinc-400">
            {m.channels.wizard.notion.initIntro}
          </p>
          <div className="grid gap-3 sm:grid-cols-2">
            <div>
              <Label>{m.channels.wizard.notion.parentPageLabel}</Label>
              <Input
                value={state.parentPageId}
                onChange={(e) =>
                  onChange((p) => ({ ...p, parentPageId: e.target.value }))
                }
                placeholder="https://www.notion.so/..."
                className="mt-1 font-mono text-xs"
              />
            </div>
            <div>
              <Label>{m.channels.wizard.notion.dbTitleLabel}</Label>
              <Input
                value={state.dbTitle}
                onChange={(e) =>
                  onChange((p) => ({ ...p, dbTitle: e.target.value }))
                }
                placeholder="FeedCourier Inbox"
                className="mt-1"
              />
            </div>
          </div>
          <div className="flex flex-wrap items-center justify-between gap-3">
            <Button
              variant="secondary"
              size="sm"
              onClick={() => {
                onError(null);
                if (!hasStoredAccountSecret && !secret?.trim()) {
                  onError(m.channels.wizard.errors.notionInitInputs);
                  return;
                }
                if (!state.parentPageId.trim()) {
                  onError(
                    hasStoredAccountSecret
                      ? m.channels.wizard.errors.notionInitParentOnly
                      : m.channels.wizard.errors.notionInitInputs,
                  );
                  return;
                }
                initNotionDbMutation.mutate();
              }}
              loading={initNotionDbMutation.isPending}
            >
              <Sparkles className="h-3.5 w-3.5 text-emerald-600" />
              {m.channels.wizard.notion.autoCreateBtn}
            </Button>

            {state.dataSourceId && (
              <span className="truncate font-mono text-xs text-emerald-600 dark:text-emerald-400">
                data_source_id: {state.dataSourceId}
              </span>
            )}
          </div>

          {autoCreatedIds && (
            <div className="flex items-center gap-2 rounded-md border border-emerald-200 bg-emerald-50 px-3 py-2 text-xs text-emerald-800 dark:border-emerald-900/60 dark:bg-emerald-950/40 dark:text-emerald-300">
              <CheckCircle2 className="h-4 w-4 shrink-0" />
              <span>
                {m.channels.wizard.notion.autoCreatedBanner(
                  autoCreatedIds.databaseId,
                  autoCreatedIds.dataSourceId,
                )}
              </span>
            </div>
          )}
        </div>
      ) : (
        <div className="space-y-3 rounded-lg border border-zinc-200/80 bg-zinc-50/70 p-3.5 dark:border-zinc-800 dark:bg-zinc-950/60">
          <div>
            <Label>{m.channels.wizard.notion.existingTargetLabel}</Label>
            <div className="mt-1 flex flex-col gap-2 sm:flex-row">
              <Input
                value={state.targetInput}
                onChange={(e) => {
                  const val = e.target.value;
                  onChange((p) => ({
                    ...p,
                    targetInput: val,
                    dataSourceId: val,
                  }));
                }}
                placeholder="data_source_id / https://www.notion.so/..."
                className="flex-1 font-mono text-xs"
              />
              <Button
                variant="secondary"
                size="sm"
                onClick={() => {
                  onError(null);
                  if (!hasStoredAccountSecret && !secret?.trim()) {
                    onError(m.channels.wizard.errors.notionInspectSecret);
                    return;
                  }
                  if (!state.targetInput.trim()) {
                    onError(m.channels.wizard.errors.notionInspectTarget);
                    return;
                  }
                  inspectNotionMutation.mutate(false);
                }}
                loading={
                  inspectNotionMutation.isPending &&
                  !inspectNotionMutation.variables
                }
              >
                <RefreshCw className="h-3.5 w-3.5" />
                {m.channels.wizard.notion.inspectBtn}
              </Button>
            </div>
          </div>

          {/* Patch Missing Columns Banner */}
          {schemaInspection && missingColumnsCount > 0 && (
            <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-amber-200 bg-amber-50/80 px-3 py-2.5 text-xs text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-200">
              <div className="flex items-center gap-2">
                <AlertTriangle className="h-4 w-4 shrink-0 text-amber-600" />
                <span>
                  {m.channels.wizard.notion.missingBanner(missingColumnsCount)}
                </span>
              </div>
              <Button
                size="sm"
                variant="primary"
                onClick={() => inspectNotionMutation.mutate(true)}
                loading={
                  inspectNotionMutation.isPending &&
                  Boolean(inspectNotionMutation.variables)
                }
              >
                <Wrench className="h-3.5 w-3.5" />
                {m.channels.wizard.notion.patchMissingBtn}
              </Button>
            </div>
          )}

          {/* 7-Column Mapping Table */}
          <div className="overflow-x-auto rounded-lg border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-900">
            <table className="w-full text-left text-xs">
              <thead className="border-b border-zinc-100 bg-zinc-50 text-[11px] text-zinc-500 dark:border-zinc-800 dark:bg-zinc-950">
                <tr>
                  <th className="px-3 py-2 font-medium">
                    {m.channels.wizard.notion.colField}
                  </th>
                  <th className="px-3 py-2 font-medium">
                    {m.channels.wizard.notion.colExpectedType}
                  </th>
                  <th className="px-3 py-2 font-medium">
                    {m.channels.wizard.notion.colStatus}
                  </th>
                  <th className="px-3 py-2 font-medium">
                    {m.channels.wizard.notion.colMappedProp}
                  </th>
                </tr>
              </thead>
              <tbody className="divide-y divide-zinc-100 dark:divide-zinc-800">
                {NOTION_STANDARD_COLUMNS.map((col) => {
                  const inspected = schemaInspection?.columns.find(
                    (c) => c.field === col.key,
                  );
                  const compatibleProps =
                    schemaInspection?.available_properties.filter(
                      (p) => p.property_type === col.expectedType,
                    ) ?? [];
                  const currentVal = state.columnMapping[col.key] ?? "";

                  return (
                    <tr key={col.key}>
                      <td className="px-3 py-2">
                        <div className="flex items-center gap-1.5 font-medium">
                          <span className="font-mono">{col.standardName}</span>
                          {col.required && (
                            <Badge variant="danger">{m.common.required}</Badge>
                          )}
                        </div>
                        <div className="text-[10px] text-zinc-400">
                          {m.channels.wizard.notion.purposes[col.key]}
                        </div>
                      </td>
                      <td className="px-3 py-2 font-mono text-[11px] text-zinc-500">
                        {col.expectedType}
                        {inspected?.actual_type &&
                          inspected.actual_type !== col.expectedType && (
                            <span className="ml-1 text-rose-500">
                              {m.channels.wizard.notion.foundType(
                                inspected.actual_type,
                              )}
                            </span>
                          )}
                      </td>
                      <td className="px-3 py-2">
                        {inspected ? (
                          inspected.status === "matched" ? (
                            <Badge variant="success">
                              {m.channels.wizard.notion.statusMatched}
                            </Badge>
                          ) : inspected.status === "mismatched" ? (
                            <Badge variant="warning">
                              {m.channels.wizard.notion.statusMismatch}
                            </Badge>
                          ) : (
                            <Badge variant="danger">
                              {m.channels.wizard.notion.statusMissing}
                            </Badge>
                          )
                        ) : (
                          <Badge variant="outline">
                            {m.channels.wizard.notion.statusUnverified}
                          </Badge>
                        )}
                      </td>
                      <td className="px-3 py-2">
                        {schemaInspection ? (
                          <Select
                            value={currentVal}
                            onChange={(e) => {
                              const next = e.target.value;
                              onChange((p) => ({
                                ...p,
                                columnMapping: {
                                  ...p.columnMapping,
                                  [col.key]: col.required ? next : next || null,
                                },
                              }));
                            }}
                            className="h-8 font-mono text-xs"
                          >
                            {col.required ? (
                              <option value="">
                                {m.channels.wizard.notion.selectRequiredOption(
                                  col.expectedType,
                                )}
                              </option>
                            ) : (
                              <option value="">
                                {m.channels.wizard.notion.skipFieldOption}
                              </option>
                            )}
                            {compatibleProps.map((p) => (
                              <option key={p.name} value={p.name}>
                                {p.name} ({p.property_type})
                              </option>
                            ))}
                          </Select>
                        ) : (
                          <Input
                            value={currentVal}
                            onChange={(e) => {
                              const next = e.target.value;
                              onChange((p) => ({
                                ...p,
                                columnMapping: {
                                  ...p.columnMapping,
                                  [col.key]: col.required ? next : next || null,
                                },
                              }));
                            }}
                            placeholder={
                              col.required
                                ? col.standardName
                                : m.channels.wizard.notion
                                    .optionalBlankPlaceholder
                            }
                            className="h-8 font-mono text-xs"
                          />
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Account Create / Edit Modal
// ---------------------------------------------------------------------------

function ChannelAccountModal({
  open,
  onClose,
  channel,
}: {
  open: boolean;
  onClose: () => void;
  channel: ChannelDto | null;
}) {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const isEdit = channel !== null;

  const [kind, setKind] = useState<ChannelKind>(channel?.kind ?? "telegram");
  const [name, setName] = useState(channel?.name ?? "");
  const [minIntervalSecs, setMinIntervalSecs] = useState<number>(
    channel?.min_interval_secs ??
      kindDefaults(channel?.kind ?? "telegram").defaultInterval,
  );
  const [jitterMaxSecs, setJitterMaxSecs] = useState<number>(
    channel?.jitter_max_secs ??
      kindDefaults(channel?.kind ?? "telegram").defaultJitter,
  );
  const [hourlyQuota, setHourlyQuota] = useState<string>(
    channel?.hourly_quota !== null && channel?.hourly_quota !== undefined
      ? String(channel.hourly_quota)
      : "",
  );
  const [dailyQuota, setDailyQuota] = useState<string>(
    channel?.daily_quota !== null && channel?.daily_quota !== undefined
      ? String(channel.daily_quota)
      : "",
  );
  const [useProxy, setUseProxy] = useState<boolean>(
    channel?.use_proxy ?? false,
  );
  const [enabled, setEnabled] = useState<boolean>(channel?.enabled ?? true);

  // --- Account credentials ---
  const initialSmtp =
    channel?.config.kind === "smtp" ? channel.config : undefined;
  const [smtpHost, setSmtpHost] = useState(initialSmtp?.host ?? "");
  const [smtpPort, setSmtpPort] = useState<number>(initialSmtp?.port ?? 587);
  const [smtpSecurity, setSmtpSecurity] = useState<SmtpSecurity>(
    initialSmtp?.security ?? "start_tls",
  );
  const [smtpUsername, setSmtpUsername] = useState(
    initialSmtp?.username ?? "",
  );
  const [smtpPassword, setSmtpPassword] = useState("");
  const [smtpFromAddress, setSmtpFromAddress] = useState(
    initialSmtp?.from_address ?? "",
  );

  const initialTg =
    channel?.config.kind === "telegram" ? channel.config : undefined;
  const [tgBotToken, setTgBotToken] = useState("");
  const [tgApiBaseUrl, setTgApiBaseUrl] = useState(
    initialTg?.api_base_url ?? "",
  );

  const initialNotion =
    channel?.config.kind === "notion" ? channel.config : undefined;
  const [notionSecret, setNotionSecret] = useState("");

  // --- Optional Initial Destination (Create mode only) ---
  const [includeInitialDest, setIncludeInitialDest] = useState<boolean>(!isEdit);
  const [initDestName, setInitDestName] = useState("");
  const [initSmtpTo, setInitSmtpTo] = useState<string[]>([]);
  const [initSmtpReplyTo, setInitSmtpReplyTo] = useState("");
  const [initTgChatId, setInitTgChatId] = useState("");
  const [initTgThreadId, setInitTgThreadId] = useState("");
  const [initNotionDest, setInitNotionDest] = useState<NotionDestinationState>({
    mode: "init_db",
    parentPageId: "",
    dbTitle: "FeedCourier Inbox",
    targetInput: "",
    dataSourceId: "",
    databaseId: null,
    columnMapping: { ...DEFAULT_NOTION_COLUMN_MAPPING },
  });

  const [formError, setFormError] = useState<string | null>(null);

  const handleKindSwitch = (nextKind: ChannelKind) => {
    setKind(nextKind);
    const defaults = kindDefaults(nextKind);
    setMinIntervalSecs(defaults.defaultInterval);
    setJitterMaxSecs(defaults.defaultJitter);
    setFormError(null);
  };

  const saveMutation = useMutation({
    mutationFn: async () => {
      const parsedHourly = hourlyQuota.trim()
        ? Number(hourlyQuota.trim())
        : null;
      const parsedDaily = dailyQuota.trim() ? Number(dailyQuota.trim()) : null;

      if (isEdit && channel) {
        let configPatch: ChannelAccountPatch;
        if (kind === "smtp") {
          configPatch = {
            kind: "smtp",
            host: smtpHost.trim(),
            port: smtpPort,
            security: smtpSecurity,
            username: smtpUsername.trim() || null,
            password: smtpPassword.trim() ? smtpPassword : null,
            from_address: smtpFromAddress.trim(),
          };
        } else if (kind === "telegram") {
          configPatch = {
            kind: "telegram",
            bot_token: tgBotToken.trim() ? tgBotToken.trim() : null,
            api_base_url: tgApiBaseUrl.trim() || null,
          };
        } else {
          configPatch = {
            kind: "notion",
            integration_secret: notionSecret.trim()
              ? notionSecret.trim()
              : null,
          };
        }

        const payload: UpdateChannelRequest = {
          name: name.trim(),
          config: configPatch,
          min_interval_secs: minIntervalSecs,
          jitter_max_secs: jitterMaxSecs,
          hourly_quota: parsedHourly,
          daily_quota: parsedDaily,
          use_proxy: useProxy,
          enabled,
        };
        return api.channels.update(channel.id, payload);
      }

      let configCreate: ChannelAccountCreate;
      if (kind === "smtp") {
        configCreate = {
          kind: "smtp",
          host: smtpHost.trim(),
          port: smtpPort,
          security: smtpSecurity,
          username: smtpUsername.trim() || null,
          password: smtpPassword || null,
          from_address: smtpFromAddress.trim(),
        };
      } else if (kind === "telegram") {
        configCreate = {
          kind: "telegram",
          bot_token: tgBotToken.trim(),
          api_base_url: tgApiBaseUrl.trim() || null,
        };
      } else {
        configCreate = {
          kind: "notion",
          integration_secret: notionSecret.trim(),
        };
      }

      let initialDestination: CreateDestinationRequest | undefined;
      if (includeInitialDest) {
        let destConfig: DestinationConfigCreate;
        if (kind === "smtp") {
          destConfig = {
            kind: "smtp",
            to_addresses: initSmtpTo,
            reply_to: initSmtpReplyTo.trim() || null,
          };
        } else if (kind === "telegram") {
          destConfig = {
            kind: "telegram",
            chat_id: initTgChatId.trim(),
            message_thread_id: initTgThreadId.trim()
              ? Number(initTgThreadId.trim())
              : null,
          };
        } else {
          destConfig = {
            kind: "notion",
            data_source_id:
              initNotionDest.dataSourceId.trim() ||
              initNotionDest.targetInput.trim(),
            database_id: initNotionDest.databaseId,
            parent_page_id: initNotionDest.parentPageId.trim() || null,
            column_mapping: initNotionDest.columnMapping,
          };
        }

        initialDestination = {
          name: initDestName.trim() || undefined,
          config: destConfig,
          enabled: true,
        };
      }

      const payload: CreateChannelRequest = {
        name: name.trim() || undefined,
        kind,
        config: configCreate,
        initial_destination: initialDestination,
        min_interval_secs: minIntervalSecs,
        jitter_max_secs: jitterMaxSecs,
        hourly_quota: parsedHourly,
        daily_quota: parsedDaily,
        use_proxy: useProxy,
        enabled,
      };
      return api.channels.create(payload);
    },
    onSuccess: () => {
      toast.success(
        isEdit ? m.channels.toastUpdated : m.channels.toastCreated,
      );
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

    if (isEdit && !name.trim()) {
      setFormError(m.channels.wizard.errors.emptyName);
      return;
    }

    if (kind === "smtp") {
      if (!smtpHost.trim() || !smtpFromAddress.trim()) {
        setFormError(m.channels.wizard.errors.smtpAccountRequired);
        return;
      }
      if (smtpSecurity !== "plain" && !isEdit && !smtpPassword) {
        setFormError(m.channels.wizard.errors.smtpPassword);
        return;
      }
    } else if (kind === "telegram") {
      if (!isEdit && !tgBotToken.trim()) {
        setFormError(m.channels.wizard.errors.tgToken);
        return;
      }
    } else if (kind === "notion") {
      if (!isEdit && !notionSecret.trim()) {
        setFormError(m.channels.wizard.errors.notionSecret);
        return;
      }
    }

    if (!isEdit && includeInitialDest) {
      if (kind === "smtp") {
        if (initSmtpTo.length === 0) {
          setFormError(m.channels.wizard.errors.smtpToRequired);
          return;
        }
      } else if (kind === "telegram") {
        if (!initTgChatId.trim()) {
          setFormError(m.channels.wizard.errors.tgChatId);
          return;
        }
      } else if (kind === "notion") {
        const effectiveDsId =
          initNotionDest.dataSourceId.trim() ||
          initNotionDest.targetInput.trim();
        if (!effectiveDsId) {
          setFormError(m.channels.wizard.errors.notionDataSource);
          return;
        }
        if (
          !initNotionDest.columnMapping.title.trim() ||
          !initNotionDest.columnMapping.fc_id.trim()
        ) {
          setFormError(m.channels.wizard.errors.notionRequiredCols);
          return;
        }
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
          ? m.channels.wizard.editTitle(channel?.name ?? "")
          : m.channels.wizard.newTitle
      }
      description={m.channels.wizard.desc}
      size="xl"
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            {m.common.cancel}
          </Button>
          <Button onClick={handleSubmit} loading={saveMutation.isPending}>
            {isEdit
              ? m.channels.wizard.saveChannel
              : m.channels.wizard.createChannel}
          </Button>
        </>
      }
    >
      <form onSubmit={handleSubmit} className="space-y-5">
        {/* Kind Selector (Create only) */}
        {!isEdit && (
          <div className="grid grid-cols-3 gap-2.5">
            {(["telegram", "smtp", "notion"] as const).map((k) => {
              const meta = kindMeta(k, m);
              const Icon = meta.icon;
              const selected = kind === k;
              return (
                <button
                  key={k}
                  type="button"
                  onClick={() => handleKindSwitch(k)}
                  className={`flex cursor-pointer items-center gap-2.5 rounded-lg border p-3 text-left transition-colors ${
                    selected
                      ? "border-zinc-900 bg-zinc-900 text-white dark:border-zinc-100 dark:bg-zinc-100 dark:text-zinc-900"
                      : "border-zinc-200 bg-white text-zinc-700 hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-950 dark:text-zinc-300"
                  }`}
                >
                  <Icon className="h-4 w-4 shrink-0" />
                  <span className="text-xs font-semibold">{meta.label}</span>
                </button>
              );
            })}
          </div>
        )}

        {/* Account Name */}
        <div>
          <Label htmlFor="ch-name">
            {isEdit
              ? m.channels.wizard.nameLabel
              : m.channels.wizard.nameOptionalLabel}
          </Label>
          <Input
            id="ch-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={
              kind === "smtp"
                ? m.channels.wizard.namePlaceholderSmtp
                : kind === "telegram"
                  ? m.channels.wizard.namePlaceholderTg
                  : m.channels.wizard.namePlaceholderNotion
            }
            className="mt-1.5"
          />
        </div>

        {/* Account Credentials by Kind */}
        {kind === "smtp" && (
          <div className="space-y-3.5 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
            <h3 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase">
              {m.channels.wizard.smtp.heading}
            </h3>
            <div className="grid gap-3 sm:grid-cols-3">
              <div>
                <Label>{m.channels.wizard.smtp.hostLabel}</Label>
                <Input
                  value={smtpHost}
                  onChange={(e) => setSmtpHost(e.target.value)}
                  placeholder="smtp.fastmail.com"
                  className="mt-1 font-mono text-xs"
                />
              </div>
              <div>
                <Label>{m.channels.wizard.smtp.portLabel}</Label>
                <Input
                  type="number"
                  value={smtpPort}
                  onChange={(e) => setSmtpPort(Number(e.target.value))}
                  className="mt-1 font-mono text-xs"
                />
              </div>
              <div>
                <Label>{m.channels.wizard.smtp.securityLabel}</Label>
                <Select
                  value={smtpSecurity}
                  onChange={(e) => {
                    const sec = e.target.value as SmtpSecurity;
                    setSmtpSecurity(sec);
                    if (sec === "tls" && smtpPort === 587) setSmtpPort(465);
                    if (sec === "start_tls" && smtpPort === 465)
                      setSmtpPort(587);
                  }}
                  className="mt-1"
                >
                  <option value="start_tls">
                    {m.channels.wizard.smtp.secStartTls}
                  </option>
                  <option value="tls">{m.channels.wizard.smtp.secTls}</option>
                  <option value="plain">
                    {m.channels.wizard.smtp.secPlain}
                  </option>
                </Select>
              </div>
            </div>

            <div className="grid gap-3 sm:grid-cols-2">
              <div>
                <Label>{m.channels.wizard.smtp.usernameLabel}</Label>
                <Input
                  value={smtpUsername}
                  onChange={(e) => setSmtpUsername(e.target.value)}
                  placeholder="courier@example.com"
                  className="mt-1 font-mono text-xs"
                />
              </div>
              <div>
                <Label>{m.channels.wizard.smtp.passwordLabel}</Label>
                <Input
                  type="password"
                  value={smtpPassword}
                  onChange={(e) => setSmtpPassword(e.target.value)}
                  placeholder={
                    initialSmtp?.has_password
                      ? m.channels.wizard.smtp.passwordStoredPlaceholder
                      : "••••••••••••"
                  }
                  className="mt-1 font-mono text-xs"
                />
              </div>
            </div>

            <div>
              <Label>{m.channels.wizard.smtp.fromLabel}</Label>
              <Input
                value={smtpFromAddress}
                onChange={(e) => setSmtpFromAddress(e.target.value)}
                placeholder="rss@yourdomain.com"
                className="mt-1 font-mono text-xs"
              />
              <FieldHint>{m.channels.wizard.smtp.fromHint}</FieldHint>
            </div>
          </div>
        )}

        {kind === "telegram" && (
          <div className="space-y-3.5 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
            <h3 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase">
              {m.channels.wizard.telegram.heading}
            </h3>
            <div>
              <Label>{m.channels.wizard.telegram.tokenLabel}</Label>
              <Input
                type="password"
                value={tgBotToken}
                onChange={(e) => setTgBotToken(e.target.value)}
                placeholder={
                  initialTg
                    ? m.channels.wizard.telegram.tokenStoredPlaceholder(
                        initialTg.bot_token_hint,
                      )
                    : "123456789:ABCdefGHIjklMNOpqrSTUvwxYZ"
                }
                className="mt-1 font-mono text-xs"
              />
            </div>

            <div>
              <Label>{m.channels.wizard.telegram.apiBaseLabel}</Label>
              <Input
                value={tgApiBaseUrl}
                onChange={(e) => setTgApiBaseUrl(e.target.value)}
                placeholder={m.channels.wizard.telegram.apiBasePlaceholder}
                className="mt-1 font-mono text-xs"
              />
              <FieldHint>{m.channels.wizard.telegram.apiBaseHint}</FieldHint>
            </div>
          </div>
        )}

        {kind === "notion" && (
          <div className="space-y-3 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
            <h3 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase">
              {m.channels.wizard.notion.accountHeading}
            </h3>
            <div>
              <Label>{m.channels.wizard.notion.secretLabel}</Label>
              <Input
                type="password"
                value={notionSecret}
                onChange={(e) => setNotionSecret(e.target.value)}
                placeholder={
                  initialNotion
                    ? m.channels.wizard.notion.secretStoredPlaceholder(
                        initialNotion.secret_hint,
                      )
                    : "ntn_••••••••••••"
                }
                className="mt-1 font-mono text-xs"
              />
            </div>
          </div>
        )}

        {/* Shared Pacing, Jitter, Quotas & Proxy */}
        <div className="space-y-3.5 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
          <div>
            <h3 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase">
              {m.channels.wizard.pacing.heading}
            </h3>
            <p className="mt-0.5 text-[11px] text-zinc-500 dark:text-zinc-400">
              {m.channels.wizard.pacing.sharedNote}
            </p>
          </div>
          <div className="grid gap-3 sm:grid-cols-4">
            <div>
              <Label>{m.channels.wizard.pacing.minIntervalLabel}</Label>
              <Input
                type="number"
                min={0}
                value={minIntervalSecs}
                onChange={(e) => setMinIntervalSecs(Number(e.target.value))}
                className="mt-1 font-mono text-xs"
              />
            </div>
            <div>
              <Label>{m.channels.wizard.pacing.maxJitterLabel}</Label>
              <Input
                type="number"
                min={0}
                value={jitterMaxSecs}
                onChange={(e) => setJitterMaxSecs(Number(e.target.value))}
                className="mt-1 font-mono text-xs"
              />
            </div>
            <div>
              <Label>{m.channels.wizard.pacing.hourlyQuotaLabel}</Label>
              <Input
                type="number"
                min={1}
                value={hourlyQuota}
                onChange={(e) => setHourlyQuota(e.target.value)}
                placeholder={m.common.unlimited}
                className="mt-1 font-mono text-xs"
              />
            </div>
            <div>
              <Label>{m.channels.wizard.pacing.dailyQuotaLabel}</Label>
              <Input
                type="number"
                min={1}
                value={dailyQuota}
                onChange={(e) => setDailyQuota(e.target.value)}
                placeholder={m.common.unlimited}
                className="mt-1 font-mono text-xs"
              />
            </div>
          </div>

          <div className="grid gap-3 pt-1 sm:grid-cols-2">
            <Switch
              id="ch-proxy"
              checked={useProxy}
              onCheckedChange={setUseProxy}
              label={m.channels.wizard.pacing.proxyLabel}
              description={m.channels.wizard.pacing.proxyDesc}
            />
            <Switch
              id="ch-enabled"
              checked={enabled}
              onCheckedChange={setEnabled}
              label={m.channels.wizard.pacing.enabledLabel}
              description={m.channels.wizard.pacing.enabledDesc}
            />
          </div>
        </div>

        {/* Optional Initial Destination (Create mode only) */}
        {!isEdit && (
          <div className="space-y-3.5 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
            <Switch
              id="ch-initial-dest"
              checked={includeInitialDest}
              onCheckedChange={setIncludeInitialDest}
              label={m.channels.wizard.initialDestSwitchLabel}
              description={m.channels.wizard.initialDestSwitchDesc}
            />

            {includeInitialDest && (
              <div className="space-y-3.5 border-t border-zinc-100 pt-3.5 dark:border-zinc-800">
                <div>
                  <Label htmlFor="init-dest-name">
                    {m.channels.wizard.destModal.nameOptionalLabel}
                  </Label>
                  <Input
                    id="init-dest-name"
                    value={initDestName}
                    onChange={(e) => setInitDestName(e.target.value)}
                    placeholder={
                      kind === "smtp"
                        ? m.channels.wizard.destModal.namePlaceholderSmtp
                        : kind === "telegram"
                          ? m.channels.wizard.destModal.namePlaceholderTg
                          : m.channels.wizard.destModal.namePlaceholderNotion
                    }
                    className="mt-1"
                  />
                </div>

                {kind === "smtp" && (
                  <div className="space-y-3">
                    <div>
                      <Label>{m.channels.wizard.smtp.toLabel}</Label>
                      <div className="mt-1">
                        <TagInput
                          values={initSmtpTo}
                          onChange={setInitSmtpTo}
                          mono
                          placeholder="me@example.com"
                        />
                      </div>
                    </div>
                    <div>
                      <Label>{m.channels.wizard.smtp.replyToLabel}</Label>
                      <Input
                        value={initSmtpReplyTo}
                        onChange={(e) => setInitSmtpReplyTo(e.target.value)}
                        placeholder={m.channels.wizard.smtp.replyToPlaceholder}
                        className="mt-1 font-mono text-xs"
                      />
                    </div>
                  </div>
                )}

                {kind === "telegram" && (
                  <div className="grid gap-3 sm:grid-cols-2">
                    <div>
                      <Label>{m.channels.wizard.telegram.chatIdLabel}</Label>
                      <Input
                        value={initTgChatId}
                        onChange={(e) => setInitTgChatId(e.target.value)}
                        placeholder="-1001234567890 or @mychannel"
                        className="mt-1 font-mono text-xs"
                      />
                    </div>
                    <div>
                      <Label>{m.channels.wizard.telegram.threadIdLabel}</Label>
                      <Input
                        type="number"
                        value={initTgThreadId}
                        onChange={(e) => setInitTgThreadId(e.target.value)}
                        placeholder={
                          m.channels.wizard.telegram.threadIdPlaceholder
                        }
                        className="mt-1 font-mono text-xs"
                      />
                    </div>
                  </div>
                )}

                {kind === "notion" && (
                  <NotionDestinationFields
                    state={initNotionDest}
                    onChange={setInitNotionDest}
                    secret={notionSecret}
                    useProxy={useProxy}
                    onError={setFormError}
                  />
                )}
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
// Add / Edit Destination Modal
// ---------------------------------------------------------------------------

function DestinationModal({
  open,
  onClose,
  channel,
  destination,
}: {
  open: boolean;
  onClose: () => void;
  channel: ChannelDto;
  destination: ChannelDestinationDto | null;
}) {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const isEdit = destination !== null;
  const meta = kindMeta(channel.kind, m);

  const [name, setName] = useState(destination?.name ?? "");
  const [enabled, setEnabled] = useState<boolean>(destination?.enabled ?? true);

  const initialSmtp =
    destination?.config.kind === "smtp" ? destination.config : undefined;
  const [smtpTo, setSmtpTo] = useState<string[]>(
    initialSmtp?.to_addresses ?? [],
  );
  const [smtpReplyTo, setSmtpReplyTo] = useState(initialSmtp?.reply_to ?? "");

  const initialTg =
    destination?.config.kind === "telegram" ? destination.config : undefined;
  const [tgChatId, setTgChatId] = useState(initialTg?.chat_id ?? "");
  const [tgThreadId, setTgThreadId] = useState<string>(
    initialTg?.message_thread_id !== null &&
      initialTg?.message_thread_id !== undefined
      ? String(initialTg.message_thread_id)
      : "",
  );

  const initialNotion =
    destination?.config.kind === "notion" ? destination.config : undefined;
  const [notionDest, setNotionDest] = useState<NotionDestinationState>({
    mode: isEdit ? "bind_existing" : "init_db",
    parentPageId: initialNotion?.parent_page_id ?? "",
    dbTitle: "FeedCourier Inbox",
    targetInput: initialNotion?.data_source_id ?? "",
    dataSourceId: initialNotion?.data_source_id ?? "",
    databaseId: initialNotion?.database_id ?? null,
    columnMapping: initialNotion?.column_mapping ?? {
      ...DEFAULT_NOTION_COLUMN_MAPPING,
    },
  });

  const [formError, setFormError] = useState<string | null>(null);

  const saveMutation = useMutation({
    mutationFn: async () => {
      if (isEdit && destination) {
        let patchConfig: DestinationPatch;
        if (channel.kind === "smtp") {
          patchConfig = {
            kind: "smtp",
            to_addresses: smtpTo,
            reply_to: smtpReplyTo.trim() || null,
          };
        } else if (channel.kind === "telegram") {
          patchConfig = {
            kind: "telegram",
            chat_id: tgChatId.trim(),
            message_thread_id: tgThreadId.trim()
              ? Number(tgThreadId.trim())
              : null,
          };
        } else {
          patchConfig = {
            kind: "notion",
            data_source_id:
              notionDest.dataSourceId.trim() || notionDest.targetInput.trim(),
            database_id: notionDest.databaseId,
            parent_page_id: notionDest.parentPageId.trim() || null,
            column_mapping: notionDest.columnMapping,
          };
        }

        const payload: UpdateDestinationRequest = {
          name: name.trim(),
          config: patchConfig,
          enabled,
        };
        return api.destinations.update(destination.id, payload);
      }

      let createConfig: DestinationConfigCreate;
      if (channel.kind === "smtp") {
        createConfig = {
          kind: "smtp",
          to_addresses: smtpTo,
          reply_to: smtpReplyTo.trim() || null,
        };
      } else if (channel.kind === "telegram") {
        createConfig = {
          kind: "telegram",
          chat_id: tgChatId.trim(),
          message_thread_id: tgThreadId.trim()
            ? Number(tgThreadId.trim())
            : null,
        };
      } else {
        createConfig = {
          kind: "notion",
          data_source_id:
            notionDest.dataSourceId.trim() || notionDest.targetInput.trim(),
          database_id: notionDest.databaseId,
          parent_page_id: notionDest.parentPageId.trim() || null,
          column_mapping: notionDest.columnMapping,
        };
      }

      const payload: CreateDestinationRequest = {
        name: name.trim() || undefined,
        config: createConfig,
        enabled,
      };
      return api.channels.createDestination(channel.id, payload);
    },
    onSuccess: () => {
      toast.success(
        isEdit ? m.channels.toastDestUpdated : m.channels.toastDestCreated,
      );
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

    if (isEdit && !name.trim()) {
      setFormError(m.channels.wizard.errors.emptyDestName);
      return;
    }

    if (channel.kind === "smtp") {
      if (smtpTo.length === 0) {
        setFormError(m.channels.wizard.errors.smtpToRequired);
        return;
      }
    } else if (channel.kind === "telegram") {
      if (!tgChatId.trim()) {
        setFormError(m.channels.wizard.errors.tgChatId);
        return;
      }
    } else if (channel.kind === "notion") {
      const effectiveDsId =
        notionDest.dataSourceId.trim() || notionDest.targetInput.trim();
      if (!effectiveDsId) {
        setFormError(m.channels.wizard.errors.notionDataSource);
        return;
      }
      if (
        !notionDest.columnMapping.title.trim() ||
        !notionDest.columnMapping.fc_id.trim()
      ) {
        setFormError(m.channels.wizard.errors.notionRequiredCols);
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
          ? m.channels.wizard.destModal.editTitle(destination?.name ?? "")
          : m.channels.wizard.destModal.newTitle(channel.name)
      }
      description={m.channels.wizard.destModal.desc(channel.name, meta.label)}
      size={channel.kind === "notion" ? "xl" : "lg"}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            {m.common.cancel}
          </Button>
          <Button onClick={handleSubmit} loading={saveMutation.isPending}>
            {isEdit
              ? m.channels.wizard.destModal.saveBtn
              : m.channels.wizard.destModal.createBtn}
          </Button>
        </>
      }
    >
      <form onSubmit={handleSubmit} className="space-y-4">
        <div>
          <Label htmlFor="dest-name">
            {isEdit
              ? m.channels.wizard.destModal.nameLabel
              : m.channels.wizard.destModal.nameOptionalLabel}
          </Label>
          <Input
            id="dest-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={
              channel.kind === "smtp"
                ? m.channels.wizard.destModal.namePlaceholderSmtp
                : channel.kind === "telegram"
                  ? m.channels.wizard.destModal.namePlaceholderTg
                  : m.channels.wizard.destModal.namePlaceholderNotion
            }
            className="mt-1.5"
          />
        </div>

        {channel.kind === "smtp" && (
          <div className="space-y-3 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
            <h3 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase">
              {m.channels.wizard.smtp.destHeading}
            </h3>
            <div>
              <Label>{m.channels.wizard.smtp.toLabel}</Label>
              <div className="mt-1">
                <TagInput
                  values={smtpTo}
                  onChange={setSmtpTo}
                  mono
                  placeholder="me@example.com"
                />
              </div>
            </div>
            <div>
              <Label>{m.channels.wizard.smtp.replyToLabel}</Label>
              <Input
                value={smtpReplyTo}
                onChange={(e) => setSmtpReplyTo(e.target.value)}
                placeholder={m.channels.wizard.smtp.replyToPlaceholder}
                className="mt-1 font-mono text-xs"
              />
            </div>
          </div>
        )}

        {channel.kind === "telegram" && (
          <div className="space-y-3 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
            <h3 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase">
              {m.channels.wizard.telegram.destHeading}
            </h3>
            <div className="grid gap-3 sm:grid-cols-2">
              <div>
                <Label>{m.channels.wizard.telegram.chatIdLabel}</Label>
                <Input
                  value={tgChatId}
                  onChange={(e) => setTgChatId(e.target.value)}
                  placeholder="-1001234567890 or @mychannel"
                  className="mt-1 font-mono text-xs"
                />
              </div>
              <div>
                <Label>{m.channels.wizard.telegram.threadIdLabel}</Label>
                <Input
                  type="number"
                  value={tgThreadId}
                  onChange={(e) => setTgThreadId(e.target.value)}
                  placeholder={m.channels.wizard.telegram.threadIdPlaceholder}
                  className="mt-1 font-mono text-xs"
                />
              </div>
            </div>
          </div>
        )}

        {channel.kind === "notion" && (
          <div className="rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
            <NotionDestinationFields
              state={notionDest}
              onChange={setNotionDest}
              channelId={channel.id}
              useProxy={channel.use_proxy}
              onError={setFormError}
            />
          </div>
        )}

        <div className="rounded-lg border border-zinc-200/80 bg-zinc-50/60 p-3.5 dark:border-zinc-800 dark:bg-zinc-950/60">
          <Switch
            id="dest-enabled"
            checked={enabled}
            onCheckedChange={setEnabled}
            label={m.channels.wizard.destModal.enabledLabel}
            description={m.channels.wizard.destModal.enabledDesc}
          />
        </div>

        <FieldError message={formError} />
      </form>
    </Modal>
  );
}
