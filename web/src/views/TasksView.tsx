import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ExternalLink,
  FileSearch,
  Inbox,
  RefreshCw,
  RotateCcw,
} from "lucide-react";
import { useState } from "react";
import { api } from "@/api/client";
import type {
  DeliveryCheckpoint,
  DeliveryTaskDto,
  TaskStatus,
} from "@/api/types";
import {
  Badge,
  type BadgeVariant,
  Button,
  Card,
  ConfirmDialog,
  EmptyState,
  Modal,
  Select,
} from "@/components/ui/primitives";
import { useToast } from "@/components/ui/toast";
import {
  errorMessage,
  formatDateTime,
  formatRelativeTime,
} from "@/lib/utils";

export interface TasksViewProps {
  initialStatusFilter?: TaskStatus | "all";
}

function statusBadge(status: TaskStatus): {
  label: string;
  variant: BadgeVariant;
} {
  switch (status) {
    case "queued":
      return { label: "Queued", variant: "default" };
    case "delivering":
      return { label: "Delivering", variant: "info" };
    case "delivered":
      return { label: "Delivered", variant: "success" };
    case "retrying":
      return { label: "Retrying", variant: "warning" };
    case "deferred":
      return { label: "Deferred", variant: "warning" };
    case "dead_letter":
      return { label: "Dead Letter", variant: "danger" };
  }
}

function summarizeCheckpoint(cp: DeliveryCheckpoint | null): string | null {
  if (!cp) return null;
  switch (cp.kind) {
    case "smtp":
      return "SMTP Msg-ID";
    case "telegram":
      if (cp.degraded_to_compact) return "TG degraded→compact";
      return `TG batch #${cp.next_batch_offset}`;
    case "notion":
      if (cp.async_task_id) {
        return `Notion async:${cp.async_task_id.slice(0, 8)}`;
      }
      if (cp.page_id) {
        return `Notion page:${cp.page_id.slice(0, 8)}`;
      }
      if (cp.create_initiated) {
        return "Notion FC-ID check";
      }
      return "Notion checkpoint";
  }
}

