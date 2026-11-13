import { ExternalLink, Scale, Terminal } from "lucide-react";
import {
  DEFAULT_LICENSE,
  DEFAULT_REPO_URL,
  DEFAULT_VERSION,
} from "@/api/types";

export interface ComplianceFooterProps {
  version?: string;
  repoUrl?: string;
  license?: string;
}

/**
 * Persistent AGPLv3 Section 13 compliance footer rendered across all views
 * (including initial setup & login gates) so remote network users always have
 * direct access to the Corresponding Source repository and build metadata.
 */
export function ComplianceFooter({
  version = DEFAULT_VERSION,
  repoUrl = DEFAULT_REPO_URL,
  license = DEFAULT_LICENSE,
}: ComplianceFooterProps) {
  const safeRepoUrl = repoUrl.trim() || DEFAULT_REPO_URL;
  const normalizedVersion = version.startsWith("v") ? version : `v${version}`;

  return (
    <footer className="border-t border-zinc-200/80 bg-white/80 px-4 py-3 text-xs text-zinc-500 backdrop-blur-xs dark:border-zinc-800/80 dark:bg-zinc-950/80 dark:text-zinc-400">
      <div className="mx-auto flex max-w-6xl flex-wrap items-center justify-between gap-2">
        <div className="flex flex-wrap items-center gap-2">
          <span className="inline-flex items-center gap-1.5 font-semibold text-zinc-800 dark:text-zinc-200">
            <Terminal className="h-3.5 w-3.5 text-emerald-600 dark:text-emerald-400" />
            FeedCourier
          </span>
          <span className="rounded bg-zinc-100 px-1.5 py-0.5 font-mono text-[11px] text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300">
            {normalizedVersion}
          </span>
          <span className="text-zinc-300 dark:text-zinc-700">·</span>
          <span className="inline-flex items-center gap-1 font-mono text-[11px]">
            <Scale className="h-3 w-3" />
            {license || DEFAULT_LICENSE}
          </span>
        </div>

        <a
          href={safeRepoUrl}
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center gap-1 font-medium text-zinc-600 underline-offset-4 hover:text-zinc-900 hover:underline dark:text-zinc-300 dark:hover:text-white"
        >
          Source Repository
          <ExternalLink className="h-3 w-3" />
        </a>
      </div>
    </footer>
  );
}
