import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  Clock,
  ExternalLink,
  FileSearch,
  Inbox,
  Pause,
  Play,
  RefreshCw,
  RotateCcw,
} from "lucide-react";
import { useState } from "react";
import { api } from "@/api/client";
import type {
  DeliveryCheckpoint,
  DeliveryTaskDto,
  QueueStateDto,
  TaskStatus,
  UpdateQueueRequest,
} from "@/api/types";
import {
  Badge,
  type BadgeVariant,
  Button,
  Card,
  ConfirmDialog,
  EmptyState,
  FieldError,
  Input,
  Modal,
  Select,
} from "@/components/ui/primitives";
import { useToast } from "@/components/ui/toast";
import { useI18n } from "@/i18n";
import type { Messages } from "@/i18n/en";
import {
  cn,
  errorMessage,
  formatDateTime,
  formatRelativeTime,
} from "@/lib/utils";

export interface TasksViewProps {
  initialStatusFilter?: TaskStatus | "all";
}

const DELIVERED_WARNING_MARKERS = [
  "Link Rewrite partial",
  "hard-bounced",
  "is disabled; kept original",
  "not found; kept original URLs",
  "capped at",
] as const;

function hasDeliveredWarning(task: DeliveryTaskDto): boolean {
  if (task.status !== "delivered" || !task.diagnostic_log) return false;
  return DELIVERED_WARNING_MARKERS.some((marker) =>
    task.diagnostic_log?.includes(marker),
  );
}

function statusBadge(
  task: DeliveryTaskDto,
  m: Messages,
): {
  label: string;
  variant: BadgeVariant;
  warning?: boolean;
} {
  if (hasDeliveredWarning(task)) {
    return {
      label: m.status.task.deliveredWarning,
      variant: "warning",
      warning: true,
    };
  }
  switch (task.status) {
    case "queued":
      return { label: m.status.task.queued, variant: "default" };
    case "delivering":
      return { label: m.status.task.delivering, variant: "info" };
    case "delivered":
      return { label: m.status.task.delivered, variant: "success" };
    case "retrying":
      return { label: m.status.task.retrying, variant: "warning" };
    case "deferred":
      return { label: m.status.task.deferred, variant: "warning" };
    case "dead_letter":
      return { label: m.status.task.dead_letter, variant: "danger" };
  }
}

/**
 * Semantic status code badge variant:
 * - `202` (Notion async accepted) / `429` (rate limit) / transient SMTP 421/45x -> amber (`warning`)
 * - Other `2xx` -> green (`success`)
 * - Permanent `4xx` / `5xx` -> red (`danger`)
 * - Other -> neutral (`default`)
 */
function statusCodeVariant(code: number, status: TaskStatus): BadgeVariant {
  if (code === 202 || code === 429) return "warning";
  if (code >= 200 && code < 300) return "success";
  if (
    (code === 421 || (code >= 450 && code <= 459)) &&
    (status === "retrying" || status === "deferred")
  ) {
    return "warning";
  }
  if (code >= 400) return "danger";
  return "default";
}

/**
 * Semantic attempt count coloring:
 * - `0x` / `1x` -> neutral (`default`)
 * - `2x..5x` -> amber (`warning`)
 * - `>= 6x` (retry limit) -> red (`danger`)
 */
function attemptCountVariant(attempts: number): BadgeVariant {
  if (attempts >= 6) return "danger";
  if (attempts >= 2) return "warning";
  return "default";
}

/** Diagnostic log preview color in table rows matching task status (amber for delivered with warnings). */
function diagnosticRowTextClass(status: TaskStatus, isWarning = false): string {
  if (isWarning) {
    return "text-amber-700 dark:text-amber-400";
  }
  switch (status) {
    case "delivered":
      return "text-emerald-700 dark:text-emerald-400";
    case "retrying":
    case "deferred":
      return "text-amber-700 dark:text-amber-400";
    case "dead_letter":
      return "text-rose-600 dark:text-rose-400";
    case "queued":
    case "delivering":
      return "text-zinc-600 dark:text-zinc-400";
  }
}

