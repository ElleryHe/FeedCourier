import {
  Activity,
  GitFork,
  Inbox,
  LogOut,
  Menu,
  Moon,
  Radio,
  Rss,
  Settings,
  Sun,
  X,
} from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import { cn } from "@/lib/utils";
import { ComplianceFooter } from "./ComplianceFooter";

export type NavSection =
  | "overview"
  | "feeds"
  | "channels"
  | "routes"
  | "tasks"
  | "settings";

interface NavItem {
  id: NavSection;
  label: string;
  subtitle: string;
  icon: typeof Activity;
  badge?: number;
  badgeAlert?: boolean;
}

export interface AppShellProps {
  activeSection: NavSection;
  onNavigate: (section: NavSection) => void;
  onLogout: () => void;
  loggingOut?: boolean;
  deadLetterCount?: number;
  circuitOpenCount?: number;
  version?: string;
  repoUrl?: string;
  license?: string;
  children: ReactNode;
}

function getInitialDarkMode(): boolean {
  if (typeof window === "undefined") return false;
  const saved = window.localStorage.getItem("fc-theme");
  if (saved === "dark") return true;
  if (saved === "light") return false;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

export function useThemeMode() {
  const [dark, setDark] = useState<boolean>(getInitialDarkMode);

  useEffect(() => {
    const root = document.documentElement;
    if (dark) {
      root.classList.add("dark");
      window.localStorage.setItem("fc-theme", "dark");
    } else {
      root.classList.remove("dark");
      window.localStorage.setItem("fc-theme", "light");
    }
  }, [dark]);

  return { dark, toggleTheme: () => setDark((prev) => !prev) };
}

export function AppShell({
  activeSection,
  onNavigate,
  onLogout,
  loggingOut = false,
  deadLetterCount = 0,
  circuitOpenCount = 0,
  version,
  repoUrl,
  license,
  children,
}: AppShellProps) {
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false);
  const { dark, toggleTheme } = useThemeMode();

  const navItems: NavItem[] = [
    {
      id: "overview",
      label: "Overview",
      subtitle: "Telemetry & Snapshot",
      icon: Activity,
    },
    {
      id: "feeds",
      label: "Feeds",
      subtitle: "RSS / Atom / Readability",
      icon: Rss,
      badge: circuitOpenCount > 0 ? circuitOpenCount : undefined,
      badgeAlert: circuitOpenCount > 0,
    },
    {
      id: "channels",
      label: "Channels",
      subtitle: "SMTP · Telegram · Notion",
      icon: Radio,
    },
    {
      id: "routes",
      label: "Routes",
      subtitle: "Filters & Delivery Modes",
      icon: GitFork,
    },
    {
      id: "tasks",
      label: "Outbox",
      subtitle: "Leases & Dead-Letter",
      icon: Inbox,
      badge: deadLetterCount > 0 ? deadLetterCount : undefined,
      badgeAlert: deadLetterCount > 0,
    },
    {
      id: "settings",
      label: "System & MCP",
      subtitle: "Proxy · SSRF · AI Tokens",
      icon: Settings,
    },
  ];

  const handleSelect = (id: NavSection) => {
    onNavigate(id);
    setMobileMenuOpen(false);
  };

  const sidebarBody = (
    <div className="flex h-full flex-col justify-between">
      <div>
        {/* Brand Header */}
        <div className="flex items-center justify-between border-b border-zinc-200/80 px-4 py-4 dark:border-zinc-800/80">
          <div className="flex items-center gap-2.5">
            <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-zinc-900 text-emerald-400 shadow-xs dark:bg-zinc-800">
              <Rss className="h-4 w-4" />
            </div>
            <div>
              <div className="flex items-center gap-1.5">
                <span className="text-sm font-bold tracking-tight text-zinc-900 dark:text-zinc-100">
                  FeedCourier
                </span>
              </div>
              <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
                Delivery Gateway
              </p>
            </div>
          </div>
          <button
            type="button"
            onClick={() => setMobileMenuOpen(false)}
            aria-label="Close navigation menu"
            className="rounded-md p-1 text-zinc-500 hover:bg-zinc-100 lg:hidden dark:hover:bg-zinc-800"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {/* Navigation Items */}
        <nav className="space-y-1 p-3" aria-label="Main navigation">
          {navItems.map((item) => {
            const Icon = item.icon;
            const active = activeSection === item.id;
            return (
              <button
                key={item.id}
                type="button"
                onClick={() => handleSelect(item.id)}
                className={cn(
                  "group flex w-full cursor-pointer items-center justify-between rounded-lg px-3 py-2 text-left transition-colors",
                  active
                    ? "bg-zinc-900 text-white shadow-2xs dark:bg-zinc-100 dark:text-zinc-950"
                    : "text-zinc-700 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800/70",
                )}
              >
                <div className="flex min-w-0 items-center gap-2.5">
                  <Icon
                    className={cn(
                      "h-4 w-4 shrink-0",
                      active
                        ? "text-emerald-400 dark:text-emerald-600"
                        : "text-zinc-400 group-hover:text-zinc-700 dark:text-zinc-500 dark:group-hover:text-zinc-200",
                    )}
                  />
                  <div className="min-w-0">
                    <div className="truncate text-xs font-semibold">
                      {item.label}
                    </div>
                    <div
                      className={cn(
                        "truncate text-[10px]",
                        active
                          ? "text-zinc-300 dark:text-zinc-600"
                          : "text-zinc-400 dark:text-zinc-500",
                      )}
                    >
                      {item.subtitle}
                    </div>
                  </div>
                </div>
                {item.badge !== undefined && (
                  <span
                    className={cn(
                      "ml-2 rounded-full px-1.5 py-0.5 font-mono text-[10px] font-semibold",
                      item.badgeAlert
                        ? "bg-rose-500 text-white"
                        : "bg-zinc-200 text-zinc-700 dark:bg-zinc-700 dark:text-zinc-200",
                    )}
                  >
                    {item.badge}
                  </span>
                )}
              </button>
            );
          })}
        </nav>
      </div>

      {/* Bottom Controls */}
      <div className="space-y-1.5 border-t border-zinc-200/80 p-3 dark:border-zinc-800/80">
        <div className="flex items-center gap-1.5">
          <button
            type="button"
            onClick={toggleTheme}
            className="flex flex-1 cursor-pointer items-center justify-center gap-1.5 rounded-md border border-zinc-200 bg-white px-2.5 py-1.5 text-xs font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-300 dark:hover:bg-zinc-800"
          >
            {dark ? (
              <>
                <Sun className="h-3.5 w-3.5 text-amber-400" />
                Light
              </>
            ) : (
              <>
                <Moon className="h-3.5 w-3.5 text-zinc-500" />
                Dark
              </>
            )}
          </button>
          <button
            type="button"
            onClick={onLogout}
            disabled={loggingOut}
            className="flex flex-1 cursor-pointer items-center justify-center gap-1.5 rounded-md border border-zinc-200 bg-white px-2.5 py-1.5 text-xs font-medium text-zinc-700 hover:border-rose-200 hover:bg-rose-50 hover:text-rose-700 disabled:opacity-50 dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-300 dark:hover:border-rose-900 dark:hover:bg-rose-950/50 dark:hover:text-rose-300"
          >
            <LogOut className="h-3.5 w-3.5" />
            Sign out
          </button>
        </div>
      </div>
    </div>
  );

  return (
    <div className="flex min-h-screen bg-zinc-50 text-zinc-900 dark:bg-zinc-950 dark:text-zinc-100">
      {/* Desktop Sidebar */}
      <aside className="hidden w-60 shrink-0 border-r border-zinc-200/80 bg-white lg:fixed lg:inset-y-0 lg:left-0 lg:z-20 lg:block dark:border-zinc-800/80 dark:bg-zinc-900/80">
        {sidebarBody}
      </aside>

      {/* Mobile Sidebar Drawer */}
      {mobileMenuOpen && (
        <div className="fixed inset-0 z-30 lg:hidden">
          <div
            className="fixed inset-0 bg-zinc-950/50 backdrop-blur-[1px]"
            onClick={() => setMobileMenuOpen(false)}
            aria-hidden="true"
          />
          <aside className="fixed inset-y-0 left-0 z-40 w-64 border-r border-zinc-200 bg-white shadow-xl dark:border-zinc-800 dark:bg-zinc-900">
            {sidebarBody}
          </aside>
        </div>
      )}

      {/* Main Content Area */}
      <div className="flex min-w-0 flex-1 flex-col lg:pl-60">
        {/* Mobile Top Bar */}
        <header className="sticky top-0 z-10 flex items-center justify-between border-b border-zinc-200/80 bg-white/90 px-4 py-3 backdrop-blur-xs lg:hidden dark:border-zinc-800/80 dark:bg-zinc-900/90">
          <div className="flex items-center gap-2.5">
            <button
              type="button"
              onClick={() => setMobileMenuOpen(true)}
              aria-label="Open navigation menu"
              className="rounded-md border border-zinc-200 p-1.5 text-zinc-700 dark:border-zinc-800 dark:text-zinc-200"
            >
              <Menu className="h-4 w-4" />
            </button>
            <span className="text-sm font-bold tracking-tight">
              FeedCourier
            </span>
          </div>
          <span className="rounded bg-zinc-100 px-2 py-0.5 text-xs font-medium capitalize dark:bg-zinc-800">
            {activeSection}
          </span>
        </header>

        <main className="mx-auto w-full max-w-6xl flex-1 px-4 py-6 sm:px-6 lg:px-8">
          {children}
        </main>

        <ComplianceFooter
          version={version}
          repoUrl={repoUrl}
          license={license}
        />
      </div>
    </div>
  );
}
