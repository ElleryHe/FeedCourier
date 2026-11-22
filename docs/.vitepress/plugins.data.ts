import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export interface MarketPlugin {
  id: string;
  name: string;
  version: string;
  api_version: string;
  kind: "builtin" | "external" | "coming_soon" | string;
  min_core?: string;
  category: string;
  icon: string;
  description: string;
  homepage?: string;
  repository?: string;
  hooks: string[];
  permissions: string[];
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

function readBundledSnapshot(indexUrl: string): PluginsMarketData {
  const raw = readFileSync(SNAPSHOT_PATH, "utf-8");
  const parsed = JSON.parse(raw) as {
    schema_version?: number;
    generated_at?: string | null;
    plugins?: MarketPlugin[];
  };
  return {
    schema_version: typeof parsed.schema_version === "number" ? parsed.schema_version : 1,
    generated_at: typeof parsed.generated_at === "string" ? parsed.generated_at : null,
    source: "snapshot",
    index_url: indexUrl,
    plugins: Array.isArray(parsed.plugins) ? parsed.plugins : [],
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
          "User-Agent": "FeedCourier-Docs-Builder/0.3.0",
        },
      });
      if (response.ok) {
        const parsed = (await response.json()) as {
          schema_version?: number;
          generated_at?: string | null;
          plugins?: MarketPlugin[];
        };
        if (parsed && Array.isArray(parsed.plugins)) {
          return {
            schema_version:
              typeof parsed.schema_version === "number" ? parsed.schema_version : 1,
            generated_at:
              typeof parsed.generated_at === "string" ? parsed.generated_at : null,
            source: "remote",
            index_url: indexUrl,
            plugins: parsed.plugins,
          };
        }
      }
    } catch {
      // Offline build, network timeout, or unpushed remote repository: fall back cleanly.
    }

    return readBundledSnapshot(indexUrl);
  },
};