/** Diagnostic log color inside dark modal `<pre>` matching task status. */
function diagnosticModalTextClass(
  status: TaskStatus,
  isWarning = false,
): string {
  if (isWarning) {
    return "text-amber-300";
  }
  switch (status) {
    case "delivered":
      return "text-emerald-300";
    case "retrying":
    case "deferred":
      return "text-amber-300";
    case "dead_letter":
      return "text-rose-300";
    case "queued":
    case "delivering":
      return "text-zinc-200";
  }
}

function summarizeCheckpoint(
  cp: DeliveryCheckpoint | null,
  m: Messages,
): string | null {
  if (!cp) return null;
  switch (cp.kind) {
    case "smtp":
      return cp.sent_recipients && cp.sent_recipients.length > 0
        ? m.tasks.checkpoints.smtpSent(cp.sent_recipients.length)
        : m.tasks.checkpoints.smtp;
    case "telegram":
      if (cp.degraded_to_compact) return m.tasks.checkpoints.tgCompact;
      return m.tasks.checkpoints.tgBatch(cp.next_batch_offset);
    case "notion":
      if (cp.async_task_id) {
        return m.tasks.checkpoints.notionAsync(cp.async_task_id.slice(0, 8));
      }
      if (cp.page_id) {
        return m.tasks.checkpoints.notionPage(cp.page_id.slice(0, 8));
      }
      if (cp.create_initiated) {
        return m.tasks.checkpoints.notionFcId;
      }
      return m.tasks.checkpoints.notionDefault;
  }
}

const PAUSE_PRESETS = [
  { id: "forever", secs: null },
  { id: "15m", secs: 15 * 60 },
  { id: "30m", secs: 30 * 60 },
  { id: "1h", secs: 60 * 60 },
  { id: "3h", secs: 3 * 60 * 60 },
  { id: "6h", secs: 6 * 60 * 60 },
  { id: "custom", secs: null },
] as const;

type PausePresetId = (typeof PAUSE_PRESETS)[number]["id"];

function isFuture(value: string | null | undefined): boolean {
  if (!value) return false;
  const t = Date.parse(value);
  return Number.isFinite(t) && t > Date.now();
}

