import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export interface MarketPluginVersion {
  version: string;
  min_core?: string;
  path: string;
  sha256: string;
  changelog_path?: string;
  changelog?: string;
}

export interface MarketPlugin {
  id: string;
  name: string;
  version: string;
  api_version: string;
  kind: "builtin" | "processor" | "channel" | "external" | "coming_soon" | string;
  runtime?: "builtin" | "js" | string;
  min_core?: string;
  category: string;
  icon: string;
  description: string;
  description_i18n?: Record<string, string>;
  homepage?: string;
  repository?: string;
  hooks: string[];
  permissions: string[];
  allowed_hosts: string[];
  latest?: string;
  versions: MarketPluginVersion[];
}

export interface PluginsMarketData {
  schema_version: number;
  generated_at: string | null;
  source: "remote" | "snapshot";
  index_url: string;
  plugins: MarketPlugin[];
}

declare const data: PluginsMarketData;
export { data };

const DEFAULT_INDEX_URL =
  process.env.PLUGIN_INDEX_URL?.trim() ||
  "https://raw.githubusercontent.com/ElleryHe/FeedCourier-Plugins/main/index.json";

const __dirname = dirname(fileURLToPath(import.meta.url));
const SNAPSHOT_PATH = resolve(__dirname, "../../src/plugins/market_snapshot.json");

function normalizePlugin(raw: Partial<MarketPlugin>): MarketPlugin {
  const version = typeof raw.version === "string" ? raw.version : "0.0.0";
  const latest = typeof raw.latest === "string" && raw.latest ? raw.latest : version;
  const runtime =
    raw.runtime === "builtin" || raw.runtime === "js"
      ? raw.runtime
      : raw.kind === "builtin"
        ? "builtin"
        : "js";

  return {
    id: String(raw.id ?? ""),
    name: String(raw.name ?? raw.id ?? ""),
    version,
    api_version: typeof raw.api_version === "string" ? raw.api_version : "1",
    kind: typeof raw.kind === "string" ? raw.kind : "channel",
    runtime,
    min_core: typeof raw.min_core === "string" ? raw.min_core : undefined,
    category: typeof raw.category === "string" ? raw.category : "general",
    icon: typeof raw.icon === "string" ? raw.icon : "puzzle",
    description: typeof raw.description === "string" ? raw.description : "",
    description_i18n:
      raw.description_i18n && typeof raw.description_i18n === "object"
        ? raw.description_i18n
        : undefined,
    homepage: typeof raw.homepage === "string" ? raw.homepage : undefined,
    repository: typeof raw.repository === "string" ? raw.repository : undefined,
    hooks: Array.isArray(raw.hooks) ? raw.hooks.map(String) : [],
    permissions: Array.isArray(raw.permissions) ? raw.permissions.map(String) : [],
    allowed_hosts: Array.isArray(raw.allowed_hosts)
      ? raw.allowed_hosts.map(String)
      : [],
    latest,
    versions: Array.isArray(raw.versions) ? raw.versions : [],
  };
}

function readBundledSnapshot(indexUrl: string): PluginsMarketData {
  const raw = readFileSync(SNAPSHOT_PATH, "utf-8");
  const parsed = JSON.parse(raw) as {
    schema_version?: number;
    generated_at?: string | null;
    plugins?: Partial<MarketPlugin>[];
  };
  return {
    schema_version: typeof parsed.schema_version === "number" ? parsed.schema_version : 1,
    generated_at: typeof parsed.generated_at === "string" ? parsed.generated_at : null,
    source: "snapshot",
    index_url: indexUrl,
    plugins: Array.isArray(parsed.plugins)
      ? parsed.plugins.map(normalizePlugin)
      : [],
  };
}

export default {
  watch: ["../../src/plugins/market_snapshot.json"],
  async load(): Promise<PluginsMarketData> {
    const indexUrl = DEFAULT_INDEX_URL;
    try {
      const response = await fetch(indexUrl, {
        signal: AbortSignal.timeout(4000),
        headers: {
          Accept: "application/json",
          "User-Agent": "FeedCourier-Docs-Builder/0.3.1",
        },
      });
      if (response.ok) {
        const parsed = (await response.json()) as {
          schema_version?: number;
          generated_at?: string | null;
          plugins?: Partial<MarketPlugin>[];
        };
        if (parsed && Array.isArray(parsed.plugins)) {
          return {
            schema_version:
              typeof parsed.schema_version === "number" ? parsed.schema_version : 1,
            generated_at:
              typeof parsed.generated_at === "string" ? parsed.generated_at : null,
            source: "remote",
            index_url: indexUrl,
            plugins: parsed.plugins.map(normalizePlugin),
          };
        }
      }
    } catch {
      // Offline build, network timeout, or unpushed remote repository: fall back cleanly.
    }

    return readBundledSnapshot(indexUrl);
  },
};
