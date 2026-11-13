import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, Loader2, RefreshCw } from "lucide-react";
import { useEffect, useState } from "react";
import { api, onUnauthorized } from "@/api/client";
import {
  DEFAULT_LICENSE,
  DEFAULT_REPO_URL,
  DEFAULT_VERSION,
  type FeedHealthStatus,
  type TaskStatus,
} from "@/api/types";
import {
  AppShell,
  type NavSection,
  useThemeMode,
} from "@/components/layout/AppShell";
import { ComplianceFooter } from "@/components/layout/ComplianceFooter";
import { Button, Card, CardContent } from "@/components/ui/primitives";
import { useToast } from "@/components/ui/toast";
import { errorMessage } from "@/lib/utils";
import { AuthGateView } from "@/views/AuthGateView";
import { ChannelsView } from "@/views/ChannelsView";
import { FeedsView } from "@/views/FeedsView";
import { OverviewView } from "@/views/OverviewView";
import { RoutesView } from "@/views/RoutesView";
import { SettingsView } from "@/views/SettingsView";
import { TasksView } from "@/views/TasksView";

const VALID_SECTIONS: NavSection[] = [
  "overview",
  "feeds",
  "channels",
  "routes",
  "tasks",
  "settings",
];

function parseHashSection(): NavSection {
  if (typeof window === "undefined") return "overview";
  const raw = window.location.hash.replace(/^#\/?/, "").split("?")[0];
  if (VALID_SECTIONS.includes(raw as NavSection)) {
    return raw as NavSection;
  }
  return "overview";
}

export function App() {
  useThemeMode();
  const toast = useToast();
  const queryClient = useQueryClient();

  const [section, setSection] = useState<NavSection>(parseHashSection);
  const [feedFilterPreset, setFeedFilterPreset] = useState<{
    status: FeedHealthStatus | "all";
    nonce: number;
  }>({ status: "all", nonce: 0 });
  const [taskFilterPreset, setTaskFilterPreset] = useState<{
    status: TaskStatus | "all";
    nonce: number;
  }>({ status: "all", nonce: 0 });

  useEffect(() => {
    const onHashChange = () => {
      setSection(parseHashSection());
    };
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, []);

  const navigate = (next: NavSection) => {
    setSection(next);
    window.history.replaceState(null, "", `#/${next}`);
  };

  const authQuery = useQuery({
    queryKey: ["auth-status"],
    queryFn: () => api.auth.status(),
    retry: 1,
  });

  useEffect(() => {
    return onUnauthorized(() => {
      void queryClient.invalidateQueries({ queryKey: ["auth-status"] });
    });
  }, [queryClient]);

  const authenticated = authQuery.data?.authenticated === true;

  const overviewBadgeQuery = useQuery({
    queryKey: ["overview"],
    queryFn: () => api.overview.get(),
    enabled: authenticated,
    refetchInterval: 20_000,
  });

  const logoutMutation = useMutation({
    mutationFn: () => api.auth.logout(),
    onSuccess: () => {
      queryClient.clear();
      toast.info("Signed out");
      void authQuery.refetch();
    },
    onError: (err) => {
      toast.error("Sign out failed", errorMessage(err));
    },
  });

  const version = authQuery.data?.version || DEFAULT_VERSION;
  const repoUrl = authQuery.data?.repo_url || DEFAULT_REPO_URL;
  const license = authQuery.data?.license || DEFAULT_LICENSE;

  // Initial loading state
  if (authQuery.isLoading) {
    return (
      <div className="flex min-h-screen flex-col bg-zinc-50 text-zinc-900 dark:bg-zinc-950 dark:text-zinc-100">
        <div className="flex flex-1 items-center justify-center">
          <div className="flex items-center gap-2.5 text-xs text-zinc-500">
            <Loader2 className="h-4 w-4 animate-spin" />
            Connecting to FeedCourier gateway…
          </div>
        </div>
        <ComplianceFooter
          version={version}
          repoUrl={repoUrl}
          license={license}
        />
      </div>
    );
  }

  // Backend unreachable diagnostic state
  if (authQuery.isError) {
    return (
      <div className="flex min-h-screen flex-col bg-zinc-50 text-zinc-900 dark:bg-zinc-950 dark:text-zinc-100">
        <div className="flex flex-1 items-center justify-center px-4">
          <Card className="w-full max-w-md">
            <CardContent className="space-y-4 pt-6">
              <div className="flex items-start gap-3">
                <AlertTriangle className="mt-0.5 h-5 w-5 shrink-0 text-amber-500" />
                <div>
                  <h2 className="text-sm font-semibold">
                    Backend API Unreachable
                  </h2>
                  <p className="mt-1 text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">
                    Could not reach{" "}
                    <code className="font-mono">/api/v1/auth/status</code>:{" "}
                    {errorMessage(authQuery.error)}
                  </p>
                </div>
              </div>
              <Button
                className="w-full"
                variant="outline"
                onClick={() => void authQuery.refetch()}
                loading={authQuery.isFetching}
              >
                <RefreshCw className="h-4 w-4" />
                Retry Connection
              </Button>
            </CardContent>
          </Card>
        </div>
        <ComplianceFooter
          version={version}
          repoUrl={repoUrl}
          license={license}
        />
      </div>
    );
  }

  // Setup or Login Gate
  if (!authenticated) {
    return (
      <AuthGateView
        setupRequired={Boolean(authQuery.data?.setup_required)}
        version={version}
        repoUrl={repoUrl}
        license={license}
        onAuthenticated={() => {
          void queryClient.invalidateQueries({ queryKey: ["auth-status"] });
        }}
      />
    );
  }

  return (
    <AppShell
      activeSection={section}
      onNavigate={(next) => {
        if (next === "feeds") {
          setFeedFilterPreset({ status: "all", nonce: Date.now() });
        } else if (next === "tasks") {
          setTaskFilterPreset({ status: "all", nonce: Date.now() });
        }
        navigate(next);
      }}
      onLogout={() => logoutMutation.mutate()}
      loggingOut={logoutMutation.isPending}
      deadLetterCount={overviewBadgeQuery.data?.tasks.dead_letter ?? 0}
      circuitOpenCount={overviewBadgeQuery.data?.feeds.circuit_open ?? 0}
      version={version}
      repoUrl={repoUrl}
      license={license}
    >
      {section === "overview" && (
        <OverviewView
          onNavigateFeeds={(status) => {
            setFeedFilterPreset({
              status: status ?? "all",
              nonce: Date.now(),
            });
            navigate("feeds");
          }}
          onNavigateTasks={(status) => {
            setTaskFilterPreset({
              status: status ?? "all",
              nonce: Date.now(),
            });
            navigate("tasks");
          }}
          onNavigateSection={navigate}
        />
      )}

      {section === "feeds" && (
        <FeedsView
          key={`feeds-${feedFilterPreset.nonce}`}
          initialStatusFilter={feedFilterPreset.status}
        />
      )}

      {section === "channels" && <ChannelsView />}

      {section === "routes" && <RoutesView />}

      {section === "tasks" && (
        <TasksView
          key={`tasks-${taskFilterPreset.nonce}`}
          initialStatusFilter={taskFilterPreset.status}
        />
      )}

      {section === "settings" && <SettingsView />}
    </AppShell>
  );
}