function QueueControlBar({ queue }: { queue: QueueStateDto | undefined }) {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const [preset, setPreset] = useState<PausePresetId>("forever");
  const [customAt, setCustomAt] = useState("");
  const [error, setError] = useState<string | null>(null);

  const paused = queue?.paused === true;

  const mutation = useMutation({
    mutationFn: (payload: UpdateQueueRequest) => api.tasks.updateQueue(payload),
    onSuccess: (res) => {
      queryClient.setQueryData(["task-queue"], res);
      void queryClient.invalidateQueries({ queryKey: ["tasks"] });
      toast.success(
        res.paused ? m.tasks.queue.toastPaused : m.tasks.queue.toastResumed,
      );
      setError(null);
    },
    onError: (err) => {
      setError(errorMessage(err));
    },
  });

  const handlePause = () => {
    setError(null);
    const chosen = PAUSE_PRESETS.find((p) => p.id === preset);
    if (preset === "custom") {
      const when = customAt ? new Date(customAt) : null;
      if (
        !when ||
        Number.isNaN(when.getTime()) ||
        when.getTime() <= Date.now()
      ) {
        setError(m.tasks.queue.errCustomPast);
        return;
      }
      mutation.mutate({ action: "pause", resume_at: when.toISOString() });
      return;
    }
    mutation.mutate(
      chosen?.secs
        ? { action: "pause", resume_in_secs: chosen.secs }
        : { action: "pause" },
    );
  };

  return (
    <Card
      className={`p-3.5 ${
        paused
          ? "border-amber-300 bg-amber-50/70 dark:border-amber-900/60 dark:bg-amber-950/30"
          : ""
      }`}
    >
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2.5">
          {paused ? (
            <Pause className="h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400" />
          ) : (
            <Play className="h-4 w-4 shrink-0 text-emerald-600 dark:text-emerald-400" />
          )}
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-xs font-semibold">
                {m.tasks.queue.title}
              </span>
              <Badge variant={paused ? "warning" : "success"}>
                {paused ? m.tasks.queue.paused : m.tasks.queue.running}
              </Badge>
            </div>
            <p className="mt-0.5 text-[11px] text-zinc-500 dark:text-zinc-400">
              {paused
                ? queue?.resume_at
                  ? m.tasks.queue.pausedUntil(
                      formatDateTime(queue.resume_at),
                      formatRelativeTime(queue.resume_at),
                    )
                  : m.tasks.queue.pausedIndefinitely
                : m.tasks.queue.runningHint}
            </p>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <Select
            value={preset}
            onChange={(e) => {
              setPreset(e.target.value as PausePresetId);
              setError(null);
            }}
            aria-label={m.tasks.queue.durationLabel}
            className="h-8 w-auto text-xs"
          >
            {PAUSE_PRESETS.map((p) => (
              <option key={p.id} value={p.id}>
                {m.tasks.queue.presets[p.id]}
              </option>
            ))}
          </Select>
          {preset === "custom" && (
            <Input
              type="datetime-local"
              value={customAt}
              onChange={(e) => {
                setCustomAt(e.target.value);
                setError(null);
              }}
              aria-label={m.tasks.queue.customLabel}
              className="h-8 w-auto text-xs"
            />
          )}
          <Button
            variant="outline"
            size="sm"
            onClick={handlePause}
            loading={
              mutation.isPending && mutation.variables?.action === "pause"
            }
          >
            <Pause className="h-3.5 w-3.5" />
            {paused ? m.tasks.queue.updatePause : m.tasks.queue.pauseBtn}
          </Button>
          {paused && (
            <Button
              size="sm"
              onClick={() => mutation.mutate({ action: "resume" })}
              loading={
                mutation.isPending && mutation.variables?.action === "resume"
              }
            >
              <Play className="h-3.5 w-3.5" />
              {m.tasks.queue.resumeBtn}
            </Button>
          )}
        </div>
      </div>
      <FieldError message={error} />
      <p className="mt-2 text-[11px] text-zinc-500 dark:text-zinc-400">
        {m.tasks.queue.orderHint}
      </p>
    </Card>
  );
}

