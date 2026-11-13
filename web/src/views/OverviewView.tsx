import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  Archive,
  CheckCircle2,
  Clock,
  Database,
  Download,
  Fingerprint,
  PauseCircle,
  RefreshCw,
  Send,
  ShieldAlert,
  Sparkles,
  Trash2,
} from "lucide-react";
import { api } from "@/api/client";
import type { FeedHealthStatus, TaskStatus } from "@/api/types";
import type { NavSection } from "@/components/layout/AppShell";
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/primitives";
import { useToast } from "@/components/ui/toast";
import {
  cn,
  errorMessage,
  formatDateTime,
  formatRelativeTime,
} from "@/lib/utils";

export interface OverviewViewProps {
  onNavigateFeeds: (status?: FeedHealthStatus) => void;
  onNavigateTasks: (status?: TaskStatus) => void;
  onNavigateSection: (section: NavSection) => void;
}

export function OverviewView({
  onNavigateFeeds,
  onNavigateTasks,
  onNavigateSection,
}: OverviewViewProps) {
  const toast = useToast();
  const queryClient = useQueryClient();

  const overviewQuery = useQuery({
    queryKey: ["overview"],
    queryFn: () => api.overview.get(),
    refetchInterval: 15_000,
  });

  const gcMutation = useMutation({
    mutationFn: () => api.maintenance.runGc(),
    onSuccess: (res) => {
      toast.success(
        "Payload GC completed",
        `Purged transient content from ${res.purged_entries} closed ${res.purged_entries === 1 ? "entry" : "entries"}; SHA-256 fingerprints retained.`,
      );
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
    },
    onError: (err) => {
      toast.error("Payload GC failed", errorMessage(err));
    },
  });

  const backupMutation = useMutation({
    mutationFn: () => api.maintenance.downloadBackup(),
    onSuccess: (filename) => {
      toast.success("SQLite snapshot downloaded", `Saved ${filename}`);
    },
    onError: (err) => {
      toast.error("Snapshot backup failed", errorMessage(err));
    },
  });

  const data = overviewQuery.data;

  const feedCards: Array<{
    key: FeedHealthStatus;
    label: string;
    count: number;
    icon: typeof CheckCircle2;
    tone: string;
  }> = [
    {
      key: "active",
      label: "Active",
      count: data?.feeds.active ?? 0,
      icon: CheckCircle2,
      tone: "text-emerald-600 dark:text-emerald-400",
    },
    {
      key: "backoff",
      label: "Backoff",
      count: data?.feeds.backoff ?? 0,
      icon: Clock,
      tone: "text-amber-600 dark:text-amber-400",
    },
    {
      key: "circuit_open",
      label: "Circuit Open",
      count: data?.feeds.circuit_open ?? 0,
      icon: ShieldAlert,
      tone: "text-rose-600 dark:text-rose-400",
    },
    {
      key: "paused",
      label: "Paused",
      count: data?.feeds.paused ?? 0,
      icon: PauseCircle,
      tone: "text-zinc-500 dark:text-zinc-400",
    },
  ];

  const taskCards: Array<{
    key: TaskStatus;
    label: string;
    count: number;
    badge: "default" | "info" | "warning" | "danger" | "success";
  }> = [
    {
      key: "queued",
      label: "Queued",
      count: data?.tasks.queued ?? 0,
      badge: "default",
    },
    {
      key: "delivering",
      label: "Delivering (Leased)",
      count: data?.tasks.delivering ?? 0,
      badge: "info",
    },
    {
      key: "retrying",
      label: "Retrying",
      count: data?.tasks.retrying ?? 0,
      badge: "warning",
    },
    {
      key: "deferred",
      label: "Quota Deferred",
      count: data?.tasks.deferred ?? 0,
      badge: "warning",
    },
    {
      key: "dead_letter",
      label: "Dead Letter",
      count: data?.tasks.dead_letter ?? 0,
      badge: "danger",
    },
    {
      key: "delivered",
      label: "Delivered",
      count: data?.tasks.delivered ?? 0,
      badge: "success",
    },
  ];

  return (
    <div className="space-y-6">
      {/* Page Header */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-lg font-bold tracking-tight">System Overview</h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400">
            Real-time polling health, transactional outbox leases, channel pacing cooldowns, and SQLite maintenance.
          </p>
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={() => void overviewQuery.refetch()}
          loading={overviewQuery.isFetching}
        >
          <RefreshCw className="h-3.5 w-3.5" />
          Refresh
        </Button>
      </div>

      {overviewQuery.isError && (
        <Card className="border-rose-200 bg-rose-50/50 dark:border-rose-900/60 dark:bg-rose-950/20">
          <CardContent className="flex items-center justify-between gap-4 pt-5">
            <div className="flex items-center gap-2.5 text-xs text-rose-700 dark:text-rose-300">
              <AlertTriangle className="h-4 w-4 shrink-0" />
              <span>
                Unable to load overview metrics: {errorMessage(overviewQuery.error)}
              </span>
            </div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void overviewQuery.refetch()}
            >
              Retry
            </Button>
          </CardContent>
        </Card>
      )}

      {/* Feed Health Section */}
      <section className="space-y-2.5">
        <div className="flex items-center justify-between">
          <h2 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase dark:text-zinc-400">
            Subscription Health
          </h2>
          <button
            type="button"
            onClick={() => onNavigateFeeds()}
            className="cursor-pointer text-xs font-medium text-zinc-600 hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-zinc-100"
          >
            Manage feeds →
          </button>
        </div>
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          {feedCards.map((item) => {
            const Icon = item.icon;
            return (
              <button
                key={item.key}
                type="button"
                onClick={() => onNavigateFeeds(item.key)}
                className="cursor-pointer rounded-xl border border-zinc-200/90 bg-white p-4 text-left transition-colors hover:border-zinc-300 hover:bg-zinc-50/70 dark:border-zinc-800 dark:bg-zinc-900 dark:hover:border-zinc-700"
              >
                <div className="flex items-center justify-between">
                  <span className="text-xs font-medium text-zinc-500 dark:text-zinc-400">
                    {item.label}
                  </span>
                  <Icon className={cn("h-4 w-4", item.tone)} />
                </div>
                <div className="mt-2 font-mono text-2xl font-bold tracking-tight">
                  {overviewQuery.isLoading ? "—" : item.count}
                </div>
              </button>
            );
          })}
        </div>
      </section>

      {/* Outbox Pipeline Section */}
      <section className="space-y-2.5">
        <div className="flex items-center justify-between">
          <h2 className="text-xs font-semibold tracking-wider text-zinc-500 uppercase dark:text-zinc-400">
            Transactional Outbox & Leases (1 Task = 1 Entry × 1 Route)
          </h2>
          <button
            type="button"
            onClick={() => onNavigateTasks()}
            className="cursor-pointer text-xs font-medium text-zinc-600 hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-zinc-100"
          >
            Inspect outbox →
          </button>
        </div>
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-6">
          {taskCards.map((item) => (
            <button
              key={item.key}
              type="button"
              onClick={() => onNavigateTasks(item.key)}
              className={cn(
                "cursor-pointer rounded-xl border border-zinc-200/90 bg-white p-3.5 text-left transition-colors hover:border-zinc-300 dark:border-zinc-800 dark:bg-zinc-900 dark:hover:border-zinc-700",
                item.key === "dead_letter" &&
                  item.count > 0 &&
                  "border-rose-300 bg-rose-50/40 dark:border-rose-900/70 dark:bg-rose-950/20",
              )}
            >
              <Badge variant={item.badge}>{item.label}</Badge>
              <div className="mt-2.5 font-mono text-xl font-bold">
                {overviewQuery.isLoading ? "—" : item.count}
              </div>
            </button>
          ))}
        </div>
      </section>

      {/* Bottom Grid: Deduplication Store + Channel Cooldowns + Maintenance */}
      <div className="grid gap-4 lg:grid-cols-3">
        {/* Fingerprints vs Ephemeral Payloads */}
        <Card>
          <CardHeader>
            <div className="flex items-center gap-2">
              <Fingerprint className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
              <CardTitle>Fingerprints & Ephemeral Payloads</CardTitle>
            </div>
            <CardDescription>
              SHA-256 fingerprints stay forever to prevent duplicates; HTML & media metadata burn after delivery + retention window.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-3">
            <div className="flex items-center justify-between rounded-lg bg-zinc-50 px-3.5 py-2.5 dark:bg-zinc-950">
              <div>
                <div className="text-xs font-medium text-zinc-700 dark:text-zinc-300">
                  Permanent Fingerprints
                </div>
                <div className="text-[11px] text-zinc-500">
                  4-tier dedup SHA-256 index
                </div>
              </div>
              <span className="font-mono text-lg font-bold">
                {overviewQuery.isLoading
                  ? "—"
                  : (data?.entries.total_fingerprints ?? 0).toLocaleString()}
              </span>
            </div>

            <div className="flex items-center justify-between rounded-lg bg-zinc-50 px-3.5 py-2.5 dark:bg-zinc-950">
              <div>
                <div className="text-xs font-medium text-zinc-700 dark:text-zinc-300">
                  Active Cached Payloads
                </div>
                <div className="text-[11px] text-zinc-500">
                  In-flight or within retry retention window
                </div>
              </div>
              <span className="font-mono text-lg font-bold text-emerald-600 dark:text-emerald-400">
                {overviewQuery.isLoading
                  ? "—"
                  : (data?.entries.active_payloads ?? 0).toLocaleString()}
              </span>
            </div>
          </CardContent>
        </Card>

        {/* Active Channel Cooldowns */}
        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <Send className="h-4 w-4 text-sky-600 dark:text-sky-400" />
                <CardTitle>Active Channel Cooldowns</CardTitle>
              </div>
              <button
                type="button"
                onClick={() => onNavigateSection("channels")}
                className="cursor-pointer text-xs text-zinc-500 hover:text-zinc-900 dark:hover:text-zinc-200"
              >
                Channels →
              </button>
            </div>
            <CardDescription>
              Downstream rate-limit locks (SMTP 421/45x, Telegram retry_after, Notion 429) shared across credential groups.
            </CardDescription>
          </CardHeader>
          <CardContent>
            {!data || data.active_cooldowns.length === 0 ? (
              <div className="flex items-center gap-2.5 rounded-lg border border-dashed border-zinc-200 px-3.5 py-5 text-xs text-zinc-500 dark:border-zinc-800 dark:text-zinc-400">
                <Sparkles className="h-4 w-4 shrink-0 text-emerald-500" />
                <span>
                  All delivery channels are pacing normally — no active cooldown locks.
                </span>
              </div>
            ) : (
              <div className="space-y-2">
                {data.active_cooldowns.map((cd) => (
                  <div
                    key={cd.channel_id}
                    className="flex items-center justify-between rounded-lg border border-amber-200/80 bg-amber-50/50 px-3 py-2 text-xs dark:border-amber-900/60 dark:bg-amber-950/30"
                  >
                    <div className="min-w-0">
                      <div className="truncate font-medium text-zinc-900 dark:text-zinc-100">
                        {cd.channel_name}
                      </div>
                      <div className="font-mono text-[11px] uppercase text-zinc-500">
                        {cd.kind}
                      </div>
                    </div>
                    <Badge
                      variant="warning"
                      title={formatDateTime(cd.cooldown_until)}
                    >
                      Resumes {formatRelativeTime(cd.cooldown_until)}
                    </Badge>
                  </div>
                ))}
              </div>
            )}
          </CardContent>
        </Card>

        {/* Maintenance & Online Backup */}
        <Card>
          <CardHeader>
            <div className="flex items-center gap-2">
              <Database className="h-4 w-4 text-zinc-700 dark:text-zinc-300" />
              <CardTitle>SQLite WAL Maintenance</CardTitle>
            </div>
            <CardDescription>
              Reclaim closed article payloads via <code className="font-mono">NOT EXISTS</code> GC or download a zero-downtime <code className="font-mono">VACUUM INTO</code> snapshot.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-2.5">
            <Button
              variant="outline"
              className="w-full justify-start"
              onClick={() => gcMutation.mutate()}
              loading={gcMutation.isPending}
            >
              <Trash2 className="h-4 w-4 text-zinc-500" />
              Run Ephemeral Payload GC Now
            </Button>

            <Button
              variant="secondary"
              className="w-full justify-start"
              onClick={() => backupMutation.mutate()}
              loading={backupMutation.isPending}
            >
              <Download className="h-4 w-4 text-zinc-700 dark:text-zinc-200" />
              Download Online SQLite Snapshot (.db)
            </Button>

            <p className="flex items-center gap-1.5 pt-1 text-[11px] text-zinc-500 dark:text-zinc-400">
              <Archive className="h-3.5 w-3.5 shrink-0" />
              Snapshots include feeds, channels (encrypted), routes, tasks & fingerprints.
            </p>
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
