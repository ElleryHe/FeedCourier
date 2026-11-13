import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";
import { ApiError } from "@/api/client";

export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}

export function errorMessage(err: unknown): string {
  if (err instanceof ApiError) {
    return err.details ? `${err.message} (${err.details})` : err.message;
  }
  if (err instanceof Error) {
    return err.message;
  }
  return "Unexpected error occurred";
}

function parseDateValue(val: string | number | null | undefined): Date | null {
  if (val === null || val === undefined || val === "") return null;
  if (typeof val === "number") {
    // Unix epoch seconds vs milliseconds
    const ms = val < 1e12 ? val * 1000 : val;
    const d = new Date(ms);
    return Number.isNaN(d.getTime()) ? null : d;
  }
  const numeric = Number(val);
  if (Number.isFinite(numeric) && /^\d+$/.test(val.trim())) {
    const ms = numeric < 1e12 ? numeric * 1000 : numeric;
    const d = new Date(ms);
    return Number.isNaN(d.getTime()) ? null : d;
  }
  const d = new Date(val);
  return Number.isNaN(d.getTime()) ? null : d;
}

export function formatDateTime(
  value: string | number | null | undefined,
): string {
  const d = parseDateValue(value);
  if (!d) return value ? String(value) : "—";
  return d.toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
}

export function formatRelativeTime(
  value: string | number | null | undefined,
): string {
  const d = parseDateValue(value);
  if (!d) return value ? String(value) : "—";
  const diffSec = Math.round((d.getTime() - Date.now()) / 1000);
  const absSec = Math.abs(diffSec);

  if (absSec < 5) return "just now";
  if (absSec < 60) {
    return diffSec > 0 ? `in ${absSec}s` : `${absSec}s ago`;
  }
  const absMin = Math.round(absSec / 60);
  if (absMin < 60) {
    return diffSec > 0 ? `in ${absMin}m` : `${absMin}m ago`;
  }
  const absHr = Math.round(absMin / 60);
  if (absHr < 48) {
    return diffSec > 0 ? `in ${absHr}h` : `${absHr}h ago`;
  }
  const absDays = Math.round(absHr / 24);
  return diffSec > 0 ? `in ${absDays}d` : `${absDays}d ago`;
}

export function formatDurationSecs(secs: number): string {
  if (secs <= 0) return "0s";
  if (secs < 60) return `${secs}s`;
  if (secs % 3600 === 0) return `${secs / 3600}h`;
  if (secs % 60 === 0) return `${secs / 60}m`;
  const mins = Math.floor(secs / 60);
  const rem = secs % 60;
  return `${mins}m ${rem}s`;
}

export function isValidHttpUrl(raw: string): boolean {
  try {
    const parsed = new URL(raw.trim());
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

export function isValidProxyUrl(raw: string): boolean {
  try {
    const parsed = new URL(raw.trim());
    return ["http:", "https:", "socks5:", "socks5h:"].includes(parsed.protocol);
  } catch {
    return false;
  }
}

export function validateRegex(pattern: string): string | null {
  const trimmed = pattern.trim();
  if (!trimmed) return null;
  try {
    new RegExp(trimmed);
    return null;
  } catch (err) {
    return err instanceof Error ? err.message : "Invalid regular expression";
  }
}

export async function copyToClipboard(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const textarea = document.createElement("textarea");
    textarea.value = text;
    textarea.style.position = "fixed";
    textarea.style.opacity = "0";
    document.body.appendChild(textarea);
    textarea.select();
    const ok = document.execCommand("copy");
    textarea.remove();
    return ok;
  }
}