export function TasksView({ initialStatusFilter = "all" }: TasksViewProps) {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();

  const [statusFilter, setStatusFilter] = useState<TaskStatus | "all">(
    initialStatusFilter,
  );
  const [limit, setLimit] = useState<number>(100);
  const [selectedIds, setSelectedIds] = useState<number[]>([]);
  const [inspectingTask, setInspectingTask] =
    useState<DeliveryTaskDto | null>(null);
  const [confirmRetryAllOpen, setConfirmRetryAllOpen] = useState(false);

  const tasksQuery = useQuery({
    queryKey: ["tasks", statusFilter, limit],
    queryFn: () =>
      api.tasks.list({
        status: statusFilter === "all" ? undefined : statusFilter,
        limit,
      }),
    refetchInterval: 10_000,
  });

  const queueQuery = useQuery({
    queryKey: ["task-queue"],
    queryFn: () => api.tasks.queue(),
    refetchInterval: 10_000,
  });
  const queuePaused = queueQuery.data?.paused === true;

  const retryMutation = useMutation({
    mutationFn: (payload: { task_ids?: number[]; all_dead_letter?: boolean }) =>
      api.tasks.retry(payload),
    onSuccess: (res) => {
      toast.success(
        m.tasks.toastRequeuedTitle,
        m.tasks.toastRequeuedDesc(res.requeued_count),
      );
      setSelectedIds([]);
      setConfirmRetryAllOpen(false);
      void queryClient.invalidateQueries({ queryKey: ["tasks"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
    },
    onError: (err) => {
      toast.error(m.tasks.toastRetryFailed, errorMessage(err));
    },
  });

  const tasks = tasksQuery.data ?? [];
  const deadLetterCount = tasks.filter((t) => t.status === "dead_letter").length;

  const toggleSelect = (id: number) => {
    setSelectedIds((prev) =>
      prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id],
    );
  };

  return (
    <div className="space-y-5">
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-lg font-bold tracking-tight">{m.tasks.title}</h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400">
            {m.tasks.subtitle}
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          {selectedIds.length > 0 && (
            <Button
              size="sm"
              onClick={() => retryMutation.mutate({ task_ids: selectedIds })}
              loading={retryMutation.isPending}
            >
              <RotateCcw className="h-3.5 w-3.5" />
              {m.tasks.retrySelected(selectedIds.length)}
            </Button>
          )}
          <Button
            variant="secondary"
            size="sm"
            onClick={() => setConfirmRetryAllOpen(true)}
            disabled={
              statusFilter !== "all" &&
              statusFilter !== "dead_letter" &&
              deadLetterCount === 0
            }
          >
            <RotateCcw className="h-3.5 w-3.5" />
            {m.tasks.retryAllDeadLetter}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => void tasksQuery.refetch()}
            loading={tasksQuery.isFetching}
          >
            <RefreshCw className="h-3.5 w-3.5" />
            {m.common.refresh}
          </Button>
        </div>
      </div>

      {/* Global Queue Control */}
      <QueueControlBar queue={queueQuery.data} />

      {/* Filter Bar */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-1 rounded-lg border border-zinc-200 bg-white p-1 dark:border-zinc-800 dark:bg-zinc-900">
          {(
            [
              { id: "all", label: m.common.all },
              { id: "queued", label: m.status.task.queued },
              { id: "delivering", label: m.status.task.delivering },
              { id: "retrying", label: m.status.task.retrying },
              { id: "deferred", label: m.status.task.deferred },
              { id: "dead_letter", label: m.status.task.dead_letter },
              { id: "delivered", label: m.status.task.delivered },
            ] as const
          ).map((tab) => (
            <button
              key={tab.id}
              type="button"
              onClick={() => {
                setStatusFilter(tab.id);
                setSelectedIds([]);
              }}
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

        <div className="flex items-center gap-2">
          <span className="text-xs text-zinc-500">{m.common.rows}</span>
          <Select
            value={limit}
            onChange={(e) => setLimit(Number(e.target.value))}
            className="h-8 w-24 font-mono text-xs"
          >
            <option value={50}>50</option>
            <option value={100}>100</option>
            <option value={250}>250</option>
          </Select>
        </div>
      </div>

      {/* Tasks Table */}
      {tasksQuery.isLoading ? (
        <Card className="p-8 text-center text-xs text-zinc-500">
          {m.tasks.loading}
        </Card>
      ) : tasks.length === 0 ? (
        <EmptyState
          icon={Inbox}
          title={m.tasks.emptyTitle}
          description={m.tasks.emptyDesc}
        />
      ) : (
        <Card className="overflow-hidden">
          <div className="overflow-x-auto">
            <table className="w-full text-left text-xs">
              <thead className="border-b border-zinc-200 bg-zinc-50/80 text-[11px] text-zinc-500 uppercase dark:border-zinc-800 dark:bg-zinc-950/60">
                <tr>
                  <th className="w-8 px-3 py-2.5">
                    <span className="sr-only">{m.tasks.colSelect}</span>
                  </th>
                  <th className="px-3 py-2.5 font-medium">
                    {m.tasks.colEntry}
                  </th>
                  <th className="px-3 py-2.5 font-medium">
                    {m.tasks.colRouteChannel}
                  </th>
                  <th className="px-3 py-2.5 font-medium">
                    {m.tasks.colStatusLease}
                  </th>
                  <th className="px-3 py-2.5 font-medium">
                    {m.tasks.colAttemptsCode}
                  </th>
                  <th className="px-3 py-2.5 font-medium">
                    {m.tasks.colTiming}
                  </th>
                  <th className="px-3 py-2.5 text-right font-medium">
                    {m.tasks.colActions}
                  </th>
                </tr>
              </thead>
              <tbody className="divide-y divide-zinc-100 dark:divide-zinc-800/80">
                {tasks.map((task) => {
                  const badge = statusBadge(task, m);
                  const isPartialWarning = Boolean(badge.warning);
                  const cpSummary = summarizeCheckpoint(task.checkpoint, m);
                  const canRetry =
                    task.status === "dead_letter" ||
                    task.status === "retrying" ||
                    task.status === "deferred";

                  return (
                    <tr
                      key={task.id}
                      className="hover:bg-zinc-50/70 dark:hover:bg-zinc-950/40"
                    >
                      <td className="px-3 py-3">
                        {canRetry && (
                          <input
                            type="checkbox"
                            checked={selectedIds.includes(task.id)}
                            onChange={() => toggleSelect(task.id)}
                            aria-label={m.tasks.selectTaskAria(task.id)}
                            className="accent-zinc-900 dark:accent-emerald-500"
                          />
                        )}
                      </td>

                      <td className="max-w-xs px-3 py-3">
                        <div className="flex items-center gap-1.5">
                          <span className="font-mono text-[10px] text-zinc-400">
                            #{task.id}
                          </span>
                          {task.entry_title ? (
                            <span className="truncate font-medium text-zinc-900 dark:text-zinc-100">
                              {task.entry_title}
                            </span>
                          ) : (
                            <span className="italic text-zinc-400">
                              {m.tasks.payloadPurged(task.entry_id)}
                            </span>
                          )}
                          {task.entry_url && (
                            <a
                              href={task.entry_url}
                              target="_blank"
                              rel="noopener noreferrer"
                              className="text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
                            >
                              <ExternalLink className="h-3 w-3 shrink-0" />
                            </a>
                          )}
                        </div>
                        {task.feed_title && (
                          <div className="mt-0.5 truncate text-[11px] text-zinc-500">
                            {task.feed_title}
                          </div>
                        )}
                      </td>

                      <td className="px-3 py-3">
                        <div className="font-medium text-zinc-800 dark:text-zinc-200">
                          {task.route_name}
                        </div>
                        <div className="mt-0.5 flex flex-wrap items-center gap-1 text-[11px] text-zinc-500">
                          <span>→ {task.channel_name}</span>
                          {task.destination_name && (
                            <>
                              <span className="text-zinc-400">/</span>
                              <span className="font-medium text-zinc-700 dark:text-zinc-300">
                                {task.destination_name}
                              </span>
                            </>
                          )}
                          <span className="font-mono text-[10px] uppercase text-zinc-400">
                            ({task.channel_kind})
                          </span>
                        </div>
                      </td>

                      <td className="px-3 py-3">
                        <div className="flex flex-wrap items-center gap-1">
                          <Badge variant={badge.variant}>
                            {isPartialWarning && (
                              <AlertTriangle className="h-3 w-3 shrink-0" />
                            )}
                            {badge.label}
                          </Badge>
                          {cpSummary && (
                            <Badge variant="mono">{cpSummary}</Badge>
                          )}
                          {queuePaused &&
                            (task.status === "queued" ||
                              task.status === "retrying" ||
                              task.status === "deferred") && (
                              <Badge variant="warning">
                                <Pause className="h-3 w-3" />
                                {m.tasks.queue.heldBadge}
                              </Badge>
                            )}
                        </div>
                        {(task.status === "queued" ||
                          task.status === "deferred") &&
                          isFuture(task.next_attempt_at) && (
                            <div
                              className="mt-1 inline-flex items-center gap-1 text-[10px] text-amber-700 dark:text-amber-400"
                              title={formatDateTime(task.next_attempt_at)}
                            >
                              <Clock className="h-3 w-3" />
                              {m.tasks.scheduledPrefix(
                                formatRelativeTime(task.next_attempt_at),
                              )}
                            </div>
                          )}
                        {task.lease_until && task.status === "delivering" && (
                          <div
                            className="mt-1 font-mono text-[10px] text-sky-600 dark:text-sky-400"
                            title={formatDateTime(task.lease_until)}
                          >
                            {m.tasks.leasePrefix(
                              formatRelativeTime(task.lease_until),
                            )}
                          </div>
                        )}
                      </td>

                      <td className="px-3 py-3">
                        <div className="flex flex-wrap items-center gap-1.5 font-mono">
                          <Badge
                            variant={attemptCountVariant(task.attempt_count)}
                            className="font-mono text-[11px]"
                          >
                            {task.attempt_count}x
                          </Badge>
                          {task.last_status_code !== null && (
                            <Badge
                              variant={statusCodeVariant(
                                task.last_status_code,
                                task.status,
                              )}
                              className="font-mono text-[11px]"
                            >
                              {task.last_status_code}
                            </Badge>
                          )}
                        </div>
                        {task.diagnostic_log && (
                          <div
                            className={cn(
                              "mt-1 max-w-[200px] truncate font-sans text-[11px]",
                              diagnosticRowTextClass(
                                task.status,
                                isPartialWarning,
                              ),
                            )}
                            title={task.diagnostic_log}
                          >
                            {task.diagnostic_log}
                          </div>
                        )}
                      </td>

                      <td className="px-3 py-3 text-[11px] text-zinc-500">
                        <div title={formatDateTime(task.created_at)}>
                          {m.tasks.createdPrefix(
                            formatRelativeTime(task.created_at),
                          )}
                        </div>
                        {task.completed_at ? (
                          <div title={formatDateTime(task.completed_at)}>
                            {m.tasks.donePrefix(
                              formatRelativeTime(task.completed_at),
                            )}
                          </div>
                        ) : task.next_attempt_at ? (
                          <div title={formatDateTime(task.next_attempt_at)}>
                            {m.tasks.nextPrefix(
                              formatRelativeTime(task.next_attempt_at),
                            )}
                          </div>
                        ) : null}
                      </td>

                      <td className="px-3 py-3 text-right">
                        <div className="inline-flex items-center gap-1">
                          {canRetry && (
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={() =>
                                retryMutation.mutate({ task_ids: [task.id] })
                              }
                              loading={retryMutation.isPending}
                            >
                              <RotateCcw className="h-3 w-3" />
                              {m.common.retry}
                            </Button>
                          )}
                          <Button
                            variant="ghost"
                            size="icon"
                            onClick={() => setInspectingTask(task)}
                            title={m.tasks.inspectTitle}
                            aria-label={m.tasks.inspectTitle}
                          >
                            <FileSearch className="h-4 w-4" />
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

      {/* Task Diagnostic & Checkpoint Inspector Modal */}
      {inspectingTask && (
        <Modal
          open={inspectingTask !== null}
          onClose={() => setInspectingTask(null)}
          title={m.tasks.modal.title(inspectingTask.id)}
          description={`${inspectingTask.route_name} → ${inspectingTask.channel_name}${inspectingTask.destination_name ? ` / ${inspectingTask.destination_name}` : ""} (${inspectingTask.channel_kind.toUpperCase()})`}
          size="lg"
          footer={
            <>
              {(inspectingTask.status === "dead_letter" ||
                inspectingTask.status === "retrying" ||
                inspectingTask.status === "deferred") && (
                <Button
                  onClick={() => {
                    retryMutation.mutate({ task_ids: [inspectingTask.id] });
                    setInspectingTask(null);
                  }}
                >
                  <RotateCcw className="h-3.5 w-3.5" />
                  {m.tasks.modal.requeueBtn}
                </Button>
              )}
              <Button
                variant="outline"
                onClick={() => setInspectingTask(null)}
              >
                {m.common.close}
              </Button>
            </>
          }
        >
          <div className="space-y-4 text-xs">
            <div className="grid grid-cols-2 gap-3 rounded-lg border border-zinc-200 bg-zinc-50/60 p-3 sm:grid-cols-3 dark:border-zinc-800 dark:bg-zinc-950">
              <div>
                <div className="text-zinc-500">{m.tasks.modal.status}</div>
                <div className="mt-1">
                  {(() => {
                    const modalBadge = statusBadge(inspectingTask, m);
                    return (
                      <Badge variant={modalBadge.variant}>
                        {modalBadge.warning && (
                          <AlertTriangle className="h-3 w-3 shrink-0" />
                        )}
                        {modalBadge.label}
                      </Badge>
                    );
                  })()}
                </div>
              </div>
              <div>
                <div className="text-zinc-500">{m.tasks.modal.attempts}</div>
                <div className="mt-1">
                  <Badge
                    variant={attemptCountVariant(inspectingTask.attempt_count)}
                    className="font-mono"
                  >
                    {inspectingTask.attempt_count}x
                  </Badge>
                </div>
              </div>
              <div>
                <div className="text-zinc-500">
                  {m.tasks.modal.lastStatusCode}
                </div>
                <div className="mt-1">
                  {inspectingTask.last_status_code !== null ? (
                    <Badge
                      variant={statusCodeVariant(
                        inspectingTask.last_status_code,
                        inspectingTask.status,
                      )}
                      className="font-mono"
                    >
                      {inspectingTask.last_status_code}
                    </Badge>
                  ) : (
                    <span className="font-mono font-semibold text-zinc-400">
                      —
                    </span>
                  )}
                </div>
              </div>
              <div>
                <div className="text-zinc-500">{m.tasks.modal.leaseUntil}</div>
                <div className="mt-0.5 font-mono text-[11px]">
                  {formatDateTime(inspectingTask.lease_until)}
                </div>
              </div>
              <div>
                <div className="text-zinc-500">{m.tasks.modal.nextAttempt}</div>
                <div className="mt-0.5 font-mono text-[11px]">
                  {formatDateTime(inspectingTask.next_attempt_at)}
                </div>
              </div>
              <div>
                <div className="text-zinc-500">{m.tasks.modal.completedAt}</div>
                <div className="mt-0.5 font-mono text-[11px]">
                  {formatDateTime(inspectingTask.completed_at)}
                </div>
              </div>
            </div>

            <div>
              <div className="mb-1 font-semibold text-zinc-700 dark:text-zinc-300">
                {m.tasks.modal.resumeCheckpoint} (
                <code className="font-mono">checkpoint_json</code>)
              </div>
              <pre className="overflow-x-auto rounded-lg bg-zinc-900 p-3 font-mono text-[11px] text-emerald-300">
                {inspectingTask.checkpoint
                  ? JSON.stringify(inspectingTask.checkpoint, null, 2)
                  : m.tasks.modal.noCheckpoint}
              </pre>
            </div>

            <div>
              <div className="mb-1 font-semibold text-zinc-700 dark:text-zinc-300">
                {m.tasks.modal.diagnosticLog}
              </div>
              <pre
                className={cn(
                  "max-h-56 overflow-auto whitespace-pre-wrap rounded-lg bg-zinc-900 p-3 font-mono text-[11px]",
                  inspectingTask.diagnostic_log
                    ? diagnosticModalTextClass(
                        inspectingTask.status,
                        hasDeliveredWarning(inspectingTask),
                      )
                    : "text-zinc-400",
                )}
              >
                {inspectingTask.diagnostic_log || m.tasks.modal.noDiagnosticLog}
              </pre>
            </div>
          </div>
        </Modal>
      )}

      {/* Confirm Retry All Dead-Letter */}
      <ConfirmDialog
        open={confirmRetryAllOpen}
        onClose={() => setConfirmRetryAllOpen(false)}
        onConfirm={() => retryMutation.mutate({ all_dead_letter: true })}
        title={m.tasks.confirmAllTitle}
        description={m.tasks.confirmAllDesc}
        confirmLabel={m.tasks.confirmAllBtn}
        destructive={false}
        loading={retryMutation.isPending}
      />
    </div>
  );
}
