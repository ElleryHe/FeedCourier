import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  BookOpen,
  Check,
  Download,
  Eraser,
  ExternalLink,
  Eye,
  FileCode2,
  Filter,
  Globe,
  Pause,
  Pencil,
  Play,
  Plus,
  RefreshCw,
  Rss,
  Search,
  Send,
  ShieldAlert,
  Trash2,
  Upload,
  X,
} from "lucide-react";
import { type ChangeEvent, type FormEvent, useMemo, useState } from "react";
import { api } from "@/api/client";
import type {
  ContentFilterRules,
  CreateFeedRequest,
  FeedDto,
  FeedHealthStatus,
  FeedPreviewResponse,
  SystemLocale,
  TelegramDeliveryMode,
  UpdateFeedRequest,
} from "@/api/types";
import {
  Badge,
  type BadgeVariant,
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
  Textarea,
} from "@/components/ui/primitives";
import { useToast } from "@/components/ui/toast";
import { type Locale, useI18n } from "@/i18n";
import type { Messages } from "@/i18n/en";
import {
  errorMessage,
  formatDateTime,
  formatDurationSecs,
  formatRelativeTime,
  isValidHttpUrl,
  validateRegex,
} from "@/lib/utils";

export interface FeedsViewProps {
  initialStatusFilter?: FeedHealthStatus | "all";
}

const POLL_PRESETS = [
  { label: "5m", secs: 300 },
  { label: "15m", secs: 900 },
  { label: "30m", secs: 1800 },
  { label: "1h", secs: 3600 },
  { label: "6h", secs: 21600 },
];

function healthBadge(
  status: FeedHealthStatus,
  m: Messages,
): {
  label: string;
  variant: BadgeVariant;
} {
  switch (status) {
    case "active":
      return { label: m.status.feed.active, variant: "success" };
    case "backoff":
      return { label: m.status.feed.backoff, variant: "warning" };
    case "circuit_open":
      return { label: m.status.feed.circuit_open, variant: "danger" };
    case "paused":
      return { label: m.status.feed.paused, variant: "default" };
  }
}

