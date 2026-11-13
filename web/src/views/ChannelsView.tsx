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
  purpose: string;
}> = [
  {
    key: "title",
    standardName: "Title",
    expectedType: "title",
    required: true,
    purpose: "Article title",
  },
  {
    key: "fc_id",
    standardName: "FC-ID",
    expectedType: "rich_text",
    required: true,
    purpose: "SHA-256 fingerprint for idempotency reconciliation",
  },
  {
    key: "link",
    standardName: "Link",
    expectedType: "url",
    required: false,
    purpose: "Canonical article URL",
  },
  {
    key: "source",
    standardName: "Source",
    expectedType: "select",
    required: false,
    purpose: "Feed source title",
  },
  {
    key: "published",
    standardName: "Published",
    expectedType: "date",
    required: false,
    purpose: "Original publication timestamp",
  },
  {
    key: "state",
    standardName: "State",
    expectedType: "select",
    required: false,
    purpose: "UnRead / Read reading state",
  },
  {
    key: "tag",
    standardName: "Tag",
    expectedType: "multi_select",
    required: false,
    purpose: "Synced subscription tags",
  },
];

function kindMeta(kind: ChannelKind) {
  switch (kind) {
    case "smtp":
      return {
        label: "SMTP Email",
        icon: Mail,
        defaultInterval: 15,
        defaultJitter: 10,
      };
    case "telegram":
      return {
        label: "Telegram Bot",
        icon: MessageSquare,
        defaultInterval: 3,
        defaultJitter: 2,
      };
    case "notion":
      return {
        label: "Notion Data Source",
        icon: Database,
        defaultInterval: 2,
        defaultJitter: 2,
      };
  }
}

