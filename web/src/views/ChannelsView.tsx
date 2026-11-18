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
  Sparkles,
  Trash2,
  Wrench,
} from "lucide-react";
import { type FormEvent, useState } from "react";
import { api } from "@/api/client";
import {
  type ChannelDto,
  type ChannelKind,
  type ChannelTargetCreate,
  type ChannelTargetPatch,
  type CreateChannelRequest,
  DEFAULT_NOTION_COLUMN_MAPPING,
  type NotionColumnMapping,
  type NotionInspectSchemaResponse,
  type NotionStandardFieldKey,
  type SmtpSecurity,
  type UpdateChannelRequest,
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

  const [wizardOpen, setWizardOpen] = useState(false);
  const [editingChannel, setEditingChannel] = useState<ChannelDto | null>(null);
  const [deletingChannel, setDeletingChannel] = useState<ChannelDto | null>(
    null,
  );
  const [testingId, setTestingId] = useState<number | null>(null);

  const channelsQuery = useQuery({
    queryKey: ["channels"],
    queryFn: () => api.channels.list(),
  });

  const testMutation = useMutation({
    mutationFn: (id: number) => {
      setTestingId(id);
      return api.channels.test(id, { locale });
    },
    onSuccess: (res) => {
      if (res.ok) {
        toast.success(m.channels.toastTestOk, res.message);
      } else {
        toast.error(m.channels.toastTestIssue, res.message);
      }
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
    },
    onError: (err) => {
      toast.error(m.channels.toastTestFailed, errorMessage(err));
    },
    onSettled: () => setTestingId(null),
  });

  const toggleEnabledMutation = useMutation({
    mutationFn: ({ id, enabled }: { id: number; enabled: boolean }) =>
      api.channels.update(id, { enabled }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
    },
    onError: (err) => {
      toast.error(m.channels.toastToggleFailed, errorMessage(err));
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (id: number) => api.channels.remove(id),
    onSuccess: () => {
      toast.success(m.channels.toastDeleted);
      setDeletingChannel(null);
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
      void queryClient.invalidateQueries({ queryKey: ["routes"] });
    },
    onError: (err) => {
      toast.error(m.channels.toastDeleteFailed, errorMessage(err));
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
            setWizardOpen(true);
          }}
        >
          <Plus className="h-3.5 w-3.5" />
          {m.channels.newChannel}
        </Button>
      </div>

      {/* Channels List */}
      {channelsQuery.isLoading ? (
        <Card className="p-8 text-center text-xs text-zinc-500">
          {m.channels.loading}
        </Card>
      ) : (channelsQuery.data?.length ?? 0) === 0 ? (
        <EmptyState
          icon={Radio}
          title={m.channels.emptyTitle}
          description={m.channels.emptyDesc}
          action={
            <Button
              size="sm"
              onClick={() => {
                setEditingChannel(null);
                setWizardOpen(true);
              }}
            >
              <Plus className="h-3.5 w-3.5" />
              {m.channels.configureFirst}
            </Button>
          }
        />
      ) : (
        <div className="grid gap-3.5 lg:grid-cols-2">
          {(channelsQuery.data ?? []).map((ch) => {
            const meta = kindMeta(ch.kind, m);
            const Icon = meta.icon;
            const hasCooldown =
              ch.cooldown_until &&
              new Date(ch.cooldown_until).getTime() > Date.now();

            return (
              <Card key={ch.id} className="flex flex-col justify-between p-4">
                <div className="space-y-3">
                  {/* Top Row */}
                  <div className="flex items-start justify-between gap-3">
                    <div className="flex items-center gap-2.5">
                      <div className="flex h-9 w-9 items-center justify-center rounded-lg border border-zinc-200 bg-zinc-50 text-zinc-700 dark:border-zinc-800 dark:bg-zinc-950 dark:text-zinc-200">
                        <Icon className="h-4 w-4" />
                      </div>
                      <div>
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
                        </div>
                        <p className="mt-0.5 font-mono text-[11px] text-zinc-500">
                          {m.channels.pacingLine(
                            formatDurationSecs(ch.min_interval_secs),
                            formatDurationSecs(ch.jitter_max_secs),
                            ch.hourly_quota,
                            ch.daily_quota,
                          )}
                        </p>
                      </div>
                    </div>

                    <Switch
                      checked={ch.enabled}
                      onCheckedChange={(enabled) =>
                        toggleEnabledMutation.mutate({ id: ch.id, enabled })
                      }
                    />
                  </div>

                  {/* Masked Target Details (`ChannelConfig::redacted_view()`) */}
                  <div className="rounded-lg border border-zinc-100 bg-zinc-50/70 px-3 py-2.5 text-xs dark:border-zinc-800/80 dark:bg-zinc-950/60">
                    {ch.config.kind === "smtp" && (
                      <div className="space-y-1">
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-zinc-500">
                            {m.channels.card.server}
                          </span>
                          <span className="font-mono">
                            {ch.config.host}:{ch.config.port} (
                            {ch.config.security})
                          </span>
                        </div>
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-zinc-500">
                            {m.channels.card.fromTo}
                          </span>
                          <span className="truncate font-mono">
                            {ch.config.from_address} →{" "}
                            {ch.config.to_addresses.join(", ")}
                          </span>
                        </div>
                        <div className="flex items-center justify-between gap-2">
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
                      <div className="space-y-1">
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-zinc-500">
                            {m.channels.card.targetChat}
                          </span>
                          <span className="font-mono">
                            {ch.config.chat_id}
                            {ch.config.message_thread_id !== null
                              ? m.channels.card.topicSuffix(
                                  ch.config.message_thread_id,
                                )
                              : ""}
                          </span>
                        </div>
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-zinc-500">
                            {m.channels.card.botApi}
                          </span>
                          <span className="truncate font-mono text-[11px]">
                            {ch.config.api_base_url ||
                              "https://api.telegram.org"}
                          </span>
                        </div>
                        <div className="flex items-center justify-between gap-2">
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
                      </div>
                    )}

                    {ch.config.kind === "notion" && (
                      <div className="space-y-1">
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-zinc-500">
                            {m.channels.card.dataSourceId}
                          </span>
                          <span className="truncate font-mono text-[11px]">
                            {ch.config.data_source_id}
                          </span>
                        </div>
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-zinc-500">
                            {m.channels.card.requiredKeys}
                          </span>
                          <span className="font-mono text-[11px]">
                            {ch.config.column_mapping.title} ·{" "}
                            {ch.config.column_mapping.fc_id}
                          </span>
                        </div>
                        <div className="flex items-center justify-between gap-2">
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

                  {/* Capabilities & Cooldown Badges */}
                  <div className="flex flex-wrap items-center gap-1.5">
                    {ch.kind === "telegram" &&
                      ch.capabilities.supports_rich_message !== undefined && (
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
                    {ch.capabilities.fallback_reason && (
                      <Badge variant="warning">
                        {m.channels.card.fallbackPrefix(
                          ch.capabilities.fallback_reason,
                        )}
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
                </div>

                {/* Bottom Actions */}
                <div className="mt-4 flex items-center justify-between border-t border-zinc-100 pt-3 dark:border-zinc-800">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => testMutation.mutate(ch.id)}
                    loading={testingId === ch.id}
                  >
                    <Send className="h-3.5 w-3.5" />
                    {m.channels.card.sendTest}
                  </Button>

                  <div className="flex items-center gap-1">
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => {
                        setEditingChannel(ch);
                        setWizardOpen(true);
                      }}
                    >
                      <Pencil className="h-3.5 w-3.5" />
                      {m.common.edit}
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
              </Card>
            );
          })}
        </div>
      )}

      {/* Create / Edit Wizard */}
      {wizardOpen && (
        <ChannelWizardModal
          open={wizardOpen}
          onClose={() => setWizardOpen(false)}
          channel={editingChannel}
        />
      )}

      {/* Delete Confirmation */}
      <ConfirmDialog
        open={deletingChannel !== null}
        onClose={() => setDeletingChannel(null)}
        onConfirm={() => {
          if (deletingChannel) deleteMutation.mutate(deletingChannel.id);
        }}
        title={m.channels.deleteModalTitle}
        description={m.channels.deleteModalDesc(deletingChannel?.name ?? "")}
        confirmLabel={m.channels.deleteModalConfirm}
        loading={deleteMutation.isPending}
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Channel Wizard Modal (SMTP / Telegram / Notion Auto-Create & Inspect-Schema)
// ---------------------------------------------------------------------------

function ChannelWizardModal({
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

  // --- SMTP state (`SmtpChannelConfig`) ---
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
  const [smtpToAddresses, setSmtpToAddresses] = useState<string[]>(
    initialSmtp?.to_addresses ?? [],
  );
  const [smtpReplyTo, setSmtpReplyTo] = useState(initialSmtp?.reply_to ?? "");

  // --- Telegram state (`TelegramChannelConfig`) ---
  const initialTg =
    channel?.config.kind === "telegram" ? channel.config : undefined;
  const [tgBotToken, setTgBotToken] = useState("");
  const [tgChatId, setTgChatId] = useState(initialTg?.chat_id ?? "");
  const [tgThreadId, setTgThreadId] = useState<string>(
    initialTg?.message_thread_id !== null &&
      initialTg?.message_thread_id !== undefined
      ? String(initialTg.message_thread_id)
      : "",
  );
  const [tgApiBaseUrl, setTgApiBaseUrl] = useState(
    initialTg?.api_base_url ?? "",
  );

  // --- Notion state (`NotionChannelConfig`) ---
  const initialNotion =
    channel?.config.kind === "notion" ? channel.config : undefined;
  const [notionMode, setNotionMode] = useState<"init_db" | "bind_existing">(
    isEdit ? "bind_existing" : "init_db",
  );
  const [notionSecret, setNotionSecret] = useState("");
  const [notionParentPageId, setNotionParentPageId] = useState(
    initialNotion?.parent_page_id ?? "",
  );
  const [notionDbTitle, setNotionDbTitle] = useState("FeedCourier Inbox");
  const [notionTargetInput, setNotionTargetInput] = useState(
    initialNotion?.data_source_id ?? "",
  );
  const [notionDataSourceId, setNotionDataSourceId] = useState(
    initialNotion?.data_source_id ?? "",
  );
  const [notionDatabaseId, setNotionDatabaseId] = useState<string | null>(
    initialNotion?.database_id ?? null,
  );
  const [notionMapping, setNotionMapping] = useState<NotionColumnMapping>(
    initialNotion?.column_mapping ?? { ...DEFAULT_NOTION_COLUMN_MAPPING },
  );
  const [schemaInspection, setSchemaInspection] =
    useState<NotionInspectSchemaResponse | null>(null);
  const [autoCreatedIds, setAutoCreatedIds] = useState<{
    databaseId: string;
    dataSourceId: string;
  } | null>(null);

  const [formError, setFormError] = useState<string | null>(null);

  const handleKindSwitch = (nextKind: ChannelKind) => {
    setKind(nextKind);
    const defaults = kindDefaults(nextKind);
    setMinIntervalSecs(defaults.defaultInterval);
    setJitterMaxSecs(defaults.defaultJitter);
    setFormError(null);
  };

  // Notion Mode 1: Auto-create 7-column database (`POST /api/v1/channels/notion/init-db`)
  const initNotionDbMutation = useMutation({
    mutationFn: () =>
      api.channels.notionInitDb({
        secret: notionSecret.trim(),
        parent_page_id: notionParentPageId.trim(),
        title: notionDbTitle.trim() || undefined,
        use_proxy: useProxy,
      }),
    onSuccess: (res) => {
      setNotionDataSourceId(res.data_source_id);
      setNotionTargetInput(res.data_source_id);
      setNotionDatabaseId(res.database_id);
      setNotionMapping(res.column_mapping);
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
      setFormError(errorMessage(err));
    },
  });

  // Notion Mode 2: Inspect / Patch existing data source (`POST /api/v1/channels/notion/inspect-schema`)
  const inspectNotionMutation = useMutation({
    mutationFn: (patchMissing: boolean) =>
      api.channels.notionInspectSchema({
        secret: notionSecret.trim(),
        data_source_or_db_id: notionTargetInput.trim(),
        patch_missing: patchMissing,
        use_proxy: useProxy,
        channel_id: isEdit ? channel?.id : undefined,
      }),
    onSuccess: (res, patchMissing) => {
      setSchemaInspection(res);
      setNotionDataSourceId(res.data_source_id);
      setNotionDatabaseId(res.database_id);
      setNotionMapping(res.suggested_mapping);
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
      setFormError(errorMessage(err));
    },
  });

  const saveMutation = useMutation({
    mutationFn: async () => {
      const parsedHourly = hourlyQuota.trim()
        ? Number(hourlyQuota.trim())
        : null;
      const parsedDaily = dailyQuota.trim() ? Number(dailyQuota.trim()) : null;

      if (isEdit && channel) {
        let configPatch: ChannelTargetPatch;
        if (kind === "smtp") {
          configPatch = {
            kind: "smtp",
            host: smtpHost.trim(),
            port: smtpPort,
            security: smtpSecurity,
            username: smtpUsername.trim() || null,
            password: smtpPassword.trim() ? smtpPassword : null,
            from_address: smtpFromAddress.trim(),
            to_addresses: smtpToAddresses,
            reply_to: smtpReplyTo.trim() || null,
          };
        } else if (kind === "telegram") {
          configPatch = {
            kind: "telegram",
            bot_token: tgBotToken.trim() ? tgBotToken.trim() : null,
            chat_id: tgChatId.trim(),
            message_thread_id: tgThreadId.trim()
              ? Number(tgThreadId.trim())
              : null,
            api_base_url: tgApiBaseUrl.trim() || null,
          };
        } else {
          configPatch = {
            kind: "notion",
            integration_secret: notionSecret.trim()
              ? notionSecret.trim()
              : null,
            data_source_id:
              notionDataSourceId.trim() || notionTargetInput.trim(),
            database_id: notionDatabaseId,
            parent_page_id: notionParentPageId.trim() || null,
            column_mapping: notionMapping,
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

      let configCreate: ChannelTargetCreate;
      if (kind === "smtp") {
        configCreate = {
          kind: "smtp",
          host: smtpHost.trim(),
          port: smtpPort,
          security: smtpSecurity,
          username: smtpUsername.trim() || null,
          password: smtpPassword || null,
          from_address: smtpFromAddress.trim(),
          to_addresses: smtpToAddresses,
          reply_to: smtpReplyTo.trim() || null,
        };
      } else if (kind === "telegram") {
        configCreate = {
          kind: "telegram",
          bot_token: tgBotToken.trim(),
          chat_id: tgChatId.trim(),
          message_thread_id: tgThreadId.trim()
            ? Number(tgThreadId.trim())
            : null,
          api_base_url: tgApiBaseUrl.trim() || null,
        };
      } else {
        configCreate = {
          kind: "notion",
          integration_secret: notionSecret.trim(),
          data_source_id:
            notionDataSourceId.trim() || notionTargetInput.trim(),
          database_id: notionDatabaseId,
          parent_page_id: notionParentPageId.trim() || null,
          column_mapping: notionMapping,
        };
      }

      const payload: CreateChannelRequest = {
        name: name.trim(),
        kind,
        config: configCreate,
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

    if (!name.trim()) {
      setFormError(m.channels.wizard.errors.emptyName);
      return;
    }

    if (kind === "smtp") {
      if (
        !smtpHost.trim() ||
        !smtpFromAddress.trim() ||
        smtpToAddresses.length === 0
      ) {
        setFormError(m.channels.wizard.errors.smtpRequired);
        return;
      }
      if (smtpSecurity !== "plain" && !isEdit && !smtpPassword) {
        setFormError(m.channels.wizard.errors.smtpPassword);
        return;
      }
    } else if (kind === "telegram") {
      if (!tgChatId.trim()) {
        setFormError(m.channels.wizard.errors.tgChatId);
        return;
      }
      if (!isEdit && !tgBotToken.trim()) {
        setFormError(m.channels.wizard.errors.tgToken);
        return;
      }
    } else if (kind === "notion") {
      if (!isEdit && !notionSecret.trim()) {
        setFormError(m.channels.wizard.errors.notionSecret);
        return;
      }
      const effectiveDsId =
        notionDataSourceId.trim() || notionTargetInput.trim();
      if (!effectiveDsId) {
        setFormError(m.channels.wizard.errors.notionDataSource);
        return;
      }
      if (!notionMapping.title.trim() || !notionMapping.fc_id.trim()) {
        setFormError(m.channels.wizard.errors.notionRequiredCols);
        return;
      }
    }

    saveMutation.mutate();
  };

  const missingColumnsCount =
    schemaInspection?.columns.filter((c) => c.status === "missing").length ?? 0;

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
        {/* Kind Selector */}
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

        {/* Channel Name */}
        <div>
          <Label htmlFor="ch-name">{m.channels.wizard.nameLabel}</Label>
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

        {/* 1. SMTP Specific Form */}
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

            <div className="grid gap-3 sm:grid-cols-2">
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

            <div>
              <Label>{m.channels.wizard.smtp.toLabel}</Label>
              <div className="mt-1">
                <TagInput
                  values={smtpToAddresses}
                  onChange={setSmtpToAddresses}
                  mono
                  placeholder="me@example.com"
                />
              </div>
            </div>
          </div>
        )}

        {/* 2. Telegram Specific Form */}
        {kind === "telegram" && (
          <div className="space-y-3.5 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
            <h3 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase">
              {m.channels.wizard.telegram.heading}
            </h3>
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="sm:col-span-2">
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

              <div className="sm:col-span-2">
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
          </div>
        )}

        {/* 3. Notion Specific Form (Auto-Create DB OR Bind Existing Database) */}
        {kind === "notion" && (
          <div className="space-y-4 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h3 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase">
                {m.channels.wizard.notion.heading}
              </h3>
              <div className="inline-flex rounded-lg border border-zinc-200 bg-zinc-50 p-0.5 dark:border-zinc-800 dark:bg-zinc-950">
                <button
                  type="button"
                  onClick={() => setNotionMode("init_db")}
                  className={`cursor-pointer rounded-md px-2.5 py-1 text-xs font-medium ${
                    notionMode === "init_db"
                      ? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
                      : "text-zinc-600 dark:text-zinc-400"
                  }`}
                >
                  {m.channels.wizard.notion.modeInit}
                </button>
                <button
                  type="button"
                  onClick={() => setNotionMode("bind_existing")}
                  className={`cursor-pointer rounded-md px-2.5 py-1 text-xs font-medium ${
                    notionMode === "bind_existing"
                      ? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
                      : "text-zinc-600 dark:text-zinc-400"
                  }`}
                >
                  {m.channels.wizard.notion.modeBind}
                </button>
              </div>
            </div>

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

            {notionMode === "init_db" ? (
              <div className="space-y-3 rounded-lg border border-zinc-200/80 bg-zinc-50/70 p-3.5 dark:border-zinc-800 dark:bg-zinc-950/60">
                <p className="text-xs text-zinc-600 dark:text-zinc-400">
                  {m.channels.wizard.notion.initIntro}
                </p>
                <div className="grid gap-3 sm:grid-cols-2">
                  <div>
                    <Label>{m.channels.wizard.notion.parentPageLabel}</Label>
                    <Input
                      value={notionParentPageId}
                      onChange={(e) => setNotionParentPageId(e.target.value)}
                      placeholder="https://www.notion.so/..."
                      className="mt-1 font-mono text-xs"
                    />
                  </div>
                  <div>
                    <Label>{m.channels.wizard.notion.dbTitleLabel}</Label>
                    <Input
                      value={notionDbTitle}
                      onChange={(e) => setNotionDbTitle(e.target.value)}
                      placeholder="FeedCourier Inbox"
                      className="mt-1"
                    />
                  </div>
                </div>
                <div className="flex items-center justify-between gap-3">
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={() => {
                      setFormError(null);
                      if (!notionSecret.trim() || !notionParentPageId.trim()) {
                        setFormError(
                          m.channels.wizard.errors.notionInitInputs,
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

                  {notionDataSourceId && (
                    <span className="truncate font-mono text-xs text-emerald-600 dark:text-emerald-400">
                      data_source_id: {notionDataSourceId}
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
                      value={notionTargetInput}
                      onChange={(e) => {
                        setNotionTargetInput(e.target.value);
                        setNotionDataSourceId(e.target.value);
                      }}
                      placeholder="data_source_id / https://www.notion.so/..."
                      className="flex-1 font-mono text-xs"
                    />
                    <Button
                      variant="secondary"
                      size="sm"
                      onClick={() => {
                        setFormError(null);
                        if (!notionSecret.trim() && !isEdit) {
                          setFormError(
                            m.channels.wizard.errors.notionInspectSecret,
                          );
                          return;
                        }
                        if (!notionTargetInput.trim()) {
                          setFormError(
                            m.channels.wizard.errors.notionInspectTarget,
                          );
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

                {/* Patch Missing Columns Confirmation Banner */}
                {schemaInspection && missingColumnsCount > 0 && (
                  <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-amber-200 bg-amber-50/80 px-3 py-2.5 text-xs text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-200">
                    <div className="flex items-center gap-2">
                      <AlertTriangle className="h-4 w-4 shrink-0 text-amber-600" />
                      <span>
                        {m.channels.wizard.notion.missingBanner(
                          missingColumnsCount,
                        )}
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
                        const currentVal = notionMapping[col.key] ?? "";

                        return (
                          <tr key={col.key}>
                            <td className="px-3 py-2">
                              <div className="flex items-center gap-1.5 font-medium">
                                <span className="font-mono">
                                  {col.standardName}
                                </span>
                                {col.required && (
                                  <Badge variant="danger">
                                    {m.common.required}
                                  </Badge>
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
                                    setNotionMapping((prev) => ({
                                      ...prev,
                                      [col.key]: col.required
                                        ? next
                                        : next || null,
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
                                    setNotionMapping((prev) => ({
                                      ...prev,
                                      [col.key]: col.required
                                        ? next
                                        : next || null,
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
        )}

        {/* Shared Pacing & Rate-Limit Guardrails */}
        <div className="space-y-3.5 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
          <h3 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase">
            {m.channels.wizard.pacing.heading}
          </h3>
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

        <FieldError message={formError} />
      </form>
    </Modal>
  );
}