export function FeedsView({ initialStatusFilter = "all" }: FeedsViewProps) {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();

  const [statusFilter, setStatusFilter] = useState<FeedHealthStatus | "all">(
    initialStatusFilter,
  );
  const [searchQuery, setSearchQuery] = useState("");

  // Modals state
  const [editorOpen, setEditorOpen] = useState(false);
  const [editingFeed, setEditingFeed] = useState<FeedDto | null>(null);
  const [deletingFeed, setDeletingFeed] = useState<FeedDto | null>(null);
  const [opmlOpen, setOpmlOpen] = useState(false);
  const [previewOpen, setPreviewOpen] = useState(false);
  const [previewInitialUrl, setPreviewInitialUrl] = useState("");
  const [previewInitialReadability, setPreviewInitialReadability] =
    useState(false);
  const [previewInitialProxy, setPreviewInitialProxy] = useState(false);
  const [previewFeedId, setPreviewFeedId] = useState<number | null>(null);
  const [clearingFeed, setClearingFeed] = useState<FeedDto | null>(null);
  const [pollingFeedId, setPollingFeedId] = useState<number | null>(null);

  const feedsQuery = useQuery({
    queryKey: ["feeds"],
    queryFn: () => api.feeds.list(),
    refetchInterval: 30_000,
  });

  const allKnownTags = useMemo(() => {
    const set = new Set<string>();
    for (const feed of feedsQuery.data ?? []) {
      for (const tag of feed.tags) set.add(tag);
    }
    return Array.from(set).sort();
  }, [feedsQuery.data]);

  const filteredFeeds = useMemo(() => {
    const q = searchQuery.trim().toLowerCase();
    return (feedsQuery.data ?? []).filter((feed) => {
      if (statusFilter !== "all" && feed.health_status !== statusFilter) {
        return false;
      }
      if (!q) return true;
      const inTitle = (feed.title ?? "").toLowerCase().includes(q);
      const inUrl = feed.url.toLowerCase().includes(q);
      const inTags = feed.tags.some((t) => t.toLowerCase().includes(q));
      return inTitle || inUrl || inTags;
    });
  }, [feedsQuery.data, statusFilter, searchQuery]);

  const pollMutation = useMutation({
    mutationFn: (id: number) => {
      setPollingFeedId(id);
      return api.feeds.pollNow(id);
    },
    onSuccess: (res) => {
      toast.success(
        res.not_modified
          ? m.feeds.toastNotModifiedTitle
          : m.feeds.toastPolledTitle,
        res.not_modified
          ? m.feeds.toastNotModifiedDesc
          : m.feeds.toastPolledDesc(res.new_entries, res.enqueued_tasks),
      );
      void queryClient.invalidateQueries({ queryKey: ["feeds"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
    },
    onError: (err) => {
      toast.error(m.feeds.toastPollFailed, errorMessage(err));
    },
    onSettled: () => {
      setPollingFeedId(null);
    },
  });

  const quickPatchMutation = useMutation({
    mutationFn: ({ id, patch }: { id: number; patch: UpdateFeedRequest }) =>
      api.feeds.update(id, patch),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["feeds"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
    },
    onError: (err) => {
      toast.error(m.feeds.toastUpdateFailed, errorMessage(err));
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (id: number) => api.feeds.remove(id),
    onSuccess: () => {
      toast.success(m.feeds.toastRemoved);
      setDeletingFeed(null);
      void queryClient.invalidateQueries({ queryKey: ["feeds"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
    },
    onError: (err) => {
      toast.error(m.feeds.toastDeleteFailed, errorMessage(err));
    },
  });

  const clearCacheMutation = useMutation({
    mutationFn: ({ id, pollNow }: { id: number; pollNow: boolean }) =>
      api.feeds.clearCache(id, { poll_now: pollNow }),
    onSuccess: (res) => {
      toast.success(
        m.feeds.clearCache.toastDoneTitle,
        res.poll
          ? m.feeds.clearCache.toastDoneDescPolled(
              res.deleted_entries,
              res.poll.new_entries,
              res.poll.enqueued_tasks,
            )
          : m.feeds.clearCache.toastDoneDesc(res.deleted_entries),
      );
      setClearingFeed(null);
      void queryClient.invalidateQueries({ queryKey: ["feeds"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
      void queryClient.invalidateQueries({ queryKey: ["tasks"] });
    },
    onError: (err) => {
      toast.error(m.feeds.clearCache.toastFailed, errorMessage(err));
    },
  });

  const exportOpmlMutation = useMutation({
    mutationFn: () => api.feeds.exportOpml(),
    onSuccess: (filename) => {
      toast.success(
        m.feeds.toastOpmlExported,
        m.feeds.toastOpmlExportedDesc(filename),
      );
    },
    onError: (err) => {
      toast.error(m.feeds.toastOpmlExportFailed, errorMessage(err));
    },
  });

  const openCreateModal = (prefill?: {
    url: string;
    fetch_full_content: boolean;
    use_proxy: boolean;
  }) => {
    setEditingFeed(null);
    if (prefill) {
      setPreviewInitialUrl(prefill.url);
      setPreviewInitialReadability(prefill.fetch_full_content);
      setPreviewInitialProxy(prefill.use_proxy);
    } else {
      setPreviewInitialUrl("");
      setPreviewInitialReadability(false);
      setPreviewInitialProxy(false);
    }
    setEditorOpen(true);
  };

  const openPreviewModal = (feed?: {
    url: string;
    fetch_full_content: boolean;
    use_proxy: boolean;
    feed_id?: number;
  }) => {
    setPreviewFeedId(feed?.feed_id ?? null);
    setPreviewInitialUrl(feed?.url ?? "");
    setPreviewInitialReadability(feed?.fetch_full_content ?? false);
    setPreviewInitialProxy(feed?.use_proxy ?? false);
    setPreviewOpen(true);
  };

  return (
    <div className="space-y-5">
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-lg font-bold tracking-tight">{m.feeds.title}</h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400">
            {m.feeds.subtitle}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => openPreviewModal()}
          >
            <Eye className="h-3.5 w-3.5" />
            {m.feeds.livePreview}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => setOpmlOpen(true)}
          >
            <Upload className="h-3.5 w-3.5" />
            {m.feeds.importOpml}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => exportOpmlMutation.mutate()}
            loading={exportOpmlMutation.isPending}
          >
            <Download className="h-3.5 w-3.5" />
            {m.feeds.exportOpml}
          </Button>
          <Button size="sm" onClick={() => openCreateModal()}>
            <Plus className="h-3.5 w-3.5" />
            {m.feeds.addFeed}
          </Button>
        </div>
      </div>

      {/* Filters & Search */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-1 rounded-lg border border-zinc-200 bg-white p-1 dark:border-zinc-800 dark:bg-zinc-900">
          {(
            [
              { id: "all", label: m.common.all },
              { id: "active", label: m.status.feed.active },
              { id: "backoff", label: m.status.feed.backoff },
              { id: "circuit_open", label: m.status.feed.circuit_open },
              { id: "paused", label: m.status.feed.paused },
            ] as const
          ).map((tab) => (
            <button
              key={tab.id}
              type="button"
              onClick={() => setStatusFilter(tab.id)}
              className={`cursor-pointer rounded-md px-2.5 py-1 text-xs font-medium transition-colors ${
                statusFilter === tab.id
                  ? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
                  : "text-zinc-600 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:bg-zinc-800"
              }`}
            >
              {tab.label}
            </button>
          ))}
        </div>

        <div className="relative w-full sm:w-72">
          <Search className="pointer-events-none absolute top-2.5 left-2.5 h-4 w-4 text-zinc-400" />
          <Input
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            placeholder={m.feeds.searchPlaceholder}
            className="pl-8"
          />
        </div>
      </div>

      {/* Feed List */}
      {feedsQuery.isLoading ? (
        <Card className="p-8 text-center text-xs text-zinc-500">
          {m.feeds.loading}
        </Card>
      ) : filteredFeeds.length === 0 ? (
        <EmptyState
          icon={Rss}
          title={
            (feedsQuery.data?.length ?? 0) === 0
              ? m.feeds.emptyTitle
              : m.feeds.emptyFilteredTitle
          }
          description={
            (feedsQuery.data?.length ?? 0) === 0
              ? m.feeds.emptyDesc
              : m.feeds.emptyFilteredDesc
          }
          action={
            (feedsQuery.data?.length ?? 0) === 0 ? (
              <div className="flex items-center gap-2">
                <Button size="sm" onClick={() => openCreateModal()}>
                  <Plus className="h-3.5 w-3.5" />
                  {m.feeds.addFirstFeed}
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setOpmlOpen(true)}
                >
                  <Upload className="h-3.5 w-3.5" />
                  {m.feeds.importOpml}
                </Button>
              </div>
            ) : undefined
          }
        />
      ) : (
        <div className="space-y-2.5">
          {filteredFeeds.map((feed) => {
            const badge = healthBadge(feed.health_status, m);
            const isPaused = feed.health_status === "paused";
            return (
              <Card key={feed.id} className="p-4">
                <div className="flex flex-col justify-between gap-3 lg:flex-row lg:items-center">
                  {/* Left Info */}
                  <div className="min-w-0 flex-1 space-y-1.5">
                    <div className="flex flex-wrap items-center gap-2">
                      {feed.favicon_url ? (
                        <img
                          src={feed.favicon_url}
                          alt=""
                          className="h-4 w-4 shrink-0 rounded-xs object-contain"
                          onError={(e) => {
                            e.currentTarget.style.display = "none";
                          }}
                        />
                      ) : (
                        <Rss className="h-4 w-4 shrink-0 text-zinc-400" />
                      )}
                      <span className="truncate text-sm font-semibold text-zinc-900 dark:text-zinc-100">
                        {feed.title || feed.url}
                      </span>
                      <Badge variant={badge.variant}>{badge.label}</Badge>
                      {feed.consecutive_failures > 0 && (
                        <Badge variant="warning">
                          {m.feeds.failsBadge(feed.consecutive_failures)}
                        </Badge>
                      )}
                      {feed.fetch_full_content && (
                        <Badge variant="info">
                          <BookOpen className="h-3 w-3" />
                          {m.common.readability}
                        </Badge>
                      )}
                      {feed.use_proxy && (
                        <Badge variant="outline">
                          <Globe className="h-3 w-3" />
                          {m.common.proxy}
                        </Badge>
                      )}
                    </div>

                    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-zinc-500 dark:text-zinc-400">
                      <a
                        href={feed.url}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="inline-flex max-w-md items-center gap-1 truncate font-mono text-[11px] hover:text-zinc-800 hover:underline dark:hover:text-zinc-200"
                      >
                        {feed.url}
                        <ExternalLink className="h-2.5 w-2.5 shrink-0" />
                      </a>
                      <span>·</span>
                      <span>
                        {m.feeds.intervalLabel}{" "}
                        <strong className="font-mono font-medium text-zinc-700 dark:text-zinc-300">
                          {formatDurationSecs(feed.poll_interval_secs)}
                        </strong>
                      </span>
                      <span>·</span>
                      <span title={formatDateTime(feed.next_poll_at)}>
                        {m.feeds.nextPollLabel}{" "}
                        <strong className="font-medium text-zinc-700 dark:text-zinc-300">
                          {isPaused
                            ? m.feeds.pausedLower
                            : formatRelativeTime(feed.next_poll_at)}
                        </strong>
                      </span>
                    </div>

                    {feed.tags.length > 0 && (
                      <div className="flex flex-wrap items-center gap-1 pt-0.5">
                        {feed.tags.map((tag) => (
                          <Badge key={tag} variant="mono">
                            #{tag}
                          </Badge>
                        ))}
                      </div>
                    )}

                    {feed.last_error && (
                      <div className="mt-1 flex items-start gap-1.5 rounded-md border border-rose-200/80 bg-rose-50/60 px-2.5 py-1.5 text-xs text-rose-700 dark:border-rose-900/60 dark:bg-rose-950/30 dark:text-rose-300">
                        <ShieldAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                        <span className="font-mono text-[11px] break-all">
                          {feed.last_error}
                        </span>
                      </div>
                    )}
                  </div>

                  {/* Right Actions */}
                  <div className="flex shrink-0 flex-wrap items-center gap-1.5">
                    <Button
                      variant={
                        feed.health_status === "circuit_open"
                          ? "primary"
                          : "outline"
                      }
                      size="sm"
                      onClick={() => pollMutation.mutate(feed.id)}
                      loading={pollingFeedId === feed.id}
                      title={m.feeds.pollButtonTitle}
                    >
                      <RefreshCw className="h-3.5 w-3.5" />
                      {feed.health_status === "circuit_open"
                        ? m.feeds.resetAndPoll
                        : m.feeds.pollNow}
                    </Button>

                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() =>
                        quickPatchMutation.mutate({
                          id: feed.id,
                          patch: {
                            fetch_full_content: !feed.fetch_full_content,
                          },
                        })
                      }
                      title={m.feeds.fullTextToggleTitle}
                    >
                      <BookOpen className="h-3.5 w-3.5" />
                      {feed.fetch_full_content
                        ? m.feeds.fullTextOn
                        : m.feeds.fullTextOff}
                    </Button>

                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() =>
                        openPreviewModal({
                          url: feed.url,
                          fetch_full_content: feed.fetch_full_content,
                          use_proxy: feed.use_proxy,
                          feed_id: feed.id,
                        })
                      }
                      title={m.feeds.previewButtonTitle}
                      aria-label={m.feeds.previewButtonTitle}
                    >
                      <Eye className="h-4 w-4" />
                    </Button>

                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() =>
                        quickPatchMutation.mutate({
                          id: feed.id,
                          patch: {
                            health_status: isPaused ? "active" : "paused",
                          },
                        })
                      }
                      title={
                        isPaused
                          ? m.feeds.resumePollingTitle
                          : m.feeds.pausePollingTitle
                      }
                      aria-label={
                        isPaused
                          ? m.feeds.resumePollingTitle
                          : m.feeds.pausePollingTitle
                      }
                    >
                      {isPaused ? (
                        <Play className="h-4 w-4 text-emerald-600" />
                      ) : (
                        <Pause className="h-4 w-4" />
                      )}
                    </Button>

                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => setClearingFeed(feed)}
                      title={m.feeds.clearCache.buttonTitle}
                      aria-label={m.feeds.clearCache.buttonTitle}
                    >
                      <Eraser className="h-4 w-4" />
                    </Button>

                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => {
                        setEditingFeed(feed);
                        setEditorOpen(true);
                      }}
                      title={m.feeds.editFeedTitle}
                      aria-label={m.feeds.editFeedTitle}
                    >
                      <Pencil className="h-4 w-4" />
                    </Button>

                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => setDeletingFeed(feed)}
                      title={m.feeds.deleteFeedTitle}
                      aria-label={m.feeds.deleteFeedTitle}
                      className="text-rose-600 hover:bg-rose-50 hover:text-rose-700 dark:text-rose-400 dark:hover:bg-rose-950/40"
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

      {/* Create / Edit Feed Modal */}
      {editorOpen && (
        <FeedEditorModal
          open={editorOpen}
          onClose={() => setEditorOpen(false)}
          feed={editingFeed}
          knownTags={allKnownTags}
          prefillUrl={previewInitialUrl}
          prefillReadability={previewInitialReadability}
          prefillProxy={previewInitialProxy}
          onOpenPreview={(draft) => {
            setEditorOpen(false);
            openPreviewModal(draft);
          }}
        />
      )}

      {/* Live Preview & Filter Test Modal */}
      {previewOpen && (
        <FeedPreviewModal
          open={previewOpen}
          onClose={() => setPreviewOpen(false)}
          initialUrl={previewInitialUrl}
          initialFetchFullContent={previewInitialReadability}
          initialUseProxy={previewInitialProxy}
          feedId={previewFeedId}
          onSubscribe={(settings) => {
            setPreviewOpen(false);
            openCreateModal(settings);
          }}
        />
      )}

      {/* Clear Cache Dialog */}
      {clearingFeed && (
        <ClearCacheDialog
          feed={clearingFeed}
          loading={clearCacheMutation.isPending}
          onClose={() => setClearingFeed(null)}
          onConfirm={(pollNow) =>
            clearCacheMutation.mutate({ id: clearingFeed.id, pollNow })
          }
        />
      )}

      {/* OPML Import Modal */}
      {opmlOpen && (
        <OpmlImportModal open={opmlOpen} onClose={() => setOpmlOpen(false)} />
      )}

      {/* Delete Confirmation */}
      <ConfirmDialog
        open={deletingFeed !== null}
        onClose={() => setDeletingFeed(null)}
        onConfirm={() => {
          if (deletingFeed) deleteMutation.mutate(deletingFeed.id);
        }}
        title={m.feeds.deleteModalTitle}
        description={m.feeds.deleteModalDesc(
          deletingFeed?.title || deletingFeed?.url || "",
        )}
        confirmLabel={m.feeds.deleteModalConfirm}
        loading={deleteMutation.isPending}
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Sub-component: Feed Editor Modal
// ---------------------------------------------------------------------------

function FeedEditorModal({
  open,
  onClose,
  feed,
  knownTags,
  prefillUrl,
  prefillReadability,
  prefillProxy,
  onOpenPreview,
}: {
  open: boolean;
  onClose: () => void;
  feed: FeedDto | null;
  knownTags: string[];
  prefillUrl: string;
  prefillReadability: boolean;
  prefillProxy: boolean;
  onOpenPreview: (draft: {
    url: string;
    fetch_full_content: boolean;
    use_proxy: boolean;
    feed_id?: number;
  }) => void;
}) {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const isEdit = feed !== null;

  const [url, setUrl] = useState(feed?.url ?? prefillUrl);
  const [title, setTitle] = useState(feed?.title ?? "");
  const [tags, setTags] = useState<string[]>(feed?.tags ?? []);
  const [pollIntervalSecs, setPollIntervalSecs] = useState<number>(
    feed?.poll_interval_secs ?? 1800,
  );
  const [fetchFullContent, setFetchFullContent] = useState<boolean>(
    feed?.fetch_full_content ?? prefillReadability,
  );
  const [useProxy, setUseProxy] = useState<boolean>(
    feed?.use_proxy ?? prefillProxy,
  );
  const [silentInit, setSilentInit] = useState<boolean>(
    feed?.silent_init ?? true,
  );
  const [pushLatestOne, setPushLatestOne] = useState<boolean>(false);
  const [formError, setFormError] = useState<string | null>(null);

  const saveMutation = useMutation({
    mutationFn: async () => {
      if (isEdit && feed) {
        const payload: UpdateFeedRequest = {
          url: url.trim(),
          title: title.trim() || null,
          tags,
          poll_interval_secs: pollIntervalSecs,
          fetch_full_content: fetchFullContent,
          use_proxy: useProxy,
          silent_init: silentInit,
        };
        return api.feeds.update(feed.id, payload);
      }
      const payload: CreateFeedRequest = {
        url: url.trim(),
        title: title.trim() || null,
        tags,
        poll_interval_secs: pollIntervalSecs,
        fetch_full_content: fetchFullContent,
        use_proxy: useProxy,
        silent_init: silentInit,
        push_latest_one: silentInit ? pushLatestOne : undefined,
      };
      return api.feeds.create(payload);
    },
    onSuccess: () => {
      toast.success(isEdit ? m.feeds.toastUpdated : m.feeds.toastAdded);
      void queryClient.invalidateQueries({ queryKey: ["feeds"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
      onClose();
    },
    onError: (err) => {
      setFormError(errorMessage(err));
    },
  });

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    setFormError(null);
    if (!isValidHttpUrl(url)) {
      setFormError(m.feeds.editor.errInvalidUrl);
      return;
    }
    if (!Number.isFinite(pollIntervalSecs) || pollIntervalSecs < 60) {
      setFormError(m.feeds.editor.errMinInterval);
      return;
    }
    saveMutation.mutate();
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={isEdit ? m.feeds.editor.editTitle : m.feeds.editor.addTitle}
      description={m.feeds.editor.desc}
      footer={
        <>
          {isValidHttpUrl(url) && (
            <Button
              variant="ghost"
              onClick={() =>
                onOpenPreview({
                  url: url.trim(),
                  fetch_full_content: fetchFullContent,
                  use_proxy: useProxy,
                  feed_id:
                    feed && feed.url === url.trim() ? feed.id : undefined,
                })
              }
              className="mr-auto"
            >
              <Eye className="h-4 w-4" />
              {m.feeds.editor.previewUrl}
            </Button>
          )}
          <Button variant="outline" onClick={onClose}>
            {m.common.cancel}
          </Button>
          <Button onClick={handleSubmit} loading={saveMutation.isPending}>
            {isEdit ? m.feeds.editor.saveChanges : m.feeds.editor.subscribe}
          </Button>
        </>
      }
    >
      <form onSubmit={handleSubmit} className="space-y-4">
        <div>
          <Label htmlFor="feed-url">{m.feeds.editor.urlLabel}</Label>
          <Input
            id="feed-url"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder="https://blog.rust-lang.org/feed.xml"
            className="mt-1.5 font-mono text-xs"
            autoFocus
          />
        </div>

        <div className="grid gap-4 sm:grid-cols-2">
          <div>
            <Label htmlFor="feed-title">{m.feeds.editor.titleLabel}</Label>
            <Input
              id="feed-title"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder={m.feeds.editor.titlePlaceholder}
              className="mt-1.5"
            />
          </div>

          <div>
            <Label htmlFor="feed-interval">{m.feeds.editor.intervalLabel}</Label>
            <div className="mt-1.5 flex items-center gap-1.5">
              <Input
                id="feed-interval"
                type="number"
                min={60}
                step={60}
                value={pollIntervalSecs}
                onChange={(e) => setPollIntervalSecs(Number(e.target.value))}
                className="w-28 font-mono"
              />
              <div className="flex flex-wrap gap-1">
                {POLL_PRESETS.map((p) => (
                  <button
                    key={p.secs}
                    type="button"
                    onClick={() => setPollIntervalSecs(p.secs)}
                    className={`cursor-pointer rounded border px-2 py-1 font-mono text-[11px] ${
                      pollIntervalSecs === p.secs
                        ? "border-zinc-900 bg-zinc-900 text-white dark:border-zinc-100 dark:bg-zinc-100 dark:text-zinc-900"
                        : "border-zinc-200 text-zinc-600 hover:bg-zinc-50 dark:border-zinc-700 dark:text-zinc-300"
                    }`}
                  >
                    {p.label}
                  </button>
                ))}
              </div>
            </div>
          </div>
        </div>

        <div>
          <Label>{m.feeds.editor.tagsLabel}</Label>
          <div className="mt-1.5">
            <TagInput
              values={tags}
              onChange={setTags}
              suggestions={knownTags}
              placeholder={m.feeds.editor.tagsPlaceholder}
            />
          </div>
        </div>

        <div className="space-y-3 rounded-lg border border-zinc-200/80 bg-zinc-50/60 p-3.5 dark:border-zinc-800 dark:bg-zinc-950/60">
          <Switch
            id="feed-readability"
            checked={fetchFullContent}
            onCheckedChange={setFetchFullContent}
            label={m.feeds.editor.readabilityLabel}
            description={m.feeds.editor.readabilityDesc}
          />

          <Switch
            id="feed-proxy"
            checked={useProxy}
            onCheckedChange={setUseProxy}
            label={m.feeds.editor.proxyLabel}
            description={m.feeds.editor.proxyDesc}
          />

          <Switch
            id="feed-silent-init"
            checked={silentInit}
            onCheckedChange={setSilentInit}
            label={m.feeds.editor.silentInitLabel}
            description={m.feeds.editor.silentInitDesc}
          />

          {!isEdit && silentInit && (
            <div className="border-t border-zinc-200/70 pt-2.5 dark:border-zinc-800">
              <Switch
                id="feed-push-latest"
                checked={pushLatestOne}
                onCheckedChange={setPushLatestOne}
                label={m.feeds.editor.pushLatestLabel}
                description={m.feeds.editor.pushLatestDesc}
              />
            </div>
          )}
        </div>

        <FieldError message={formError} />
      </form>
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// Sub-component: Clear Cache Dialog (`POST /api/v1/feeds/:id/clear-cache`)
// ---------------------------------------------------------------------------

function ClearCacheDialog({
  feed,
  loading,
  onClose,
  onConfirm,
}: {
  feed: FeedDto;
  loading: boolean;
  onClose: () => void;
  onConfirm: (pollNow: boolean) => void;
}) {
  const { m } = useI18n();
  const [pollNow, setPollNow] = useState(true);

  return (
    <Modal
      open
      onClose={onClose}
      title={m.feeds.clearCache.title}
      size="sm"
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={loading}>
            {m.common.cancel}
          </Button>
          <Button
            variant="destructive"
            onClick={() => onConfirm(pollNow)}
            loading={loading}
          >
            {m.feeds.clearCache.confirm}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-300">
          {m.feeds.clearCache.desc(feed.title || feed.url)}
        </p>
        <div className="rounded-lg border border-zinc-200/80 bg-zinc-50/60 p-3 dark:border-zinc-800 dark:bg-zinc-950/60">
          <Switch
            id="clear-cache-poll-now"
            checked={pollNow}
            onCheckedChange={setPollNow}
            label={m.feeds.clearCache.pollNowLabel}
            description={m.feeds.clearCache.pollNowDesc}
          />
        </div>
      </div>
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// Sub-component: Live Feed & Filter Preview Modal (`POST /api/v1/feeds/preview`)
// ---------------------------------------------------------------------------

function FeedPreviewModal({
  open,
  onClose,
  initialUrl,
  initialFetchFullContent,
  initialUseProxy,
  feedId,
  onSubscribe,
}: {
  open: boolean;
  onClose: () => void;
  initialUrl: string;
  initialFetchFullContent: boolean;
  initialUseProxy: boolean;
  /** Set when opened from an already-subscribed feed. */
  feedId: number | null;
  onSubscribe: (settings: {
    url: string;
    fetch_full_content: boolean;
    use_proxy: boolean;
  }) => void;
}) {
  const { m, locale: uiLocale } = useI18n();
  const toast = useToast();
  const [url, setUrl] = useState(initialUrl);
  const [fetchFullContent, setFetchFullContent] = useState(
    initialFetchFullContent,
  );
  const [useProxy, setUseProxy] = useState(initialUseProxy);
  const [showFilters, setShowFilters] = useState(false);
  const [includeKeywords, setIncludeKeywords] = useState<string[]>([]);
  const [excludeKeywords, setExcludeKeywords] = useState<string[]>([]);
  const [includeRegex, setIncludeRegex] = useState("");
  const [excludeRegex, setExcludeRegex] = useState("");
  const [validationErr, setValidationErr] = useState<string | null>(null);
  const [expandedFingerprint, setExpandedFingerprint] = useState<string | null>(
    null,
  );

  // --- Debug push (`POST /api/v1/feeds/preview/push`) ---
  const channelsQuery = useQuery({
    queryKey: ["channels"],
    queryFn: () => api.channels.list(),
  });
  const channels = channelsQuery.data ?? [];
  const [pushChannelId, setPushChannelId] = useState<number | null>(null);
  const [pushMode, setPushMode] =
    useState<TelegramDeliveryMode>("rich_message");
  const [pushLinkPreview, setPushLinkPreview] = useState(true);
  const [pushLocale, setPushLocale] = useState<Locale>(uiLocale);
  const [pushingKey, setPushingKey] = useState<string | null>(null);
  const [pushResult, setPushResult] = useState<{
    ok: boolean;
    message: string;
    title: string;
  } | null>(null);

  const activeChannelId = pushChannelId ?? channels[0]?.id ?? null;
  const activeChannel = channels.find((c) => c.id === activeChannelId);

  const pushMutation = useMutation({
    mutationFn: ({ fingerprint }: { fingerprint?: string; key: string }) => {
      if (activeChannelId === null) {
        throw new Error(m.feeds.preview.debugNoChannel);
      }
      const locale: SystemLocale = pushLocale;
      return api.feeds.previewPush({
        url: url.trim(),
        feed_id: feedId ?? undefined,
        fingerprint_sha256: fingerprint,
        fetch_full_content: fetchFullContent,
        use_proxy: useProxy,
        channel_id: activeChannelId,
        options:
          activeChannel?.kind === "telegram"
            ? {
                telegram_mode: pushMode,
                link_preview:
                  pushMode === "compact_card" ? pushLinkPreview : undefined,
                locale,
              }
            : { locale },
      });
    },
    onMutate: ({ key }) => {
      setPushingKey(key);
      setPushResult(null);
    },
    onSuccess: (res) => {
      setPushResult({
        ok: res.ok,
        message: res.message,
        title: res.entry_title,
      });
      if (res.ok) {
        toast.success(m.feeds.preview.debugPushedTitle, res.entry_title);
      } else {
        toast.error(m.feeds.preview.debugFailedTitle, res.message);
      }
    },
    onError: (err) => {
      setPushResult({ ok: false, message: errorMessage(err), title: "" });
    },
    onSettled: () => setPushingKey(null),
  });

  const previewMutation = useMutation({
    mutationFn: () => {
      const hasFilter =
        includeKeywords.length > 0 ||
        excludeKeywords.length > 0 ||
        includeRegex.trim().length > 0 ||
        excludeRegex.trim().length > 0;

      const filter: ContentFilterRules | undefined = hasFilter
        ? {
            include_keywords: includeKeywords,
            exclude_keywords: excludeKeywords,
            include_regex: includeRegex.trim() || null,
            exclude_regex: excludeRegex.trim() || null,
          }
        : undefined;

      return api.feeds.preview({
        url: url.trim(),
        fetch_full_content: fetchFullContent,
        use_proxy: useProxy,
        filter,
      });
    },
  });

  const handleRunPreview = (e: FormEvent) => {
    e.preventDefault();
    setValidationErr(null);
    if (!isValidHttpUrl(url)) {
      setValidationErr(m.feeds.preview.errInvalidUrl);
      return;
    }
    const incErr = validateRegex(includeRegex);
    if (incErr) {
      setValidationErr(m.feeds.preview.errIncludeRegex(incErr));
      return;
    }
    const excErr = validateRegex(excludeRegex);
    if (excErr) {
      setValidationErr(m.feeds.preview.errExcludeRegex(excErr));
      return;
    }
    previewMutation.mutate();
  };

  const result: FeedPreviewResponse | undefined = previewMutation.data;

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={m.feeds.preview.title}
      description={m.feeds.preview.desc}
      size="2xl"
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            {m.common.close}
          </Button>
          {isValidHttpUrl(url) && feedId === null && (
            <Button
              onClick={() =>
                onSubscribe({
                  url: url.trim(),
                  fetch_full_content: fetchFullContent,
                  use_proxy: useProxy,
                })
              }
            >
              <Plus className="h-4 w-4" />
              {m.feeds.preview.subscribeWithSettings}
            </Button>
          )}
        </>
      }
    >
      <div className="space-y-4">
        <form onSubmit={handleRunPreview} className="space-y-3">
          <div className="flex flex-col gap-2 sm:flex-row">
            <Input
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="https://hnrss.org/frontpage"
              className="flex-1 font-mono text-xs"
            />
            <Button
              type="submit"
              loading={previewMutation.isPending}
              className="shrink-0"
            >
              <Eye className="h-4 w-4" />
              {m.feeds.preview.fetchAndPreview}
            </Button>
          </div>

          <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-zinc-200/80 bg-zinc-50/60 px-3.5 py-2.5 dark:border-zinc-800 dark:bg-zinc-950/60">
            <div className="flex flex-wrap items-center gap-5">
              <div className="flex items-center gap-2">
                <Switch
                  id="preview-readability"
                  checked={fetchFullContent}
                  onCheckedChange={setFetchFullContent}
                />
                <label
                  htmlFor="preview-readability"
                  className="cursor-pointer text-xs font-medium"
                >
                  {m.feeds.preview.readabilitySwitch}
                </label>
              </div>
              <div className="flex items-center gap-2">
                <Switch
                  id="preview-proxy"
                  checked={useProxy}
                  onCheckedChange={setUseProxy}
                />
                <label
                  htmlFor="preview-proxy"
                  className="cursor-pointer text-xs font-medium"
                >
                  {m.feeds.preview.proxySwitch}
                </label>
              </div>
            </div>

            <button
              type="button"
              onClick={() => setShowFilters((v) => !v)}
              className="inline-flex cursor-pointer items-center gap-1 text-xs font-medium text-zinc-600 hover:text-zinc-900 dark:text-zinc-300 dark:hover:text-white"
            >
              <Filter className="h-3.5 w-3.5" />
              {showFilters
                ? m.feeds.preview.hideFilters
                : m.feeds.preview.showFilters}
            </button>
          </div>

          {showFilters && (
            <div className="grid gap-3 rounded-lg border border-zinc-200 p-3.5 sm:grid-cols-2 dark:border-zinc-800">
              <div>
                <Label>{m.feeds.preview.includeKeywordsLabel}</Label>
                <div className="mt-1">
                  <TagInput
                    values={includeKeywords}
                    onChange={setIncludeKeywords}
                    placeholder={m.feeds.preview.includeKeywordsPlaceholder}
                  />
                </div>
              </div>
              <div>
                <Label>{m.feeds.preview.excludeKeywordsLabel}</Label>
                <div className="mt-1">
                  <TagInput
                    values={excludeKeywords}
                    onChange={setExcludeKeywords}
                    placeholder={m.feeds.preview.excludeKeywordsPlaceholder}
                  />
                </div>
              </div>
              <div>
                <Label>{m.feeds.preview.includeRegexLabel}</Label>
                <Input
                  value={includeRegex}
                  onChange={(e) => setIncludeRegex(e.target.value)}
                  placeholder="(?i)\b(rust|tokio)\b"
                  className="mt-1 font-mono text-xs"
                />
              </div>
              <div>
                <Label>{m.feeds.preview.excludeRegexLabel}</Label>
                <Input
                  value={excludeRegex}
                  onChange={(e) => setExcludeRegex(e.target.value)}
                  placeholder="(?i)\bsponsored\b"
                  className="mt-1 font-mono text-xs"
                />
              </div>
            </div>
          )}

          <FieldError
            message={
              validationErr ??
              (previewMutation.isError
                ? errorMessage(previewMutation.error)
                : null)
            }
          />
        </form>

        {/* Debug Push Bar */}
        {isValidHttpUrl(url) && (
          <div className="space-y-3 rounded-lg border border-sky-200/80 bg-sky-50/40 p-3.5 dark:border-sky-900/50 dark:bg-sky-950/20">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="flex items-center gap-1.5 text-xs font-semibold text-sky-800 dark:text-sky-200">
                <Send className="h-3.5 w-3.5" />
                {m.feeds.preview.debugTitle}
              </div>
              <span className="text-[11px] text-zinc-500 dark:text-zinc-400">
                {m.feeds.preview.debugHint}
              </span>
            </div>

            {channels.length === 0 ? (
              <p className="text-xs text-zinc-500">
                {m.feeds.preview.debugNoChannel}
              </p>
            ) : (
              <>
                <div className="grid gap-2.5 sm:grid-cols-3">
                  <div>
                    <Label htmlFor="debug-push-channel">
                      {m.feeds.preview.debugChannel}
                    </Label>
                    <Select
                      id="debug-push-channel"
                      value={activeChannelId ?? ""}
                      onChange={(e) => setPushChannelId(Number(e.target.value))}
                      className="mt-1 h-8 text-xs"
                    >
                      {channels.map((ch) => (
                        <option key={ch.id} value={ch.id}>
                          {ch.name} ({ch.kind.toUpperCase()})
                        </option>
                      ))}
                    </Select>
                  </div>
                  <div>
                    <Label htmlFor="debug-push-locale">
                      {m.feeds.preview.debugLanguage}
                    </Label>
                    <Select
                      id="debug-push-locale"
                      value={pushLocale}
                      onChange={(e) => setPushLocale(e.target.value as Locale)}
                      className="mt-1 h-8 text-xs"
                    >
                      <option value="zh-CN">简体中文</option>
                      <option value="en">English</option>
                    </Select>
                  </div>
                  {activeChannel?.kind === "telegram" && (
                    <div>
                      <Label htmlFor="debug-push-mode">
                        {m.feeds.preview.debugTelegramMode}
                      </Label>
                      <Select
                        id="debug-push-mode"
                        value={pushMode}
                        onChange={(e) =>
                          setPushMode(e.target.value as TelegramDeliveryMode)
                        }
                        className="mt-1 h-8 text-xs"
                      >
                        <option value="rich_message">
                          {m.routes.modes.rich_message.label}
                        </option>
                        <option value="compact_card">
                          {m.routes.modes.compact_card.label}
                        </option>
                        <option value="media_mode">
                          {m.routes.modes.media_mode.label}
                        </option>
                      </Select>
                    </div>
                  )}
                </div>

                <div className="flex flex-wrap items-center justify-between gap-3">
                  {activeChannel?.kind === "telegram" &&
                  pushMode === "compact_card" ? (
                    <div className="flex items-center gap-2">
                      <Switch
                        id="debug-push-link-preview"
                        checked={pushLinkPreview}
                        onCheckedChange={setPushLinkPreview}
                      />
                      <label
                        htmlFor="debug-push-link-preview"
                        className="cursor-pointer text-xs font-medium"
                      >
                        {m.routes.editor.linkPreviewLabel}
                      </label>
                    </div>
                  ) : (
                    <span />
                  )}
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => pushMutation.mutate({ key: "latest" })}
                    loading={pushingKey === "latest"}
                    disabled={pushMutation.isPending}
                  >
                    <Send className="h-3.5 w-3.5" />
                    {m.feeds.preview.pushLatest}
                  </Button>
                </div>
              </>
            )}

            {pushResult && (
              <div
                className={`rounded-md border px-3 py-2 text-xs ${
                  pushResult.ok
                    ? "border-emerald-200 bg-emerald-50 text-emerald-800 dark:border-emerald-900/60 dark:bg-emerald-950/40 dark:text-emerald-200"
                    : "border-rose-200 bg-rose-50 text-rose-700 dark:border-rose-900/60 dark:bg-rose-950/40 dark:text-rose-300"
                }`}
              >
                {pushResult.title && (
                  <div className="font-semibold">{pushResult.title}</div>
                )}
                <div className="break-words whitespace-pre-wrap">
                  {pushResult.message}
                </div>
              </div>
            )}
          </div>
        )}

        {result && (
          <div className="space-y-3 border-t border-zinc-200 pt-3 dark:border-zinc-800">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="flex items-center gap-2">
                {result.favicon_url && (
                  <img
                    src={result.favicon_url}
                    alt=""
                    className="h-4 w-4 rounded-xs"
                  />
                )}
                <span className="text-sm font-semibold">
                  {result.feed_title || m.feeds.preview.untitledFeed}
                </span>
                <Badge variant="mono">
                  {m.feeds.preview.itemsCount(result.items.length)}
                </Badge>
              </div>
              <span className="text-xs text-zinc-500">
                {m.feeds.preview.matchedSummary}{" "}
                <strong>
                  {result.items.filter((i) => i.matched_filter).length}
                </strong>{" "}
                / {result.items.length}
              </span>
            </div>

            <div className="max-h-96 space-y-2 overflow-y-auto pr-1">
              {result.items.map((item) => {
                const expanded =
                  expandedFingerprint === item.fingerprint_sha256;
                return (
                  <div
                    key={item.fingerprint_sha256}
                    className="rounded-lg border border-zinc-200 bg-white p-3 dark:border-zinc-800 dark:bg-zinc-950"
                  >
                    <div className="flex flex-wrap items-start justify-between gap-2">
                      <div className="min-w-0 flex-1">
                        <div className="flex flex-wrap items-center gap-1.5">
                          {item.matched_filter ? (
                            <Badge variant="success">
                              <Check className="h-3 w-3" />
                              {m.feeds.preview.matchedBadge}
                            </Badge>
                          ) : (
                            <Badge variant="default">
                              <X className="h-3 w-3" />
                              {m.feeds.preview.filteredBadge}
                            </Badge>
                          )}
                          {item.full_content_extracted && (
                            <Badge variant="info">{m.common.readability}</Badge>
                          )}
                          {item.media_urls.length > 0 && (
                            <Badge variant="outline">
                              {m.feeds.preview.mediaCount(item.media_urls.length)}
                            </Badge>
                          )}
                          <span className="font-mono text-[10px] text-zinc-400">
                            sha256:{item.fingerprint_sha256.slice(0, 12)}
                          </span>
                        </div>
                        <div className="mt-1 text-xs font-semibold text-zinc-900 dark:text-zinc-100">
                          {item.canonical_url ? (
                            <a
                              href={item.canonical_url}
                              target="_blank"
                              rel="noopener noreferrer"
                              className="hover:underline"
                            >
                              {item.title}
                            </a>
                          ) : (
                            item.title
                          )}
                        </div>
                        {item.filter_reason && (
                          <p className="mt-0.5 text-[11px] text-amber-600 dark:text-amber-400">
                            {m.feeds.preview.filterNote(item.filter_reason)}
                          </p>
                        )}
                        {item.first_sentence && (
                          <p className="mt-1 text-xs text-zinc-600 dark:text-zinc-400">
                            <span className="font-mono text-[10px] text-zinc-400">
                              {m.feeds.preview.firstSentenceTag}
                            </span>
                            {item.first_sentence}
                          </p>
                        )}
                      </div>
                      <div className="flex shrink-0 items-center gap-1">
                        {channels.length > 0 && (
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() =>
                              pushMutation.mutate({
                                key: item.fingerprint_sha256,
                                fingerprint: item.fingerprint_sha256,
                              })
                            }
                            loading={pushingKey === item.fingerprint_sha256}
                            disabled={pushMutation.isPending}
                          >
                            <Send className="h-3.5 w-3.5" />
                            {m.feeds.preview.pushThisItem}
                          </Button>
                        )}
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={() =>
                            setExpandedFingerprint(
                              expanded ? null : item.fingerprint_sha256,
                            )
                          }
                        >
                          <FileCode2 className="h-3.5 w-3.5" />
                          {expanded
                            ? m.feeds.preview.collapse
                            : m.feeds.preview.body}
                        </Button>
                      </div>
                    </div>

                    {expanded && (
                      <div className="mt-2.5 space-y-2 border-t border-zinc-100 pt-2.5 text-xs dark:border-zinc-800">
                        {item.summary_text && (
                          <p className="whitespace-pre-wrap text-zinc-600 dark:text-zinc-300">
                            {item.summary_text}
                          </p>
                        )}
                        {item.content_html_preview && (
                          <pre className="max-h-44 overflow-auto rounded bg-zinc-900 p-2.5 font-mono text-[11px] text-zinc-200">
                            {item.content_html_preview}
                          </pre>
                        )}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </div>
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// Sub-component: OPML Import Modal (`POST /api/v1/feeds/opml/import`)
// ---------------------------------------------------------------------------

function OpmlImportModal({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const [opmlXml, setOpmlXml] = useState("");
  const [silentInit, setSilentInit] = useState(true);
  const [err, setErr] = useState<string | null>(null);

  const importMutation = useMutation({
    mutationFn: () =>
      api.feeds.importOpml({
        opml_xml: opmlXml,
        silent_init: silentInit,
      }),
    onSuccess: (res) => {
      toast.success(
        m.feeds.opml.toastDoneTitle,
        m.feeds.opml.toastDoneDesc(
          res.imported_count,
          res.skipped_existing_count,
        ),
      );
      void queryClient.invalidateQueries({ queryKey: ["feeds"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
      onClose();
    },
    onError: (e) => setErr(errorMessage(e)),
  });

  const handleFileUpload = (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result === "string") {
        setOpmlXml(reader.result);
        setErr(null);
      }
    };
    reader.readAsText(file);
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={m.feeds.opml.title}
      description={m.feeds.opml.desc}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            {m.common.cancel}
          </Button>
          <Button
            onClick={() => {
              if (!opmlXml.trim()) {
                setErr(m.feeds.opml.errEmptyXml);
                return;
              }
              importMutation.mutate();
            }}
            loading={importMutation.isPending}
          >
            <Upload className="h-4 w-4" />
            {m.feeds.opml.importBtn}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <div>
          <Label htmlFor="opml-file">{m.feeds.opml.fileLabel}</Label>
          <Input
            id="opml-file"
            type="file"
            accept=".opml,.xml,text/xml,application/xml"
            onChange={handleFileUpload}
            className="mt-1.5 cursor-pointer file:mr-3 file:rounded file:border-0 file:bg-zinc-900 file:px-2.5 file:py-1 file:text-xs file:font-medium file:text-white dark:file:bg-zinc-100 dark:file:text-zinc-900"
          />
        </div>

        <div>
          <Label htmlFor="opml-xml">{m.feeds.opml.xmlLabel}</Label>
          <Textarea
            id="opml-xml"
            value={opmlXml}
            onChange={(e) => {
              setOpmlXml(e.target.value);
              setErr(null);
            }}
            placeholder='<?xml version="1.0" encoding="UTF-8"?><opml version="2.0">…</opml>'
            rows={7}
            className="mt-1.5 font-mono text-xs"
          />
          <FieldHint>{m.feeds.opml.dedupHint}</FieldHint>
        </div>

        <div className="rounded-lg border border-zinc-200/80 bg-zinc-50/60 p-3.5 dark:border-zinc-800 dark:bg-zinc-950/60">
          <Switch
            id="opml-silent-init"
            checked={silentInit}
            onCheckedChange={setSilentInit}
            label={m.feeds.opml.silentInitLabel}
            description={m.feeds.opml.silentInitDesc}
          />
        </div>

        <FieldError message={err} />
      </div>
    </Modal>
  );
}