export function ChannelsView() {
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
      return api.channels.test(id);
    },
    onSuccess: (res) => {
      if (res.ok) {
        toast.success("Connectivity test succeeded", res.message);
      } else {
        toast.error("Connectivity test reported an issue", res.message);
      }
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
    },
    onError: (err) => {
      toast.error("Connectivity test failed", errorMessage(err));
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
      toast.error("Failed to toggle channel", errorMessage(err));
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (id: number) => api.channels.remove(id),
    onSuccess: () => {
      toast.success("Channel deleted");
      setDeletingChannel(null);
      void queryClient.invalidateQueries({ queryKey: ["channels"] });
      void queryClient.invalidateQueries({ queryKey: ["routes"] });
    },
    onError: (err) => {
      toast.error("Failed to delete channel", errorMessage(err));
    },
  });

  return (
    <div className="space-y-5">
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-lg font-bold tracking-tight">
            Trusted Delivery Channels
          </h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400">
            Credentials & destinations are ChaCha20-Poly1305 encrypted and locked exclusively to WebUI (never exposed to MCP prompts).
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
          New Trusted Channel
        </Button>
      </div>

      {/* Channels List */}
      {channelsQuery.isLoading ? (
        <Card className="p-8 text-center text-xs text-zinc-500">
          Loading trusted channels…
        </Card>
      ) : (channelsQuery.data?.length ?? 0) === 0 ? (
        <EmptyState
          icon={Radio}
          title="No trusted channels configured"
          description="Set up an SMTP inbox, a Telegram chat/topic (Bot API 10.4), or a Notion data source (2026-03-11) before creating delivery routes."
          action={
            <Button
              size="sm"
              onClick={() => {
                setEditingChannel(null);
                setWizardOpen(true);
              }}
            >
              <Plus className="h-3.5 w-3.5" />
              Configure First Channel
            </Button>
          }
        />
      ) : (
        <div className="grid gap-3.5 lg:grid-cols-2">
          {(channelsQuery.data ?? []).map((ch) => {
            const meta = kindMeta(ch.kind);
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
                            <Badge variant="default">Disabled</Badge>
                          )}
                          {ch.use_proxy && (
                            <Badge variant="outline">
                              <Globe className="h-3 w-3" />
                              Proxy
                            </Badge>
                          )}
                        </div>
                        <p className="mt-0.5 text-[11px] text-zinc-500">
                          Pacing: min{" "}
                          <span className="font-mono font-medium text-zinc-700 dark:text-zinc-300">
                            {formatDurationSecs(ch.min_interval_secs)}
                          </span>{" "}
                          + up to{" "}
                          <span className="font-mono font-medium text-zinc-700 dark:text-zinc-300">
                            {formatDurationSecs(ch.jitter_max_secs)}
                          </span>{" "}
                          jitter
                          {ch.hourly_quota ? ` · ${ch.hourly_quota}/hr` : ""}
                          {ch.daily_quota ? ` · ${ch.daily_quota}/day` : ""}
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
                          <span className="text-zinc-500">Server:</span>
                          <span className="font-mono">
                            {ch.config.host}:{ch.config.port} (
                            {ch.config.security})
                          </span>
                        </div>
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-zinc-500">
                            Force-From → To:
                          </span>
                          <span className="truncate font-mono">
                            {ch.config.from_address} →{" "}
                            {ch.config.to_addresses.join(", ")}
                          </span>
                        </div>
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-zinc-500">Auth:</span>
                          <span className="inline-flex items-center gap-1 font-mono text-[11px] text-zinc-500">
                            <Lock className="h-3 w-3" />
                            {ch.config.username || "anonymous"}{" "}
                            {ch.config.has_password ? "(••••••••)" : "(no pwd)"}
                          </span>
                        </div>
                      </div>
                    )}

                    {ch.config.kind === "telegram" && (
                      <div className="space-y-1">
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-zinc-500">Target Chat:</span>
                          <span className="font-mono">
                            {ch.config.chat_id}
                            {ch.config.message_thread_id !== null
                              ? ` (topic #${ch.config.message_thread_id})`
                              : ""}
                          </span>
                        </div>
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-zinc-500">Bot API:</span>
                          <span className="truncate font-mono text-[11px]">
                            {ch.config.api_base_url ||
                              "https://api.telegram.org"}
                          </span>
                        </div>
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-zinc-500">Bot Token:</span>
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
                          <span className="text-zinc-500">Data Source ID:</span>
                          <span className="truncate font-mono text-[11px]">
                            {ch.config.data_source_id}
                          </span>
                        </div>
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-zinc-500">Required Keys:</span>
                          <span className="font-mono text-[11px]">
                            {ch.config.column_mapping.title} ·{" "}
                            {ch.config.column_mapping.fc_id}
                          </span>
                        </div>
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-zinc-500">Secret:</span>
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
                            ? "Bot API 10.4 sendRichMessage Ready"
                            : "sendRichMessage Unsupported (Compact Fallback)"}
                        </Badge>
                      )}
                    {ch.capabilities.fallback_reason && (
                      <Badge variant="warning">
                        Fallback: {ch.capabilities.fallback_reason}
                      </Badge>
                    )}
                    {hasCooldown && ch.cooldown_until && (
                      <Badge
                        variant="warning"
                        title={formatDateTime(ch.cooldown_until)}
                      >
                        Cooldown {formatRelativeTime(ch.cooldown_until)}
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
                    Send Test
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
                      Edit
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => setDeletingChannel(ch)}
                      className="text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-950/40"
                      title="Delete channel"
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
        title="Delete Trusted Channel"
        description={`Delete "${deletingChannel?.name}"? Any routes and pending delivery tasks bound to this channel will also be removed via CASCADE.`}
        confirmLabel="Delete Channel"
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
  const toast = useToast();
  const queryClient = useQueryClient();
  const isEdit = channel !== null;

  const [kind, setKind] = useState<ChannelKind>(channel?.kind ?? "telegram");
  const [name, setName] = useState(channel?.name ?? "");
  const [minIntervalSecs, setMinIntervalSecs] = useState<number>(
    channel?.min_interval_secs ??
      kindMeta(channel?.kind ?? "telegram").defaultInterval,
  );
  const [jitterMaxSecs, setJitterMaxSecs] = useState<number>(
    channel?.jitter_max_secs ??
      kindMeta(channel?.kind ?? "telegram").defaultJitter,
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
  const [autoCreatedBanner, setAutoCreatedBanner] = useState<string | null>(
    null,
  );

  const [formError, setFormError] = useState<string | null>(null);

  const handleKindSwitch = (nextKind: ChannelKind) => {
    setKind(nextKind);
    const defaults = kindMeta(nextKind);
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
      setAutoCreatedBanner(
        `Created 7-column database (${res.database_id}) → bound data_source_id ${res.data_source_id}.`,
      );
      toast.success(
        "Notion 7-column database created",
        `Bound data_source_id: ${res.data_source_id}`,
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
          "Patched Notion data source schema",
          `Added columns: ${res.patched_columns.join(", ")}`,
        );
      } else {
        toast.info(
          "Schema inspected",
          res.ready
            ? "Required columns (Title & FC-ID) verified."
            : "Review missing or mismatched columns below.",
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
      toast.success(isEdit ? "Channel updated" : "Trusted channel created");
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
      setFormError("Please provide a channel display name.");
      return;
    }

    if (kind === "smtp") {
      if (
        !smtpHost.trim() ||
        !smtpFromAddress.trim() ||
        smtpToAddresses.length === 0
      ) {
        setFormError(
          "SMTP host, Force-From address, and at least one trusted recipient To address are required.",
        );
        return;
      }
      if (smtpSecurity !== "plain" && !isEdit && !smtpPassword) {
        setFormError(
          "SMTP password is required for TLS/STARTTLS authentication.",
        );
        return;
      }
    } else if (kind === "telegram") {
      if (!tgChatId.trim()) {
        setFormError("Telegram target chat_id is required.");
        return;
      }
      if (!isEdit && !tgBotToken.trim()) {
        setFormError("Telegram Bot Token is required when creating a channel.");
        return;
      }
    } else if (kind === "notion") {
      if (!isEdit && !notionSecret.trim()) {
        setFormError(
          "Notion Integration Secret is required when creating a channel.",
        );
        return;
      }
      const effectiveDsId =
        notionDataSourceId.trim() || notionTargetInput.trim();
      if (!effectiveDsId) {
        setFormError(
          "Notion data_source_id is required. Auto-create a database or inspect an existing data source first.",
        );
        return;
      }
      if (!notionMapping.title.trim() || !notionMapping.fc_id.trim()) {
        setFormError(
          "Notion column mapping requires both Title and FC-ID columns.",
        );
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
      title={isEdit ? `Edit Channel: ${channel?.name}` : "New Trusted Channel"}
      description="All secrets are encrypted with ChaCha20-Poly1305. Channels sharing the same underlying credential automatically share pacing locks."
      size="xl"
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button onClick={handleSubmit} loading={saveMutation.isPending}>
            {isEdit ? "Save Channel" : "Create Channel"}
          </Button>
        </>
      }
    >
      <form onSubmit={handleSubmit} className="space-y-5">
        {/* Kind Selector */}
        {!isEdit && (
          <div className="grid grid-cols-3 gap-2.5">
            {(["telegram", "smtp", "notion"] as const).map((k) => {
              const m = kindMeta(k);
              const Icon = m.icon;
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
                  <span className="text-xs font-semibold">{m.label}</span>
                </button>
              );
            })}
          </div>
        )}

        {/* Channel Name */}
        <div>
          <Label htmlFor="ch-name">Channel Remark Name</Label>
          <Input
            id="ch-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={
              kind === "smtp"
                ? "Personal Reading Inbox"
                : kind === "telegram"
                  ? "Tech Alerts Topic"
                  : "Notion Knowledge Base"
            }
            className="mt-1.5"
          />
        </div>

        {/* 1. SMTP Specific Form */}
        {kind === "smtp" && (
          <div className="space-y-3.5 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
            <h3 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase">
              SMTP Server & rss2email Force-From Envelope
            </h3>
            <div className="grid gap-3 sm:grid-cols-3">
              <div>
                <Label>SMTP Host</Label>
                <Input
                  value={smtpHost}
                  onChange={(e) => setSmtpHost(e.target.value)}
                  placeholder="smtp.fastmail.com"
                  className="mt-1 font-mono text-xs"
                />
              </div>
              <div>
                <Label>Port</Label>
                <Input
                  type="number"
                  value={smtpPort}
                  onChange={(e) => setSmtpPort(Number(e.target.value))}
                  className="mt-1 font-mono text-xs"
                />
              </div>
              <div>
                <Label>Transport Security</Label>
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
                  <option value="start_tls">STARTTLS (587)</option>
                  <option value="tls">Implicit TLS / SMTPS (465)</option>
                  <option value="plain">Plain Local Relay (25)</option>
                </Select>
              </div>
            </div>

            <div className="grid gap-3 sm:grid-cols-2">
              <div>
                <Label>SMTP Username</Label>
                <Input
                  value={smtpUsername}
                  onChange={(e) => setSmtpUsername(e.target.value)}
                  placeholder="courier@example.com"
                  className="mt-1 font-mono text-xs"
                />
              </div>
              <div>
                <Label>SMTP App Password</Label>
                <Input
                  type="password"
                  value={smtpPassword}
                  onChange={(e) => setSmtpPassword(e.target.value)}
                  placeholder={
                    initialSmtp?.has_password
                      ? "Stored (••••••••) — leave blank to keep"
                      : "••••••••••••"
                  }
                  className="mt-1 font-mono text-xs"
                />
              </div>
            </div>

            <div className="grid gap-3 sm:grid-cols-2">
              <div>
                <Label>Force-From Address (SPF/DKIM Verified)</Label>
                <Input
                  value={smtpFromAddress}
                  onChange={(e) => setSmtpFromAddress(e.target.value)}
                  placeholder="rss@yourdomain.com"
                  className="mt-1 font-mono text-xs"
                />
                <FieldHint>
                  Display name dynamically adopts each feed’s title; injects deterministic{" "}
                  <code className="font-mono">Message-ID</code> &{" "}
                  <code className="font-mono">List-Id</code>.
                </FieldHint>
              </div>
              <div>
                <Label>Reply-To Address (Optional)</Label>
                <Input
                  value={smtpReplyTo}
                  onChange={(e) => setSmtpReplyTo(e.target.value)}
                  placeholder="Optional Reply-To header"
                  className="mt-1 font-mono text-xs"
                />
              </div>
            </div>

            <div>
              <Label>Trusted Recipient Addresses (Press Enter to add)</Label>
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
              Telegram Bot API 10.4 Endpoint & Target
            </h3>
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="sm:col-span-2">
                <Label>Bot Token</Label>
                <Input
                  type="password"
                  value={tgBotToken}
                  onChange={(e) => setTgBotToken(e.target.value)}
                  placeholder={
                    initialTg
                      ? `Stored (${initialTg.bot_token_hint}) — leave blank to keep`
                      : "123456789:ABCdefGHIjklMNOpqrSTUvwxYZ"
                  }
                  className="mt-1 font-mono text-xs"
                />
              </div>

              <div>
                <Label>Target Chat ID or @Channel</Label>
                <Input
                  value={tgChatId}
                  onChange={(e) => setTgChatId(e.target.value)}
                  placeholder="-1001234567890 or @mychannel"
                  className="mt-1 font-mono text-xs"
                />
              </div>

              <div>
                <Label>Forum Topic Thread ID (Optional)</Label>
                <Input
                  type="number"
                  value={tgThreadId}
                  onChange={(e) => setTgThreadId(e.target.value)}
                  placeholder="e.g. 42"
                  className="mt-1 font-mono text-xs"
                />
              </div>

              <div className="sm:col-span-2">
                <Label>Custom Bot API Base URL (Optional)</Label>
                <Input
                  value={tgApiBaseUrl}
                  onChange={(e) => setTgApiBaseUrl(e.target.value)}
                  placeholder="https://api.telegram.org (supports reverse proxy or Local Bot API Server)"
                  className="mt-1 font-mono text-xs"
                />
                <FieldHint>
                  FeedCourier probes <code className="font-mono">sendRichMessage</code> capability on save and automatically falls back to Compact Card if a self-hosted Local Bot API lags Bot API 10.3+.
                </FieldHint>
              </div>
            </div>
          </div>
        )}

        {/* 3. Notion Specific Form (2026-03-11: Auto-Create DB OR Bind Existing Data Source) */}
        {kind === "notion" && (
          <div className="space-y-4 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h3 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase">
                Notion Data Source (API 2026-03-11 · Markdown v2)
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
                  1. Auto-Create 7-Column DB
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
                  2. Bind Existing Data Source
                </button>
              </div>
            </div>

            <div>
              <Label>Notion Internal Integration Secret</Label>
              <Input
                type="password"
                value={notionSecret}
                onChange={(e) => setNotionSecret(e.target.value)}
                placeholder={
                  initialNotion
                    ? `Stored (${initialNotion.secret_hint}) — leave blank to keep`
                    : "ntn_••••••••••••"
                }
                className="mt-1 font-mono text-xs"
              />
            </div>

            {notionMode === "init_db" ? (
              <div className="space-y-3 rounded-lg border border-zinc-200/80 bg-zinc-50/70 p-3.5 dark:border-zinc-800 dark:bg-zinc-950/60">
                <p className="text-xs text-zinc-600 dark:text-zinc-400">
                  Zero-friction setup: FeedCourier calls{" "}
                  <code className="font-mono">POST /v1/databases</code> with{" "}
                  <code className="font-mono">
                    initial_data_source.properties
                  </code>{" "}
                  to provision{" "}
                  <code className="font-mono">
                    Title, Link, Source, Published, State, Tag, FC-ID
                  </code>{" "}
                  and binds the resulting{" "}
                  <code className="font-mono">data_source_id</code>.
                </p>
                <div className="grid gap-3 sm:grid-cols-2">
                  <div>
                    <Label>Authorized Parent Page ID or URL</Label>
                    <Input
                      value={notionParentPageId}
                      onChange={(e) => setNotionParentPageId(e.target.value)}
                      placeholder="https://www.notion.so/..."
                      className="mt-1 font-mono text-xs"
                    />
                  </div>
                  <div>
                    <Label>New Database Title</Label>
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
                          "Enter both Notion Integration Secret and Parent Page ID/URL to initialize a database.",
                        );
                        return;
                      }
                      initNotionDbMutation.mutate();
                    }}
                    loading={initNotionDbMutation.isPending}
                  >
                    <Sparkles className="h-3.5 w-3.5 text-emerald-600" />
                    Auto-Create 7-Column Database Now
                  </Button>

                  {notionDataSourceId && (
                    <span className="truncate font-mono text-xs text-emerald-600 dark:text-emerald-400">
                      data_source_id: {notionDataSourceId}
                    </span>
                  )}
                </div>

                {autoCreatedBanner && (
                  <div className="flex items-center gap-2 rounded-md border border-emerald-200 bg-emerald-50 px-3 py-2 text-xs text-emerald-800 dark:border-emerald-900/60 dark:bg-emerald-950/40 dark:text-emerald-300">
                    <CheckCircle2 className="h-4 w-4 shrink-0" />
                    <span>{autoCreatedBanner}</span>
                  </div>
                )}
              </div>
            ) : (
              <div className="space-y-3 rounded-lg border border-zinc-200/80 bg-zinc-50/70 p-3.5 dark:border-zinc-800 dark:bg-zinc-950/60">
                <div>
                  <Label>Existing Data Source ID or Database URL</Label>
                  <div className="mt-1 flex flex-col gap-2 sm:flex-row">
                    <Input
                      value={notionTargetInput}
                      onChange={(e) => {
                        setNotionTargetInput(e.target.value);
                        setNotionDataSourceId(e.target.value);
                      }}
                      placeholder="data_source_id or https://www.notion.so/..."
                      className="flex-1 font-mono text-xs"
                    />
                    <Button
                      variant="secondary"
                      size="sm"
                      onClick={() => {
                        setFormError(null);
                        if (!notionSecret.trim() && !isEdit) {
                          setFormError(
                            "Provide Notion Integration Secret before inspecting schema.",
                          );
                          return;
                        }
                        if (!notionTargetInput.trim()) {
                          setFormError(
                            "Enter a Notion data_source_id or database ID/URL.",
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
                      Inspect Schema
                    </Button>
                  </div>
                </div>

                {/* Patch Missing Columns Confirmation Banner */}
                {schemaInspection && missingColumnsCount > 0 && (
                  <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-amber-200 bg-amber-50/80 px-3 py-2.5 text-xs text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-200">
                    <div className="flex items-center gap-2">
                      <AlertTriangle className="h-4 w-4 shrink-0 text-amber-600" />
                      <span>
                        <strong>{missingColumnsCount}</strong> standard{" "}
                        {missingColumnsCount === 1
                          ? "column is"
                          : "columns are"}{" "}
                        missing on this data source (including{" "}
                        <code className="font-mono">FC-ID</code> if unmapped).
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
                      Confirm & Patch Missing Columns
                    </Button>
                  </div>
                )}

                {/* 7-Column Mapping Table */}
                <div className="overflow-x-auto rounded-lg border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-900">
                  <table className="w-full text-left text-xs">
                    <thead className="border-b border-zinc-100 bg-zinc-50 text-[11px] text-zinc-500 dark:border-zinc-800 dark:bg-zinc-950">
                      <tr>
                        <th className="px-3 py-2 font-medium">Field</th>
                        <th className="px-3 py-2 font-medium">Expected Type</th>
                        <th className="px-3 py-2 font-medium">Status</th>
                        <th className="px-3 py-2 font-medium">
                          Mapped Notion Property
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
                                  <Badge variant="danger">Required</Badge>
                                )}
                              </div>
                              <div className="text-[10px] text-zinc-400">
                                {col.purpose}
                              </div>
                            </td>
                            <td className="px-3 py-2 font-mono text-[11px] text-zinc-500">
                              {col.expectedType}
                              {inspected?.actual_type &&
                                inspected.actual_type !== col.expectedType && (
                                  <span className="ml-1 text-rose-500">
                                    (found: {inspected.actual_type})
                                  </span>
                                )}
                            </td>
                            <td className="px-3 py-2">
                              {inspected ? (
                                inspected.status === "matched" ? (
                                  <Badge variant="success">Matched</Badge>
                                ) : inspected.status === "mismatched" ? (
                                  <Badge variant="warning">Type Mismatch</Badge>
                                ) : (
                                  <Badge variant="danger">Missing</Badge>
                                )
                              ) : (
                                <Badge variant="outline">Unverified</Badge>
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
                                      — Select {col.expectedType} column —
                                    </option>
                                  ) : (
                                    <option value="">— Skip field —</option>
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
                                      : "Optional (blank to skip)"
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
            Anti-Ban Pacing, Random Jitter & Quota Guardrails
          </h3>
          <div className="grid gap-3 sm:grid-cols-4">
            <div>
              <Label>Min Interval (Secs)</Label>
              <Input
                type="number"
                min={0}
                value={minIntervalSecs}
                onChange={(e) => setMinIntervalSecs(Number(e.target.value))}
                className="mt-1 font-mono text-xs"
              />
            </div>
            <div>
              <Label>Max Jitter (Secs)</Label>
              <Input
                type="number"
                min={0}
                value={jitterMaxSecs}
                onChange={(e) => setJitterMaxSecs(Number(e.target.value))}
                className="mt-1 font-mono text-xs"
              />
            </div>
            <div>
              <Label>Hourly Quota (Opt)</Label>
              <Input
                type="number"
                min={1}
                value={hourlyQuota}
                onChange={(e) => setHourlyQuota(e.target.value)}
                placeholder="Unlimited"
                className="mt-1 font-mono text-xs"
              />
            </div>
            <div>
              <Label>Daily Quota (Opt)</Label>
              <Input
                type="number"
                min={1}
                value={dailyQuota}
                onChange={(e) => setDailyQuota(e.target.value)}
                placeholder="Unlimited"
                className="mt-1 font-mono text-xs"
              />
            </div>
          </div>

          <div className="grid gap-3 pt-1 sm:grid-cols-2">
            <Switch
              id="ch-proxy"
              checked={useProxy}
              onCheckedChange={setUseProxy}
              label="Use Outbound Proxy"
              description="Route API/SMTP traffic through the global HTTP/SOCKS5 proxy."
            />
            <Switch
              id="ch-enabled"
              checked={enabled}
              onCheckedChange={setEnabled}
              label="Channel Enabled"
              description="Pause delivery to this destination without deleting routes."
            />
          </div>
        </div>

        <FieldError message={formError} />
      </form>
    </Modal>
  );
}
