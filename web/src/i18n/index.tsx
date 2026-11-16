import { clsx } from "clsx";
import { Languages } from "lucide-react";
import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
} from "react";
import { en, type Messages } from "./en";
import { zhCN } from "./zh";

export type Locale = "en" | "zh-CN";

const STORAGE_KEY = "fc-locale";

function detectInitialLocale(): Locale {
  if (typeof window === "undefined") return "en";
  try {
    const saved = window.localStorage.getItem(STORAGE_KEY);
    if (saved === "en" || saved === "zh-CN") {
      return saved;
    }
  } catch {
    // Ignore storage access errors in restricted webviews
  }
  const candidates =
    typeof navigator !== "undefined"
      ? navigator.languages?.length
        ? navigator.languages
        : [navigator.language]
      : [];
  for (const lang of candidates) {
    if (lang && lang.toLowerCase().startsWith("zh")) {
      return "zh-CN";
    }
  }
  return "en";
}

let activeLocale: Locale = detectInitialLocale();

export function getCurrentLocale(): Locale {
  return activeLocale;
}

export function getMessages(locale: Locale = activeLocale): Messages {
  return locale === "zh-CN" ? zhCN : en;
}

interface I18nContextValue {
  locale: Locale;
  setLocale: (next: Locale) => void;
  toggleLocale: () => void;
  m: Messages;
}

const I18nContext = createContext<I18nContextValue | null>(null);

export function I18nProvider({ children }: { children: ReactNode }) {
  const [locale, setLocaleState] = useState<Locale>(activeLocale);

  const setLocale = useCallback((next: Locale) => {
    activeLocale = next;
    setLocaleState(next);
    try {
      window.localStorage.setItem(STORAGE_KEY, next);
    } catch {
      // Ignore storage access errors
    }
  }, []);

  const toggleLocale = useCallback(() => {
    setLocale(locale === "zh-CN" ? "en" : "zh-CN");
  }, [locale, setLocale]);

  const m = locale === "zh-CN" ? zhCN : en;

  useEffect(() => {
    activeLocale = locale;
    document.documentElement.lang = locale;
    document.title = m.locale.consoleTitle;
  }, [locale, m.locale.consoleTitle]);

  const value = useMemo<I18nContextValue>(
    () => ({ locale, setLocale, toggleLocale, m }),
    [locale, setLocale, toggleLocale, m],
  );

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18nContextValue {
  const ctx = useContext(I18nContext);
  if (!ctx) {
    throw new Error("useI18n must be used within an I18nProvider");
  }
  return ctx;
}

export function LocaleSwitchButton({ className }: { className?: string }) {
  const { toggleLocale, m } = useI18n();
  return (
    <button
      type="button"
      onClick={toggleLocale}
      title={m.locale.switchTitle}
      aria-label={m.locale.switchTitle}
      className={clsx(
        "inline-flex cursor-pointer items-center justify-center gap-1.5 rounded-md border border-zinc-200 bg-white px-2.5 py-1.5 text-xs font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-300 dark:hover:bg-zinc-800",
        className,
      )}
    >
      <Languages className="h-3.5 w-3.5 text-zinc-500 dark:text-zinc-400" />
      <span>{m.locale.switchLabel}</span>
    </button>
  );
}