export function TasksView({ initialStatusFilter = "all" }: TasksViewProps) {
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

  const retryMutation = useMutation({
    mutationFn: (payload: { task_ids?: number[]; all_dead_letter?: boolean }) =>
      api.tasks.retry(payload),
    onSuccess: (res) => {
      toast.success(
        "Tasks requeued",
        `Requeued ${res.requeued_count} ${res.requeued_count === 1 ? "task" : "tasks"} into the transactional outbox.`,
      );
      setSelectedIds([]);
      setConfirmRetryAllOpen(false);
      void queryClient.invalidateQueries({ queryKey: ["tasks"] });
      void queryClient.invalidateQueries({ queryKey: ["overview"] });
    },
    onError: (err) => {
      toast.error("Retry failed", errorMessage(err));
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
          <h1 className="text-lg font-bold tracking-tight">
            Transactional Outbox & Dead-Letter Log
          </h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400">
            Atomic lease scheduling (<code className="font-mono">lease_until</code>), crash-safe <code className="font-mono">checkpoint</code> resume, and dead-letter diagnostics.
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
              Retry Selected ({selectedIds.length})
            </Button>
          )}
          <Button
            variant="secondary"
            size="sm"
            onClick={() => setConfirmRetryAllOpen(true)}
            disabled={statusFilter !== "all" && statusFilter !== "dead_letter" && deadLetterCount === 0}
          >
            <RotateCcw className="h-3.5 w-3.5" />
            Retry All Dead-Letter
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => void tasksQuery.refetch()}
            loading={tasksQuery.isFetching}
          >
            <RefreshCw className="h-3.5 w-3.5" />
            Refresh
          </Button>
        </div>
      </div>

      {/* Filter Bar */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-1 rounded-lg border border-zinc-200 bg-white p-1 dark:border-zinc-800 dark:bg-zinc-900">
          {(
            [
              { id: "all", label: "All" },
              { id: "queued", label: "Queued" },
              { id: "delivering", label: "Delivering" },
              { id: "retrying", label: "Retrying" },
              { id: "deferred", label: "Deferred" },
              { id: "dead_letter", label: "Dead Letter" },
              { id: "delivered", label: "Delivered" },
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
          <span className="text-xs text-zinc-500">Rows:</span>
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
          Loading outbox tasks…
        </Card>
      ) : tasks.length === 0 ? (
        <EmptyState
          icon={Inbox}
          title="No delivery tasks in this view"
          description="When subscribed feeds discover new entries matching an active route, outbox tasks appear here with lease & checkpoint state."
        />
      ) : (
        <Card className="overflow-hidden">
          <div className="overflow-x-auto">
            <table className="w-full text-left text-xs">
              <thead className="border-b border-zinc-200 bg-zinc-50/80 text-[11px] text-zinc-500 uppercase dark:border-zinc-800 dark:bg-zinc-950/60">
                <tr>
                  <th className="w-8 px-3 py-2.5">
                    <span className="sr-only">Select</span>
                  </th>
                  <th className="px-3 py-2.5 font-medium">Entry</th>
                  <th className="px-3 py-2.5 font-medium">Route → Channel</th>
                  <th className="px-3 py-2.5 font-medium">Status & Lease</th>
                  <th className="px-3 py-2.5 font-medium">Attempts / Code</th>
                  <th className="px-3 py-2.5 font-medium">Timing</th>
                  <th className="px-3 py-2.5 text-right font-medium">
                    Actions
                  </th>
                </tr>
              </thead>
              <tbody className="divide-y divide-zinc-100 dark:divide-zinc-800/80">
                {tasks.map((task) => {
                  const badge = statusBadge(task.status);
                  const cpSummary = summarizeCheckpoint(task.checkpoint);
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
                            aria-label={`Select task #${task.id}`}
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
                              Entry #{task.entry_id} (payload purged)
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
                        <div className="text-[11px] text-zinc-500">
                          → {task.channel_name} (
                          <span className="font-mono uppercase">
                            {task.channel_kind}
                          </span>
                          )
                        </div>
                      </td>

                      <td className="px-3 py-3">
                        <div className="flex flex-wrap items-center gap-1">
                          <Badge variant={badge.variant}>{badge.label}</Badge>
                          {cpSummary && (
                            <Badge variant="mono">{cpSummary}</Badge>
                          )}
                        </div>
                        {task.lease_until && task.status === "delivering" && (
                          <div
                            className="mt-1 font-mono text-[10px] text-sky-600 dark:text-sky-400"
                            title={formatDateTime(task.lease_until)}
                          >
                            Lease {formatRelativeTime(task.lease_until)}
                          </div>
                        )}
                      </td>

                      <td className="px-3 py-3 font-mono">
                        <span>{task.attempt_count}x</span>
                        {task.last_status_code !== null && (
                          <span className="ml-1.5 rounded bg-zinc-100 px-1.5 py-0.5 text-[11px] dark:bg-zinc-800">
                            {task.last_status_code}
                          </span>
                        )}
                        {task.diagnostic_log && (
                          <div className="mt-0.5 max-w-[180px] truncate font-sans text-[11px] text-rose-600 dark:text-rose-400">
                            {task.diagnostic_log}
                          </div>
                        )}
                      </td>

                      <td className="px-3 py-3 text-[11px] text-zinc-500">
                        <div title={formatDateTime(task.created_at)}>
                          Created: {formatRelativeTime(task.created_at)}
                        </div>
                        {task.completed_at ? (
                          <div title={formatDateTime(task.completed_at)}>
                            Done: {formatRelativeTime(task.completed_at)}
                          </div>
                        ) : task.next_attempt_at ? (
                          <div title={formatDateTime(task.next_attempt_at)}>
                            Next: {formatRelativeTime(task.next_attempt_at)}
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
                              Retry
                            </Button>
                          )}
                          <Button
                            variant="ghost"
                            size="icon"
                            onClick={() => setInspectingTask(task)}
                            title="Inspect checkpoint & diagnostic log"
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
          title={`Task #${inspectingTask.id} Diagnostics`}
          description={`${inspectingTask.route_name} → ${inspectingTask.channel_name} (${inspectingTask.channel_kind.toUpperCase()})`}
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
                  Requeue Task
                </Button>
              )}
              <Button
                variant="outline"
                onClick={() => setInspectingTask(null)}
              >
                Close
              </Button>
            </>
          }
        >
          <div className="space-y-4 text-xs">
            <div className="grid grid-cols-2 gap-3 rounded-lg border border-zinc-200 bg-zinc-50/60 p-3 sm:grid-cols-3 dark:border-zinc-800 dark:bg-zinc-950">
              <div>
                <div className="text-zinc-500">Status</div>
                <div className="mt-0.5 font-mono font-semibold">
                  {inspectingTask.status}
                </div>
              </div>
              <div>
                <div className="text-zinc-500">Attempts</div>
                <div className="mt-0.5 font-mono font-semibold">
                  {inspectingTask.attempt_count}
                </div>
              </div>
              <div>
                <div className="text-zinc-500">Last Status Code</div>
                <div className="mt-0.5 font-mono font-semibold">
                  {inspectingTask.last_status_code ?? "—"}
                </div>
              </div>
              <div>
                <div className="text-zinc-500">Lease Until</div>
                <div className="mt-0.5 font-mono text-[11px]">
                  {formatDateTime(inspectingTask.lease_until)}
                </div>
              </div>
              <div>
                <div className="text-zinc-500">Next Attempt</div>
                <div className="mt-0.5 font-mono text-[11px]">
                  {formatDateTime(inspectingTask.next_attempt_at)}
                </div>
              </div>
              <div>
                <div className="text-zinc-500">Completed At</div>
                <div className="mt-0.5 font-mono text-[11px]">
                  {formatDateTime(inspectingTask.completed_at)}
                </div>
              </div>
            </div>

            <div>
              <div className="mb-1 font-semibold text-zinc-700 dark:text-zinc-300">
                Resume Checkpoint (<code className="font-mono">checkpoint_json</code>)
              </div>
              <pre className="overflow-x-auto rounded-lg bg-zinc-900 p-3 font-mono text-[11px] text-emerald-300">
                {inspectingTask.checkpoint
                  ? JSON.stringify(inspectingTask.checkpoint, null, 2)
                  : "// No intermediate checkpoint recorded"}
              </pre>
            </div>

            <div>
              <div className="mb-1 font-semibold text-zinc-700 dark:text-zinc-300">
                Downstream Diagnostic Log
              </div>
              <pre className="max-h-56 overflow-auto whitespace-pre-wrap rounded-lg bg-zinc-900 p-3 font-mono text-[11px] text-zinc-200">
                {inspectingTask.diagnostic_log ||
                  "No error or diagnostic output recorded."}
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
        title="Retry All Dead-Letter Tasks"
        description="Requeue every dead_letter task whose entry payload is still available back into the queued state?"
        confirmLabel="Requeue All Dead-Letter"
        destructive={false}
        loading={retryMutation.isPending}
      />
    </div>
  );
}
