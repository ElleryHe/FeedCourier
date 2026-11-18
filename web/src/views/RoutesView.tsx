import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  ArrowRight,
  CalendarDays,
  Clock,
  FileText,
  Filter,
  GitFork,
  Image,
  Languages,
  MessageSquareText,
  Pencil,
  Plus,
  Trash2,
} from "lucide-react";
import { type FormEvent, useMemo, useState } from "react";
import { api } from "@/api/client";
import type {
  ChannelDto,
  CreateRouteRequest,
  ActiveWindow,
  FeedDto,
  RouteDto,
  RouteOptions,
  SystemLocale,
  TelegramDeliveryMode,
  UpdateRouteRequest,
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
import { type Locale, useI18n } from "@/i18n";
import type { Messages } from "@/i18n/en";
import { errorMessage, validateRegex } from "@/lib/utils";

function telegramModeLabel(
  mode: TelegramDeliveryMode | undefined,
  m: Messages,
): string {
  switch (mode) {
    case "compact_card":
      return m.routes.modes.compact_card.label;
    case "media_mode":
      return m.routes.modes.media_mode.label;
    default:
      return m.routes.modes.rich_message.label;
  }
}

const WEEKDAYS = [1, 2, 3, 4, 5, 6, 7] as const;
const TIME_RE = /^([01]\d|2[0-3]):[0-5]\d$/;

function localeName(locale: SystemLocale): string {
  return locale === "zh-CN" ? "简体中文" : "English";
}

function formatWindow(w: ActiveWindow, allDay: string): string {
  return w.start === w.end ? allDay : `${w.start}–${w.end}`;
}

export function RoutesView() {
  const { m, locale: uiLocale } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();

  const settingsQuery = useQuery({
    queryKey: ["settings"],
    queryFn: () => api.settings.get(),
  });
  const timezone = settingsQuery.data?.timezone ?? null;
  const legacyLocale: Locale | null = settingsQuery.data?.locale ?? null;

  const [editorOpen, setEditorOpen] = useState(false);
  const [editingRoute, setEditingRoute] = useState<RouteDto | null>(null);
  const [deletingRoute, setDeletingRoute] = useState<RouteDto | null>(null);

  const routesQuery = useQuery({
    queryKey: ["routes"],
    queryFn: () => api.routes.list(),
  });

  const feedsQuery = useQuery({
    queryKey: ["feeds"],
    queryFn: () => api.feeds.list(),
  });

  const channelsQuery = useQuery({
    queryKey: ["channels"],
    queryFn: () => api.channels.list(),
  });

  const feedsById = useMemo(() => {
    const map = new Map<number, FeedDto>();
    for (const f of feedsQuery.data ?? []) map.set(f.id, f);
    return map;
  }, [feedsQuery.data]);

  const channelsById = useMemo(() => {
    const map = new Map<number, ChannelDto>();
    for (const c of channelsQuery.data ?? []) map.set(c.id, c);
    return map;
  }, [channelsQuery.data]);

  const allFeedTags = useMemo(() => {
    const tags = new Set<string>();
    for (const f of feedsQuery.data ?? []) {
      for (const t of f.tags) tags.add(t);
    }
    return Array.from(tags).sort();
  }, [feedsQuery.data]);

  const toggleMutation = useMutation({
    mutationFn: ({ id, enabled }: { id: number; enabled: boolean }) =>
      api.routes.update(id, { enabled }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["routes"] });
    },
    onError: (err) => {
      toast.error(m.routes.toastToggleFailed, errorMessage(err));
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (id: number) => api.routes.remove(id),
    onSuccess: () => {
      toast.success(m.routes.toastDeleted);
      setDeletingRoute(null);
      void queryClient.invalidateQueries({ queryKey: ["routes"] });
    },
    onError: (err) => {
      toast.error(m.routes.toastDeleteFailed, errorMessage(err));
    },
  });

  return (
    <div className="space-y-5">
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-lg font-bold tracking-tight">{m.routes.title}</h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400">
            {m.routes.subtitle}
          </p>
        </div>
        <Button
          size="sm"
          onClick={() => {
            setEditingRoute(null);
            setEditorOpen(true);
          }}
        >
          <Plus className="h-3.5 w-3.5" />
          {m.routes.newRoute}
        </Button>
      </div>

      {/* Routes List */}
      {routesQuery.isLoading ? (
        <Card className="p-8 text-center text-xs text-zinc-500">
          {m.routes.loading}
        </Card>
      ) : (routesQuery.data?.length ?? 0) === 0 ? (
        <EmptyState
          icon={GitFork}
          title={m.routes.emptyTitle}
          description={m.routes.emptyDesc}
          action={
            <Button
              size="sm"
              onClick={() => {
                setEditingRoute(null);
                setEditorOpen(true);
              }}
            >
              <Plus className="h-3.5 w-3.5" />
              {m.routes.createFirst}
            </Button>
          }
        />
      ) : (
        <div className="space-y-3">
          {(routesQuery.data ?? []).map((route) => {
            const targetChannel = channelsById.get(route.channel_id);
            const hasFilters =
              route.include_keywords.length > 0 ||
              route.exclude_keywords.length > 0 ||
              Boolean(route.include_regex) ||
              Boolean(route.exclude_regex);

            return (
              <Card key={route.id} className="p-4">
                <div className="flex flex-col justify-between gap-3 lg:flex-row lg:items-center">
                  <div className="min-w-0 flex-1 space-y-2">
                    {/* Title + Status */}
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="text-sm font-semibold">
                        {route.name}
                      </span>
                      {!route.enabled && (
                        <Badge variant="default">{m.common.disabled}</Badge>
                      )}
                      {targetChannel?.kind === "telegram" && (
                        <Badge variant="info">
                          {telegramModeLabel(route.options.telegram_mode, m)}
                          {route.options.telegram_mode === "compact_card" &&
                            (route.options.link_preview === false
                              ? m.routes.previewOff
                              : m.routes.previewOn)}
                        </Badge>
                      )}
                      <Badge variant="outline">
                        <Languages className="h-3 w-3" />
                        {localeName(
                          route.options.locale ?? legacyLocale ?? uiLocale,
                        )}
                      </Badge>
                      {(route.options.active_windows ?? []).length > 0 && (
                        <Badge
                          variant="warning"
                          title={m.routes.windowBadgeTitle(timezone ?? "")}
                        >
                          <Clock className="h-3 w-3" />
                          {(route.options.active_windows ?? [])
                            .map((w) => formatWindow(w, m.routes.allDay))
                            .join(", ")}
                        </Badge>
                      )}
                      {(route.options.active_weekdays ?? []).length > 0 &&
                        (route.options.active_weekdays ?? []).length < 7 && (
                          <Badge variant="warning">
                            <CalendarDays className="h-3 w-3" />
                            {[...(route.options.active_weekdays ?? [])]
                              .sort((a, b) => a - b)
                              .map((d) => m.routes.weekdaysShort[d - 1] ?? d)
                              .join(" ")}
                          </Badge>
                        )}
                    </div>

                    {/* Source -> Destination Flow */}
                    <div className="flex flex-wrap items-center gap-2 text-xs">
                      <div className="flex flex-wrap items-center gap-1">
                        {route.feed_ids.map((fid) => {
                          const f = feedsById.get(fid);
                          return (
                            <Badge key={fid} variant="outline">
                              {f?.title || f?.url || m.routes.feedFallback(fid)}
                            </Badge>
                          );
                        })}
                        {route.match_tags.map((tag) => (
                          <Badge key={tag} variant="mono">
                            #{tag}
                          </Badge>
                        ))}
                      </div>

                      <ArrowRight className="h-3.5 w-3.5 shrink-0 text-zinc-400" />

                      <Badge variant={targetChannel ? "success" : "danger"}>
                        {targetChannel
                          ? `${targetChannel.name} (${targetChannel.kind.toUpperCase()})`
                          : m.routes.channelFallback(route.channel_id)}
                      </Badge>
                    </div>

                    {/* Filter Summary */}
                    {hasFilters && (
                      <div className="flex flex-wrap items-center gap-1.5 pt-0.5 text-[11px] text-zinc-500">
                        <Filter className="h-3 w-3 shrink-0" />
                        {route.include_keywords.length > 0 && (
                          <span>
                            +kw:{" "}
                            <code className="font-mono">
                              {route.include_keywords.join(", ")}
                            </code>
                          </span>
                        )}
                        {route.exclude_keywords.length > 0 && (
                          <span>
                            -kw:{" "}
                            <code className="font-mono">
                              {route.exclude_keywords.join(", ")}
                            </code>
                          </span>
                        )}
                        {route.include_regex && (
                          <span>
                            +re:{" "}
                            <code className="font-mono">
                              /{route.include_regex}/
                            </code>
                          </span>
                        )}
                        {route.exclude_regex && (
                          <span>
                            -re:{" "}
                            <code className="font-mono">
                              /{route.exclude_regex}/
                            </code>
                          </span>
                        )}
                      </div>
                    )}
                  </div>

                  {/* Controls */}
                  <div className="flex shrink-0 items-center gap-2">
                    <Switch
                      checked={route.enabled}
                      onCheckedChange={(enabled) =>
                        toggleMutation.mutate({ id: route.id, enabled })
                      }
                    />
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => {
                        setEditingRoute(route);
                        setEditorOpen(true);
                      }}
                    >
                      <Pencil className="h-3.5 w-3.5" />
                      {m.common.edit}
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => setDeletingRoute(route)}
                      title={m.common.delete}
                      aria-label={m.common.delete}
                      className="text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-950/40"
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

      {/* Route Editor Modal */}
      {editorOpen && (
        <RouteEditorModal
          open={editorOpen}
          onClose={() => setEditorOpen(false)}
          route={editingRoute}
          feeds={feedsQuery.data ?? []}
          channels={channelsQuery.data ?? []}
          knownTags={allFeedTags}
          timezone={timezone}
          legacyLocale={legacyLocale}
        />
      )}

      {/* Delete Confirmation */}
      <ConfirmDialog
        open={deletingRoute !== null}
        onClose={() => setDeletingRoute(null)}
        onConfirm={() => {
          if (deletingRoute) deleteMutation.mutate(deletingRoute.id);
        }}
        title={m.routes.deleteModalTitle}
        description={m.routes.deleteModalDesc(deletingRoute?.name ?? "")}
        confirmLabel={m.routes.deleteModalConfirm}
        loading={deleteMutation.isPending}
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Route Editor Modal
// ---------------------------------------------------------------------------

function RouteEditorModal({
  open,
  onClose,
  route,
  feeds,
  channels,
  knownTags,
  timezone,
  legacyLocale,
}: {
  open: boolean;
  onClose: () => void;
  route: RouteDto | null;
  feeds: FeedDto[];
  channels: ChannelDto[];
  knownTags: string[];
  timezone: string | null;
  legacyLocale: Locale | null;
}) {
  const { m, locale: uiLocale } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const isEdit = route !== null;

  const [name, setName] = useState(route?.name ?? "");
  const [feedIds, setFeedIds] = useState<number[]>(route?.feed_ids ?? []);
  const [matchTags, setMatchTags] = useState<string[]>(
    route?.match_tags ?? [],
  );
  const [channelId, setChannelId] = useState<number>(
    route?.channel_id ?? channels[0]?.id ?? 0,
  );
  const [includeKeywords, setIncludeKeywords] = useState<string[]>(
    route?.include_keywords ?? [],
  );
  const [excludeKeywords, setExcludeKeywords] = useState<string[]>(
    route?.exclude_keywords ?? [],
  );
  const [includeRegex, setIncludeRegex] = useState(
    route?.include_regex ?? "",
  );
  const [excludeRegex, setExcludeRegex] = useState(
    route?.exclude_regex ?? "",
  );
  const [telegramMode, setTelegramMode] = useState<TelegramDeliveryMode>(
    route?.options.telegram_mode ?? "rich_message",
  );
  const [linkPreview, setLinkPreview] = useState<boolean>(
    route?.options.link_preview ?? true,
  );
  const [deliveryLocale, setDeliveryLocale] = useState<SystemLocale>(
    route ? (route.options.locale ?? legacyLocale ?? uiLocale) : uiLocale,
  );
  const [activeWindows, setActiveWindows] = useState<ActiveWindow[]>(
    route?.options.active_windows?.map((w) => ({ ...w })) ?? [],
  );
  const [activeWeekdays, setActiveWeekdays] = useState<number[]>(
    route?.options.active_weekdays ?? [],
  );
  const [enabled, setEnabled] = useState<boolean>(route?.enabled ?? true);
  const [formError, setFormError] = useState<string | null>(null);

  const selectedChannel = channels.find((c) => c.id === channelId);

  const telegramModes: Array<{
    id: TelegramDeliveryMode;
    label: string;
    summary: string;
    icon: typeof FileText;
  }> = [
    {
      id: "rich_message",
      label: m.routes.modes.rich_message.label,
      summary: m.routes.modes.rich_message.summary,
      icon: FileText,
    },
    {
      id: "compact_card",
      label: m.routes.modes.compact_card.label,
      summary: m.routes.modes.compact_card.summary,
      icon: MessageSquareText,
    },
    {
      id: "media_mode",
      label: m.routes.modes.media_mode.label,
      summary: m.routes.modes.media_mode.summary,
      icon: Image,
    },
  ];

  const toggleFeedSelection = (id: number) => {
    setFeedIds((prev) =>
      prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id],
    );
  };

  const saveMutation = useMutation({
    mutationFn: async () => {
      const options: RouteOptions = {
        ...(selectedChannel?.kind === "telegram"
          ? {
              telegram_mode: telegramMode,
              link_preview:
                telegramMode === "compact_card" ? linkPreview : undefined,
            }
          : {}),
        locale: deliveryLocale,
        active_windows: activeWindows,
        active_weekdays: [...activeWeekdays].sort((a, b) => a - b),
      };

      if (isEdit && route) {
        const payload: UpdateRouteRequest = {
          name: name.trim(),
          feed_ids: feedIds,
          match_tags: matchTags,
          channel_id: channelId,
          include_keywords: includeKeywords,
          exclude_keywords: excludeKeywords,
          include_regex: includeRegex.trim() || null,
          exclude_regex: excludeRegex.trim() || null,
          options,
          enabled,
        };
        return api.routes.update(route.id, payload);
      }

      const payload: CreateRouteRequest = {
        name: name.trim(),
        feed_ids: feedIds,
        match_tags: matchTags,
        channel_id: channelId,
        include_keywords: includeKeywords,
        exclude_keywords: excludeKeywords,
        include_regex: includeRegex.trim() || null,
        exclude_regex: excludeRegex.trim() || null,
        options,
        enabled,
      };
      return api.routes.create(payload);
    },
    onSuccess: () => {
      toast.success(isEdit ? m.routes.toastUpdated : m.routes.toastCreated);
      void queryClient.invalidateQueries({ queryKey: ["routes"] });
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
      setFormError(m.routes.editor.errEmptyName);
      return;
    }
    if (!channelId) {
      setFormError(m.routes.editor.errNoChannel);
      return;
    }
    if (feedIds.length === 0 && matchTags.length === 0) {
      setFormError(m.routes.editor.errNoSource);
      return;
    }
    if (
      activeWindows.some((w) => !TIME_RE.test(w.start) || !TIME_RE.test(w.end))
    ) {
      setFormError(m.routes.editor.errWindowTime);
      return;
    }
    const incErr = validateRegex(includeRegex);
    if (incErr) {
      setFormError(m.routes.editor.errIncludeRegex(incErr));
      return;
    }
    const excErr = validateRegex(excludeRegex);
    if (excErr) {
      setFormError(m.routes.editor.errExcludeRegex(excErr));
      return;
    }

    saveMutation.mutate();
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={
        isEdit
          ? m.routes.editor.editTitle(route?.name ?? "")
          : m.routes.editor.newTitle
      }
      description={m.routes.editor.desc}
      size="xl"
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            {m.common.cancel}
          </Button>
          <Button onClick={handleSubmit} loading={saveMutation.isPending}>
            {isEdit ? m.routes.editor.saveRoute : m.routes.editor.createRoute}
          </Button>
        </>
      }
    >
      <form onSubmit={handleSubmit} className="space-y-5">
        <div className="grid gap-4 sm:grid-cols-2">
          <div>
            <Label htmlFor="route-name">{m.routes.editor.nameLabel}</Label>
            <Input
              id="route-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={m.routes.editor.namePlaceholder}
              className="mt-1.5"
            />
          </div>

          <div>
            <Label htmlFor="route-channel">{m.routes.editor.channelLabel}</Label>
            <Select
              id="route-channel"
              value={channelId || ""}
              onChange={(e) => setChannelId(Number(e.target.value))}
              className="mt-1.5"
            >
              {channels.length === 0 ? (
                <option value="">{m.routes.editor.noChannelsOption}</option>
              ) : (
                channels.map((ch) => (
                  <option key={ch.id} value={ch.id}>
                    {ch.name} ({ch.kind.toUpperCase()})
                  </option>
                ))
              )}
            </Select>
          </div>
        </div>

        {/* Source Selector */}
        <div className="space-y-3 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
          <div className="flex items-center justify-between">
            <h3 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase">
              {m.routes.editor.sourceHeading}
            </h3>
            {feeds.length > 0 && (
              <div className="flex items-center gap-2 text-xs">
                <button
                  type="button"
                  onClick={() => setFeedIds(feeds.map((f) => f.id))}
                  className="cursor-pointer text-zinc-600 hover:underline dark:text-zinc-300"
                >
                  {m.routes.editor.selectAll(feeds.length)}
                </button>
                <span className="text-zinc-300">·</span>
                <button
                  type="button"
                  onClick={() => setFeedIds([])}
                  className="cursor-pointer text-zinc-500 hover:underline"
                >
                  {m.common.clear}
                </button>
              </div>
            )}
          </div>

          <div>
            <Label>{m.routes.editor.matchTagsLabel}</Label>
            <div className="mt-1">
              <TagInput
                values={matchTags}
                onChange={setMatchTags}
                suggestions={knownTags}
                placeholder={m.routes.editor.matchTagsPlaceholder}
              />
            </div>
          </div>

          <div>
            <Label>{m.routes.editor.explicitFeedsLabel(feedIds.length)}</Label>
            {feeds.length === 0 ? (
              <p className="mt-1 text-xs text-zinc-400">
                {m.routes.editor.noFeedsHint}
              </p>
            ) : (
              <div className="mt-1.5 grid max-h-36 gap-1.5 overflow-y-auto rounded-lg border border-zinc-200 bg-zinc-50/50 p-2 sm:grid-cols-2 dark:border-zinc-800 dark:bg-zinc-950">
                {feeds.map((f) => {
                  const checked = feedIds.includes(f.id);
                  return (
                    <label
                      key={f.id}
                      className={`flex cursor-pointer items-center gap-2 rounded-md px-2.5 py-1.5 text-xs ${
                        checked
                          ? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
                          : "hover:bg-zinc-100 dark:hover:bg-zinc-900"
                      }`}
                    >
                      <input
                        type="checkbox"
                        checked={checked}
                        onChange={() => toggleFeedSelection(f.id)}
                        className="accent-emerald-500"
                      />
                      <span className="truncate font-medium">
                        {f.title || f.url}
                      </span>
                    </label>
                  );
                })}
              </div>
            )}
          </div>
        </div>

        {/* Telegram Presentation Options (3 Modes) */}
        {selectedChannel?.kind === "telegram" && (
          <div className="space-y-3 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
            <h3 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase">
              {m.routes.editor.tgHeading}
            </h3>

            {(selectedChannel.capabilities.supports_rich_message === false ||
              Boolean(selectedChannel.capabilities.fallback_reason)) &&
              telegramMode === "rich_message" && (
                <div className="flex items-start gap-2 rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-200">
                  <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
                  <span>{m.routes.editor.tgFallbackNotice}</span>
                </div>
              )}

            <div className="grid gap-2.5 sm:grid-cols-3">
              {telegramModes.map((modeOpt) => {
                const Icon = modeOpt.icon;
                const active = telegramMode === modeOpt.id;
                return (
                  <button
                    key={modeOpt.id}
                    type="button"
                    onClick={() => setTelegramMode(modeOpt.id)}
                    className={`flex cursor-pointer flex-col justify-between rounded-lg border p-3 text-left transition-colors ${
                      active
                        ? "border-zinc-900 bg-zinc-900 text-white dark:border-zinc-100 dark:bg-zinc-100 dark:text-zinc-900"
                        : "border-zinc-200 bg-white hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-950"
                    }`}
                  >
                    <div className="flex items-center gap-2">
                      <Icon className="h-4 w-4 shrink-0" />
                      <span className="text-xs font-semibold">
                        {modeOpt.label}
                      </span>
                    </div>
                    <p
                      className={`mt-2 text-[11px] leading-snug ${
                        active
                          ? "text-zinc-300 dark:text-zinc-600"
                          : "text-zinc-500 dark:text-zinc-400"
                      }`}
                    >
                      {modeOpt.summary}
                    </p>
                  </button>
                );
              })}
            </div>

            {telegramMode === "compact_card" && (
              <div className="rounded-lg border border-zinc-200/80 bg-zinc-50/70 p-3 dark:border-zinc-800 dark:bg-zinc-950/60">
                <Switch
                  id="tg-link-preview"
                  checked={linkPreview}
                  onCheckedChange={setLinkPreview}
                  label={m.routes.editor.linkPreviewLabel}
                  description={m.routes.editor.linkPreviewDesc}
                />
              </div>
            )}
          </div>
        )}

        {/* Delivery Language */}
        <div className="space-y-3 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
          <h3 className="flex items-center gap-1.5 text-xs font-semibold tracking-wider text-zinc-500 uppercase">
            <Languages className="h-3.5 w-3.5" />
            {m.routes.editor.languageHeading}
          </h3>
          <div className="grid gap-2.5 sm:grid-cols-2">
            {(["zh-CN", "en"] as const).map((loc) => {
              const active = deliveryLocale === loc;
              return (
                <button
                  key={loc}
                  type="button"
                  onClick={() => setDeliveryLocale(loc)}
                  className={`flex cursor-pointer items-center justify-between rounded-lg border px-3.5 py-2 text-left transition-colors ${
                    active
                      ? "border-zinc-900 bg-zinc-900 text-white dark:border-zinc-100 dark:bg-zinc-100 dark:text-zinc-900"
                      : "border-zinc-200 bg-white text-zinc-700 hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-950 dark:text-zinc-300 dark:hover:bg-zinc-800/70"
                  }`}
                >
                  <span className="text-xs font-semibold">
                    {localeName(loc)}
                  </span>
                  <span
                    className={`font-mono text-[11px] ${
                      active
                        ? "text-zinc-300 dark:text-zinc-600"
                        : "text-zinc-500"
                    }`}
                  >
                    {loc}
                  </span>
                </button>
              );
            })}
          </div>
          <FieldHint>{m.routes.editor.languageHint}</FieldHint>
        </div>

        {/* Active Windows */}
        <div className="space-y-3 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <h3 className="flex items-center gap-1.5 text-xs font-semibold tracking-wider text-zinc-500 uppercase">
              <Clock className="h-3.5 w-3.5" />
              {m.routes.editor.windowsHeading}
            </h3>
            <Badge variant="mono">
              {m.routes.editor.timezoneBadge(timezone ?? "—")}
            </Badge>
          </div>
          <FieldHint className="mt-0">
            {m.routes.editor.windowsHint(timezone ?? "—")}
          </FieldHint>

          {activeWindows.length === 0 ? (
            <p className="rounded-lg border border-dashed border-zinc-200 px-3 py-2 text-xs text-zinc-500 dark:border-zinc-800 dark:text-zinc-400">
              {m.routes.editor.windowsAlways}
            </p>
          ) : (
            <div className="space-y-2">
              {activeWindows.map((w, idx) => (
                <div
                  // biome-ignore lint/suspicious/noArrayIndexKey: rows are positional and editable
                  key={idx}
                  className="flex flex-wrap items-center gap-2"
                >
                  <Input
                    type="time"
                    value={w.start}
                    onChange={(e) =>
                      setActiveWindows((prev) =>
                        prev.map((x, i) =>
                          i === idx ? { ...x, start: e.target.value } : x,
                        ),
                      )
                    }
                    aria-label={m.routes.editor.windowStart}
                    className="w-32 font-mono"
                  />
                  <span className="text-xs text-zinc-400">–</span>
                  <Input
                    type="time"
                    value={w.end}
                    onChange={(e) =>
                      setActiveWindows((prev) =>
                        prev.map((x, i) =>
                          i === idx ? { ...x, end: e.target.value } : x,
                        ),
                      )
                    }
                    aria-label={m.routes.editor.windowEnd}
                    className="w-32 font-mono"
                  />
                  {w.start && w.start === w.end ? (
                    <Badge variant="info">{m.routes.allDay}</Badge>
                  ) : (
                    w.start &&
                    w.end &&
                    w.end < w.start && (
                      <Badge variant="info">
                        {m.routes.editor.windowWraps}
                      </Badge>
                    )
                  )}
                  <Button
                    variant="ghost"
                    size="icon"
                    onClick={() =>
                      setActiveWindows((prev) =>
                        prev.filter((_, i) => i !== idx),
                      )
                    }
                    title={m.routes.editor.removeWindow}
                    aria-label={m.routes.editor.removeWindow}
                    className="text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-950/40"
                  >
                    <Trash2 className="h-4 w-4" />
                  </Button>
                </div>
              ))}
            </div>
          )}

          <Button
            variant="outline"
            size="sm"
            onClick={() =>
              setActiveWindows((prev) => [
                ...prev,
                { start: "09:00", end: "18:00" },
              ])
            }
          >
            <Plus className="h-3.5 w-3.5" />
            {m.routes.editor.addWindow}
          </Button>

          <div className="space-y-1.5 border-t border-zinc-100 pt-3 dark:border-zinc-800">
            <Label>{m.routes.editor.weekdaysLabel}</Label>
            <div className="flex flex-wrap gap-1.5">
              {WEEKDAYS.map((d) => {
                const active = activeWeekdays.includes(d);
                return (
                  <button
                    key={d}
                    type="button"
                    aria-pressed={active}
                    onClick={() =>
                      setActiveWeekdays((prev) =>
                        prev.includes(d)
                          ? prev.filter((x) => x !== d)
                          : [...prev, d],
                      )
                    }
                    className={`min-w-11 cursor-pointer rounded-md border px-2.5 py-1 text-xs font-medium transition-colors ${
                      active
                        ? "border-zinc-900 bg-zinc-900 text-white dark:border-zinc-100 dark:bg-zinc-100 dark:text-zinc-900"
                        : "border-zinc-200 bg-white text-zinc-600 hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-950 dark:text-zinc-400 dark:hover:bg-zinc-800/70"
                    }`}
                  >
                    {m.routes.weekdaysShort[d - 1]}
                  </button>
                );
              })}
            </div>
            <FieldHint>
              {activeWeekdays.length === 0
                ? m.routes.editor.weekdaysEveryDay
                : m.routes.editor.weekdaysHint}
            </FieldHint>
          </div>
          <FieldHint>{m.routes.editor.queuedOutsideHint}</FieldHint>
        </div>

        {/* Content Filter Rules */}
        <div className="space-y-3 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
          <h3 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase">
            {m.routes.editor.filtersHeading}
          </h3>
          <div className="grid gap-3 sm:grid-cols-2">
            <div>
              <Label>{m.routes.editor.includeKeywordsLabel}</Label>
              <div className="mt-1">
                <TagInput
                  values={includeKeywords}
                  onChange={setIncludeKeywords}
                  placeholder={m.routes.editor.includeKeywordsPlaceholder}
                />
              </div>
            </div>
            <div>
              <Label>{m.routes.editor.excludeKeywordsLabel}</Label>
              <div className="mt-1">
                <TagInput
                  values={excludeKeywords}
                  onChange={setExcludeKeywords}
                  placeholder={m.routes.editor.excludeKeywordsPlaceholder}
                />
              </div>
            </div>
            <div>
              <Label>{m.routes.editor.includeRegexLabel}</Label>
              <Input
                value={includeRegex}
                onChange={(e) => setIncludeRegex(e.target.value)}
                placeholder="(?i)\b(release|cve-\d+)\b"
                className="mt-1 font-mono text-xs"
              />
              <FieldHint>{m.routes.editor.regexHint}</FieldHint>
            </div>
            <div>
              <Label>{m.routes.editor.excludeRegexLabel}</Label>
              <Input
                value={excludeRegex}
                onChange={(e) => setExcludeRegex(e.target.value)}
                placeholder="(?i)\b(digest|advertisement)\b"
                className="mt-1 font-mono text-xs"
              />
            </div>
          </div>
        </div>

        <div className="rounded-lg border border-zinc-200/80 bg-zinc-50/60 p-3.5 dark:border-zinc-800 dark:bg-zinc-950/60">
          <Switch
            id="route-enabled"
            checked={enabled}
            onCheckedChange={setEnabled}
            label={m.routes.editor.activeLabel}
            description={m.routes.editor.activeDesc}
          />
        </div>

        <FieldError message={formError} />
      </form>
    </Modal>
  );
}
