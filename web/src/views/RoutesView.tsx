import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  ArrowRight,
  FileText,
  Filter,
  GitFork,
  Image,
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
  FeedDto,
  RouteDto,
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
import { errorMessage, validateRegex } from "@/lib/utils";

const TELEGRAM_MODES: Array<{
  id: TelegramDeliveryMode;
  label: string;
  summary: string;
  icon: typeof FileText;
}> = [
  {
    id: "rich_message",
    label: "Rich Message (Bot API 10.4)",
    summary:
      "Calls sendRichMessage via telegram-rich Rich HTML normalizer with Document::fit block-boundary truncation + Read Original button.",
    icon: FileText,
  },
  {
    id: "compact_card",
    label: "Compact Card",
    summary:
      "Calls sendMessage (4096-char safe truncation): bold title + source tag + distilled summary + canonical link.",
    icon: MessageSquareText,
  },
  {
    id: "media_mode",
    label: "Media Mode",
    summary:
      "Extracts all media + 3-line caption (Author / 1st Unicode sentence / URL) with anti-hotlink stream relay & 10-item sendMediaGroup checkpointing.",
    icon: Image,
  },
];

function telegramModeLabel(mode?: TelegramDeliveryMode): string {
  switch (mode) {
    case "compact_card":
      return "Compact Card";
    case "media_mode":
      return "Media Mode";
    default:
      return "Rich Message";
  }
}

