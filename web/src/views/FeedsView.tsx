import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  BookOpen,
  Check,
  Download,
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
  Switch,
  TagInput,
  Textarea,
} from "@/components/ui/primitives";
import { useToast } from "@/components/ui/toast";
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

function healthBadge(status: FeedHealthStatus): {
  label: string;
  variant: BadgeVariant;
} {
  switch (status) {
    case "active":
      return { label: "Active", variant: "success" };
    case "backoff":
      return { label: "Backoff", variant: "warning" };
    case "circuit_open":
      return { label: "Circuit Open", variant: "danger" };
    case "paused":
      return { label: "Paused", variant: "default" };
  }
}

export function FeedsView({ initialStatusFilter = "all" }: FeedsViewProps) {
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
  const [pollingFeedId, setPollingFeedId] = useState<number | null>(null);

  const feedsQuery = useQuery({
    queryKey: ["feeds"],
    queryFn: () => api.feeds.list(),
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
        res.not_modified ? "304 Not Modified" : "Feed polled",
        res.not_modified
          ? "Feed returned 304 Not Modified; schedule updated."
          : `Discovered ${res.new_entries} new entries (${res.enqueued_tasks} tasks enqueued).`,
      );
      void queryClient.invalidateQueries({ queryKey: ["feeds"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
    },
    onError: (err) => {
      toast.error("Immediate poll failed", errorMessage(err));
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
      toast.error("Failed to update feed", errorMessage(err));
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (id: number) => api.feeds.remove(id),
    onSuccess: () => {
      toast.success("Feed removed");
      setDeletingFeed(null);
      void queryClient.invalidateQueries({ queryKey: ["feeds"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
    },
    onError: (err) => {
      toast.error("Failed to delete feed", errorMessage(err));
    },
  });

  const exportOpmlMutation = useMutation({
    mutationFn: () => api.feeds.exportOpml(),
    onSuccess: (filename) => {
      toast.success("OPML exported", `Downloaded ${filename}`);
    },
    onError: (err) => {
      toast.error("OPML export failed", errorMessage(err));
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
  }) => {
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
          <h1 className="text-lg font-bold tracking-tight">
            Subscriptions & Readability
          </h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400">
            RSS / Atom / JSON Feed polling with SSRF protection, conditional ETag requests, per-feed Readability full-text extraction, and 4-tier deduplication.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => openPreviewModal()}
          >
            <Eye className="h-3.5 w-3.5" />
            Live Preview
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => setOpmlOpen(true)}
          >
            <Upload className="h-3.5 w-3.5" />
            Import OPML
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => exportOpmlMutation.mutate()}
            loading={exportOpmlMutation.isPending}
          >
            <Download className="h-3.5 w-3.5" />
            Export OPML
          </Button>
          <Button size="sm" onClick={() => openCreateModal()}>
            <Plus className="h-3.5 w-3.5" />
            Add Feed
          </Button>
        </div>
      </div>

      {/* Filters & Search */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-1 rounded-lg border border-zinc-200 bg-white p-1 dark:border-zinc-800 dark:bg-zinc-900">
          {(
            [
              { id: "all", label: "All" },
              { id: "active", label: "Active" },
              { id: "backoff", label: "Backoff" },
              { id: "circuit_open", label: "Circuit Open" },
              { id: "paused", label: "Paused" },
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
            placeholder="Filter by title, URL, or #tag…"
            className="pl-8"
          />
        </div>
      </div>

      {/* Feed List */}
      {feedsQuery.isLoading ? (
        <Card className="p-8 text-center text-xs text-zinc-500">
          Loading subscriptions…
        </Card>
      ) : filteredFeeds.length === 0 ? (
        <EmptyState
          icon={Rss}
          title={
            (feedsQuery.data?.length ?? 0) === 0
              ? "No feeds configured yet"
              : "No feeds match your current filter"
          }
          description={
            (feedsQuery.data?.length ?? 0) === 0
              ? "Subscribe to an RSS, Atom, or JSON Feed, or import an existing OPML file with silent initialization."
              : "Try clearing your search query or switching health status tabs."
          }
          action={
            (feedsQuery.data?.length ?? 0) === 0 ? (
              <div className="flex items-center gap-2">
                <Button size="sm" onClick={() => openCreateModal()}>
                  <Plus className="h-3.5 w-3.5" />
                  Add First Feed
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setOpmlOpen(true)}
                >
                  <Upload className="h-3.5 w-3.5" />
                  Import OPML
                </Button>
              </div>
            ) : undefined
          }
        />
      ) : (
        <div className="space-y-2.5">
          {filteredFeeds.map((feed) => {
            const badge = healthBadge(feed.health_status);
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
                          {feed.consecutive_failures} fail
                          {feed.consecutive_failures === 1 ? "" : "s"}
                        </Badge>
                      )}
                      {feed.fetch_full_content && (
                        <Badge variant="info">
                          <BookOpen className="h-3 w-3" />
                          Readability
                        </Badge>
                      )}
                      {feed.use_proxy && (
                        <Badge variant="outline">
                          <Globe className="h-3 w-3" />
                          Proxy
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
                        Interval:{" "}
                        <strong className="font-mono font-medium text-zinc-700 dark:text-zinc-300">
                          {formatDurationSecs(feed.poll_interval_secs)}
                        </strong>
                      </span>
                      <span>·</span>
                      <span title={formatDateTime(feed.next_poll_at)}>
                        Next poll:{" "}
                        <strong className="font-medium text-zinc-700 dark:text-zinc-300">
                          {isPaused
                            ? "paused"
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
                      title="Reset circuit breaker / backoff and trigger immediate poll"
                    >
                      <RefreshCw className="h-3.5 w-3.5" />
                      {feed.health_status === "circuit_open"
                        ? "Reset & Poll"
                        : "Poll Now"}
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
                      title="Toggle Readability full-text extraction"
                    >
                      <BookOpen className="h-3.5 w-3.5" />
                      {feed.fetch_full_content ? "Full Text: ON" : "Full Text: OFF"}
                    </Button>

                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() =>
                        openPreviewModal({
                          url: feed.url,
                          fetch_full_content: feed.fetch_full_content,
                          use_proxy: feed.use_proxy,
                        })
                      }
                      title="Preview feed & test filters"
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
                      title={isPaused ? "Resume feed polling" : "Pause feed polling"}
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
                      onClick={() => {
                        setEditingFeed(feed);
                        setEditorOpen(true);
                      }}
                      title="Edit feed settings"
                    >
                      <Pencil className="h-4 w-4" />
                    </Button>

                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => setDeletingFeed(feed)}
                      title="Delete feed"
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
          onSubscribe={(settings) => {
            setPreviewOpen(false);
            openCreateModal(settings);
          }}
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
        title="Delete Subscription"
        description={`Remove "${deletingFeed?.title || deletingFeed?.url}"? Associated entry fingerprints and delivery tasks for this feed will be cascaded.`}
        confirmLabel="Delete Feed"
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
  }) => void;
}) {
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
      toast.success(isEdit ? "Feed updated" : "Feed added");
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
      setFormError("Please enter a valid http:// or https:// feed URL.");
      return;
    }
    if (!Number.isFinite(pollIntervalSecs) || pollIntervalSecs < 60) {
      setFormError("Poll interval must be at least 60 seconds.");
      return;
    }
    saveMutation.mutate();
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={isEdit ? "Edit Subscription" : "Add Subscription"}
      description="Configure polling cadence, multi-tags (synced to Routes & Notion Tag), and Readability full-text extraction."
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
                })
              }
              className="mr-auto"
            >
              <Eye className="h-4 w-4" />
              Preview URL
            </Button>
          )}
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button onClick={handleSubmit} loading={saveMutation.isPending}>
            {isEdit ? "Save Changes" : "Subscribe"}
          </Button>
        </>
      }
    >
      <form onSubmit={handleSubmit} className="space-y-4">
        <div>
          <Label htmlFor="feed-url">Feed URL (RSS / Atom / JSON Feed)</Label>
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
            <Label htmlFor="feed-title">Custom Title Override (Optional)</Label>
            <Input
              id="feed-title"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="Auto-detected from feed XML if blank"
              className="mt-1.5"
            />
          </div>

          <div>
            <Label htmlFor="feed-interval">Poll Interval (Seconds)</Label>
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
          <Label>Tags (Multi-select routing & Notion Tag sync)</Label>
          <div className="mt-1.5">
            <TagInput
              values={tags}
              onChange={setTags}
              suggestions={knownTags}
              placeholder="e.g. rust, security, ai…"
            />
          </div>
        </div>

        <div className="space-y-3 rounded-lg border border-zinc-200/80 bg-zinc-50/60 p-3.5 dark:border-zinc-800 dark:bg-zinc-950/60">
          <Switch
            id="feed-readability"
            checked={fetchFullContent}
            onCheckedChange={setFetchFullContent}
            label="Readability Full-Text Extraction (dom_smoothie)"
            description="Fetch the canonical article webpage for summary-only feeds using an isolated concurrency semaphore; falls back gracefully to feed summary on anti-bot errors."
          />

          <Switch
            id="feed-proxy"
            checked={useProxy}
            onCheckedChange={setUseProxy}
            label="Route Through Outbound Proxy"
            description="Fetch this feed and its Readability / media requests via the configured HTTP/SOCKS5 outbound proxy."
          />

          <Switch
            id="feed-silent-init"
            checked={silentInit}
            onCheckedChange={setSilentInit}
            label="Silent Initialization (Anti-Flood Protection)"
            description="Record SHA-256 fingerprints for existing historical items on first sync without flooding downstream channels."
          />

          {!isEdit && silentInit && (
            <div className="border-t border-zinc-200/70 pt-2.5 dark:border-zinc-800">
              <Switch
                id="feed-push-latest"
                checked={pushLatestOne}
                onCheckedChange={setPushLatestOne}
                label="Verify Connectivity with Latest 1 Entry"
                description="Enqueue only the single newest item during silent initialization to confirm end-to-end delivery."
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
// Sub-component: Live Feed & Filter Preview Modal (`POST /api/v1/feeds/preview`)
// ---------------------------------------------------------------------------

function FeedPreviewModal({
  open,
  onClose,
  initialUrl,
  initialFetchFullContent,
  initialUseProxy,
  onSubscribe,
}: {
  open: boolean;
  onClose: () => void;
  initialUrl: string;
  initialFetchFullContent: boolean;
  initialUseProxy: boolean;
  onSubscribe: (settings: {
    url: string;
    fetch_full_content: boolean;
    use_proxy: boolean;
  }) => void;
}) {
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
      setValidationErr("Enter a valid http:// or https:// feed URL.");
      return;
    }
    const incErr = validateRegex(includeRegex);
    if (incErr) {
      setValidationErr(`Include regex invalid: ${incErr}`);
      return;
    }
    const excErr = validateRegex(excludeRegex);
    if (excErr) {
      setValidationErr(`Exclude regex invalid: ${excErr}`);
      return;
    }
    previewMutation.mutate();
  };

  const result: FeedPreviewResponse | undefined = previewMutation.data;

  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Live Feed & Filter Preview"
      description="Test-fetch any feed via the SSRF-guarded HTTP client, compare Readability extraction, and dry-run keyword/regex filters."
      size="2xl"
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Close
          </Button>
          {isValidHttpUrl(url) && (
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
              Subscribe with Settings
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
              Fetch & Preview
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
                  Readability Full-Text
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
                  Use Outbound Proxy
                </label>
              </div>
            </div>

            <button
              type="button"
              onClick={() => setShowFilters((v) => !v)}
              className="inline-flex cursor-pointer items-center gap-1 text-xs font-medium text-zinc-600 hover:text-zinc-900 dark:text-zinc-300 dark:hover:text-white"
            >
              <Filter className="h-3.5 w-3.5" />
              {showFilters ? "Hide Filter Dry-Run" : "Test Filter Rules"}
            </button>
          </div>

          {showFilters && (
            <div className="grid gap-3 rounded-lg border border-zinc-200 p-3.5 sm:grid-cols-2 dark:border-zinc-800">
              <div>
                <Label>Include Keywords (OR)</Label>
                <div className="mt-1">
                  <TagInput
                    values={includeKeywords}
                    onChange={setIncludeKeywords}
                    placeholder="Add keyword…"
                  />
                </div>
              </div>
              <div>
                <Label>Exclude Keywords (ANY)</Label>
                <div className="mt-1">
                  <TagInput
                    values={excludeKeywords}
                    onChange={setExcludeKeywords}
                    placeholder="Sponsor, Ad…"
                  />
                </div>
              </div>
              <div>
                <Label>Include Regex</Label>
                <Input
                  value={includeRegex}
                  onChange={(e) => setIncludeRegex(e.target.value)}
                  placeholder="(?i)\b(rust|tokio)\b"
                  className="mt-1 font-mono text-xs"
                />
              </div>
              <div>
                <Label>Exclude Regex</Label>
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
                  {result.feed_title || "Untitled Feed"}
                </span>
                <Badge variant="mono">{result.items.length} items</Badge>
              </div>
              <span className="text-xs text-zinc-500">
                Matched:{" "}
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
                              Matched
                            </Badge>
                          ) : (
                            <Badge variant="default">
                              <X className="h-3 w-3" />
                              Filtered
                            </Badge>
                          )}
                          {item.full_content_extracted && (
                            <Badge variant="info">Readability</Badge>
                          )}
                          {item.media_urls.length > 0 && (
                            <Badge variant="outline">
                              {item.media_urls.length} media
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
                            Filter note: {item.filter_reason}
                          </p>
                        )}
                        {item.first_sentence && (
                          <p className="mt-1 text-xs text-zinc-600 dark:text-zinc-400">
                            <span className="font-mono text-[10px] text-zinc-400">
                              [1st sentence]{" "}
                            </span>
                            {item.first_sentence}
                          </p>
                        )}
                      </div>
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
                        {expanded ? "Collapse" : "Body"}
                      </Button>
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
        "OPML import complete",
        `Imported ${res.imported_count} feeds (${res.skipped_existing_count} existing skipped).`,
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
      title="Import OPML Subscriptions"
      description="Upload an .opml / .xml file or paste OPML 2.0 XML directly. Nested OPML folders are automatically mapped to feed tags."
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button
            onClick={() => {
              if (!opmlXml.trim()) {
                setErr("Please upload or paste OPML XML content.");
                return;
              }
              importMutation.mutate();
            }}
            loading={importMutation.isPending}
          >
            <Upload className="h-4 w-4" />
            Import Feeds
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <div>
          <Label htmlFor="opml-file">Select OPML File</Label>
          <Input
            id="opml-file"
            type="file"
            accept=".opml,.xml,text/xml,application/xml"
            onChange={handleFileUpload}
            className="mt-1.5 cursor-pointer file:mr-3 file:rounded file:border-0 file:bg-zinc-900 file:px-2.5 file:py-1 file:text-xs file:font-medium file:text-white dark:file:bg-zinc-100 dark:file:text-zinc-900"
          />
        </div>

        <div>
          <Label htmlFor="opml-xml">OPML XML Content</Label>
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
          <FieldHint>
            Existing feeds with matching URLs are safely deduplicated and skipped.
          </FieldHint>
        </div>

        <div className="rounded-lg border border-zinc-200/80 bg-zinc-50/60 p-3.5 dark:border-zinc-800 dark:bg-zinc-950/60">
          <Switch
            id="opml-silent-init"
            checked={silentInit}
            onCheckedChange={setSilentInit}
            label="Silent Initialization (Recommended)"
            description="Record SHA-256 fingerprints for backlog articles during initial sync so imported feeds do not flood your channels."
          />
        </div>

        <FieldError message={err} />
      </div>
    </Modal>
  );
}
