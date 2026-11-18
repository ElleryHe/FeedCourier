import {
  Activity,
  GitFork,
  Inbox,
  LogOut,
  Menu,
  Monitor,
  Moon,
  Radio,
  Rss,
  Settings,
  Sun,
  X,
} from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import { LocaleSwitchButton, useI18n } from "@/i18n";
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

export type ThemeMode = "system" | "light" | "dark";

const THEME_STORAGE_KEY = "fc-theme";

function readStoredThemeMode(): ThemeMode {
  if (typeof window === "undefined") return "system";
  try {
    const saved = window.localStorage.getItem(THEME_STORAGE_KEY);
    if (saved === "light" || saved === "dark" || saved === "system") {
      return saved;
    }
  } catch {
    // Ignore storage access errors
  }
  return "system";
}

function resolveIsDark(mode: ThemeMode): boolean {
  if (typeof window === "undefined") return false;
  if (mode === "dark") return true;
  if (mode === "light") return false;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

function applyDomTheme(isDark: boolean): void {
  if (typeof document === "undefined") return;
  document.documentElement.classList.toggle("dark", isDark);
}

let currentThemeMode: ThemeMode = readStoredThemeMode();
const themeSubscribers = new Set<() => void>();
let mediaListenerAttached = false;

function ensureSystemMediaListener(): void {
  if (mediaListenerAttached || typeof window === "undefined") return;
  mediaListenerAttached = true;
  applyDomTheme(resolveIsDark(currentThemeMode));
  const mql = window.matchMedia("(prefers-color-scheme: dark)");
  const onChange = () => {
    if (currentThemeMode === "system") {
      applyDomTheme(resolveIsDark("system"));
      for (const listener of themeSubscribers) listener();
    }
  };
  mql.addEventListener("change", onChange);
}

function updateThemeMode(next: ThemeMode): void {
  currentThemeMode = next;
  try {
    if (next === "system") {
      window.localStorage.removeItem(THEME_STORAGE_KEY);
    } else {
      window.localStorage.setItem(THEME_STORAGE_KEY, next);
    }
  } catch {
    // Ignore storage access errors
  }
  applyDomTheme(resolveIsDark(next));
  for (const listener of themeSubscribers) listener();
}

export function useThemeMode() {
  ensureSystemMediaListener();
  const [, setTick] = useState(0);

  useEffect(() => {
    const listener = () => setTick((t) => t + 1);
    themeSubscribers.add(listener);
    applyDomTheme(resolveIsDark(currentThemeMode));
    return () => {
      themeSubscribers.delete(listener);
    };
  }, []);

  const dark = resolveIsDark(currentThemeMode);
  const toggleTheme = () => {
    const next: ThemeMode =
      currentThemeMode === "system"
        ? "light"
        : currentThemeMode === "light"
          ? "dark"
          : "system";
    updateThemeMode(next);
  };

  return {
    themeMode: currentThemeMode,
    dark,
    setThemeMode: updateThemeMode,
    toggleTheme,
  };
}

export function FeedCourierMark({
  className = "h-7 w-7",
}: {
  className?: string;
}) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 32 32"
      fill="none"
      aria-hidden="true"
      className={className}
    >
      <circle
        cx="6.5"
        cy="25.5"
        r="2.5"
        className="fill-emerald-500 dark:fill-emerald-400"
      />
      <path
        d="M4.5 18.5A9 9 0 0 1 13.5 27.5"
        strokeWidth="2.5"
        strokeLinecap="round"
        className="stroke-zinc-900 dark:stroke-zinc-50"
      />
      <path
        d="M4.5 12.5A15 15 0 0 1 19.5 27.5"
        strokeWidth="2.5"
        strokeLinecap="round"
        className="stroke-zinc-700 dark:stroke-zinc-400"
      />
      <path
        d="M11 10L28.5 3.5L18.5 13.5Z"
        strokeWidth="0.75"
        strokeLinejoin="round"
        className="fill-emerald-500 stroke-emerald-500 dark:fill-emerald-400 dark:stroke-emerald-400"
      />
      <path
        d="M18.5 13.5L28.5 3.5L22 21Z"
        strokeWidth="0.75"
        strokeLinejoin="round"
        className="fill-emerald-600 stroke-emerald-600 dark:fill-emerald-500 dark:stroke-emerald-500"
      />
    </svg>
  );
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
  const { themeMode, toggleTheme } = useThemeMode();
  const { m } = useI18n();

  const navItems: NavItem[] = [
    {
      id: "overview",
      label: m.shell.nav.overview.label,
      subtitle: m.shell.nav.overview.subtitle,
      icon: Activity,
    },
    {
      id: "feeds",
      label: m.shell.nav.feeds.label,
      subtitle: m.shell.nav.feeds.subtitle,
      icon: Rss,
      badge: circuitOpenCount > 0 ? circuitOpenCount : undefined,
      badgeAlert: circuitOpenCount > 0,
    },
    {
      id: "channels",
      label: m.shell.nav.channels.label,
      subtitle: m.shell.nav.channels.subtitle,
      icon: Radio,
    },
    {
      id: "routes",
      label: m.shell.nav.routes.label,
      subtitle: m.shell.nav.routes.subtitle,
      icon: GitFork,
    },
    {
      id: "tasks",
      label: m.shell.nav.tasks.label,
      subtitle: m.shell.nav.tasks.subtitle,
      icon: Inbox,
      badge: deadLetterCount > 0 ? deadLetterCount : undefined,
      badgeAlert: deadLetterCount > 0,
    },
    {
      id: "settings",
      label: m.shell.nav.settings.label,
      subtitle: m.shell.nav.settings.subtitle,
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
            <FeedCourierMark className="h-8 w-8 shrink-0" />
            <div>
              <div className="flex items-center gap-1.5">
                <span className="text-sm font-bold tracking-tight text-zinc-900 dark:text-zinc-100">
                  FeedCourier
                </span>
              </div>
              <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
                {m.shell.subtitle}
              </p>
            </div>
          </div>
          <button
            type="button"
            onClick={() => setMobileMenuOpen(false)}
            aria-label={m.shell.closeNav}
            className="rounded-md p-1 text-zinc-500 hover:bg-zinc-100 lg:hidden dark:hover:bg-zinc-800"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {/* Navigation Items */}
        <nav className="space-y-1 p-3" aria-label={m.shell.mainNav}>
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
        <LocaleSwitchButton className="w-full" />
        <button
          type="button"
          onClick={toggleTheme}
          title={m.theme.toggleTitle}
          aria-label={m.theme.toggleTitle}
          className="flex w-full cursor-pointer items-center justify-center gap-1.5 rounded-md border border-zinc-200 bg-white px-2.5 py-1.5 text-xs font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-300 dark:hover:bg-zinc-800"
        >
          {themeMode === "system" ? (
            <>
              <Monitor className="h-3.5 w-3.5 text-emerald-500" />
              {m.theme.system}
            </>
          ) : themeMode === "light" ? (
            <>
              <Sun className="h-3.5 w-3.5 text-amber-500" />
              {m.theme.light}
            </>
          ) : (
            <>
              <Moon className="h-3.5 w-3.5 text-indigo-400" />
              {m.theme.dark}
            </>
          )}
        </button>
        <button
          type="button"
          onClick={onLogout}
          disabled={loggingOut}
          className="flex w-full cursor-pointer items-center justify-center gap-1.5 rounded-md border border-zinc-200 bg-white px-2.5 py-1.5 text-xs font-medium text-zinc-700 hover:border-rose-200 hover:bg-rose-50 hover:text-rose-700 disabled:opacity-50 dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-300 dark:hover:border-rose-900 dark:hover:bg-rose-950/50 dark:hover:text-rose-300"
        >
          <LogOut className="h-3.5 w-3.5" />
          {m.shell.signOut}
        </button>
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
              aria-label={m.shell.openNav}
              className="rounded-md border border-zinc-200 p-1.5 text-zinc-700 dark:border-zinc-800 dark:text-zinc-200"
            >
              <Menu className="h-4 w-4" />
            </button>
            <FeedCourierMark className="h-5 w-5 shrink-0" />
            <span className="text-sm font-bold tracking-tight">
              FeedCourier
            </span>
          </div>
          <div className="flex items-center gap-2">
            <span className="rounded bg-zinc-100 px-2 py-0.5 text-xs font-medium dark:bg-zinc-800">
              {m.shell.nav[activeSection].label}
            </span>
            <LocaleSwitchButton />
          </div>
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