export function RoutesView() {
  const toast = useToast();
  const queryClient = useQueryClient();

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
      toast.error("Failed to toggle route", errorMessage(err));
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (id: number) => api.routes.remove(id),
    onSuccess: () => {
      toast.success("Route deleted");
      setDeletingRoute(null);
      void queryClient.invalidateQueries({ queryKey: ["routes"] });
    },
    onError: (err) => {
      toast.error("Failed to delete route", errorMessage(err));
    },
  });

  return (
    <div className="space-y-5">
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-lg font-bold tracking-tight">
            Routing & Presentation Rules
          </h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400">
            Bind feed sets or tag groups to trusted channels with keyword/regex filters and per-route Telegram presentation modes.
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
          New Route
        </Button>
      </div>

      {/* Routes List */}
      {routesQuery.isLoading ? (
        <Card className="p-8 text-center text-xs text-zinc-500">
          Loading routing rules…
        </Card>
      ) : (routesQuery.data?.length ?? 0) === 0 ? (
        <EmptyState
          icon={GitFork}
          title="No delivery routes configured"
          description="Create a route to connect subscriptions (by explicit feed IDs or #tags) to a trusted SMTP, Telegram, or Notion destination."
          action={
            <Button
              size="sm"
              onClick={() => {
                setEditingRoute(null);
                setEditorOpen(true);
              }}
            >
              <Plus className="h-3.5 w-3.5" />
              Create First Route
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
                        <Badge variant="default">Disabled</Badge>
                      )}
                      {targetChannel?.kind === "telegram" && (
                        <Badge variant="info">
                          {telegramModeLabel(route.options.telegram_mode)}
                          {route.options.telegram_mode === "compact_card" &&
                            ` · Preview ${route.options.link_preview === false ? "OFF" : "ON"}`}
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
                              {f?.title || f?.url || `Feed #${fid}`}
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

                      <Badge
                        variant={targetChannel ? "success" : "danger"}
                      >
                        {targetChannel
                          ? `${targetChannel.name} (${targetChannel.kind.toUpperCase()})`
                          : `Channel #${route.channel_id}`}
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
                      Edit
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => setDeletingRoute(route)}
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
        />
      )}

      {/* Delete Confirmation */}
      <ConfirmDialog
        open={deletingRoute !== null}
        onClose={() => setDeletingRoute(null)}
        onConfirm={() => {
          if (deletingRoute) deleteMutation.mutate(deletingRoute.id);
        }}
        title="Delete Delivery Route"
        description={`Delete route "${deletingRoute?.name}"? Pending delivery tasks generated for this route will also be removed.`}
        confirmLabel="Delete Route"
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
}: {
  open: boolean;
  onClose: () => void;
  route: RouteDto | null;
  feeds: FeedDto[];
  channels: ChannelDto[];
  knownTags: string[];
}) {
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
  const [enabled, setEnabled] = useState<boolean>(route?.enabled ?? true);
  const [formError, setFormError] = useState<string | null>(null);

  const selectedChannel = channels.find((c) => c.id === channelId);

  const toggleFeedSelection = (id: number) => {
    setFeedIds((prev) =>
      prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id],
    );
  };

  const saveMutation = useMutation({
    mutationFn: async () => {
      const options =
        selectedChannel?.kind === "telegram"
          ? {
              telegram_mode: telegramMode,
              link_preview:
                telegramMode === "compact_card" ? linkPreview : undefined,
            }
          : {};

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
      toast.success(isEdit ? "Route updated" : "Route created");
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
      setFormError("Route name is required.");
      return;
    }
    if (!channelId) {
      setFormError("Select a target trusted channel first.");
      return;
    }
    if (feedIds.length === 0 && matchTags.length === 0) {
      setFormError(
        "Select at least one explicit feed or one #tag for the source selector.",
      );
      return;
    }
    const incErr = validateRegex(includeRegex);
    if (incErr) {
      setFormError(`Include regex syntax error: ${incErr}`);
      return;
    }
    const excErr = validateRegex(excludeRegex);
    if (excErr) {
      setFormError(`Exclude regex syntax error: ${excErr}`);
      return;
    }

    saveMutation.mutate();
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={isEdit ? `Edit Route: ${route?.name}` : "New Delivery Route"}
      description="Each matched article generates a dedicated transactional outbox task (1 Task = 1 Entry × 1 Route)."
      size="xl"
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button onClick={handleSubmit} loading={saveMutation.isPending}>
            {isEdit ? "Save Route" : "Create Route"}
          </Button>
        </>
      }
    >
      <form onSubmit={handleSubmit} className="space-y-5">
        <div className="grid gap-4 sm:grid-cols-2">
          <div>
            <Label htmlFor="route-name">Route Name</Label>
            <Input
              id="route-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Rust & Systems → Telegram Rich"
              className="mt-1.5"
            />
          </div>

          <div>
            <Label htmlFor="route-channel">Target Trusted Channel</Label>
            <Select
              id="route-channel"
              value={channelId || ""}
              onChange={(e) => setChannelId(Number(e.target.value))}
              className="mt-1.5"
            >
              {channels.length === 0 ? (
                <option value="">— Configure a channel first —</option>
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
              Source Selector (Feed IDs OR Tag Match)
            </h3>
            {feeds.length > 0 && (
              <div className="flex items-center gap-2 text-xs">
                <button
                  type="button"
                  onClick={() => setFeedIds(feeds.map((f) => f.id))}
                  className="cursor-pointer text-zinc-600 hover:underline dark:text-zinc-300"
                >
                  Select all ({feeds.length})
                </button>
                <span className="text-zinc-300">·</span>
                <button
                  type="button"
                  onClick={() => setFeedIds([])}
                  className="cursor-pointer text-zinc-500 hover:underline"
                >
                  Clear
                </button>
              </div>
            )}
          </div>

          <div>
            <Label>Match Tags</Label>
            <div className="mt-1">
              <TagInput
                values={matchTags}
                onChange={setMatchTags}
                suggestions={knownTags}
                placeholder="Match any feed carrying these tags…"
              />
            </div>
          </div>

          <div>
            <Label>Explicit Subscriptions ({feedIds.length} selected)</Label>
            {feeds.length === 0 ? (
              <p className="mt-1 text-xs text-zinc-400">
                No feeds added yet — you can still pre-bind by #tag above.
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
              Telegram Presentation Mode (Bot API 10.4)
            </h3>

            {(selectedChannel.capabilities.supports_rich_message === false ||
              Boolean(selectedChannel.capabilities.fallback_reason)) &&
              telegramMode === "rich_message" && (
                <div className="flex items-start gap-2 rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-200">
                  <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
                  <span>
                    Target Bot API endpoint reported{" "}
                    <code className="font-mono">sendRichMessage</code>{" "}
                    unsupported; runtime will gracefully downgrade to Compact Card until upgraded.
                  </span>
                </div>
              )}

            <div className="grid gap-2.5 sm:grid-cols-3">
              {TELEGRAM_MODES.map((m) => {
                const Icon = m.icon;
                const active = telegramMode === m.id;
                return (
                  <button
                    key={m.id}
                    type="button"
                    onClick={() => setTelegramMode(m.id)}
                    className={`flex cursor-pointer flex-col justify-between rounded-lg border p-3 text-left transition-colors ${
                      active
                        ? "border-zinc-900 bg-zinc-900 text-white dark:border-zinc-100 dark:bg-zinc-100 dark:text-zinc-900"
                        : "border-zinc-200 bg-white hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-950"
                    }`}
                  >
                    <div className="flex items-center gap-2">
                      <Icon className="h-4 w-4 shrink-0" />
                      <span className="text-xs font-semibold">{m.label}</span>
                    </div>
                    <p
                      className={`mt-2 text-[11px] leading-snug ${
                        active
                          ? "text-zinc-300 dark:text-zinc-600"
                          : "text-zinc-500 dark:text-zinc-400"
                      }`}
                    >
                      {m.summary}
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
                  label="Enable Telegram Link Preview (LinkPreviewOptions)"
                  description="Allow Telegram to render an OpenGraph webpage preview card beneath the compact message."
                />
              </div>
            )}
          </div>
        )}

        {/* Content Filter Rules */}
        <div className="space-y-3 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
          <h3 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase">
            Keyword & Regex Filters (Title + Cleaned Body)
          </h3>
          <div className="grid gap-3 sm:grid-cols-2">
            <div>
              <Label>Include Keywords (Match Any)</Label>
              <div className="mt-1">
                <TagInput
                  values={includeKeywords}
                  onChange={setIncludeKeywords}
                  placeholder="Leave empty to allow all…"
                />
              </div>
            </div>
            <div>
              <Label>Exclude Keywords (Block Any)</Label>
              <div className="mt-1">
                <TagInput
                  values={excludeKeywords}
                  onChange={setExcludeKeywords}
                  placeholder="Sponsored, Hiring…"
                />
              </div>
            </div>
            <div>
              <Label>Include Regex (Optional)</Label>
              <Input
                value={includeRegex}
                onChange={(e) => setIncludeRegex(e.target.value)}
                placeholder="(?i)\b(release|cve-\d+)\b"
                className="mt-1 font-mono text-xs"
              />
              <FieldHint>Evaluated via Rust regex crate (RE2-safe).</FieldHint>
            </div>
            <div>
              <Label>Exclude Regex (Optional)</Label>
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
            label="Route Active"
            description="When disabled, newly discovered feed items will not enqueue tasks for this route."
          />
        </div>

        <FieldError message={formError} />
      </form>
    </Modal>
  );
}
