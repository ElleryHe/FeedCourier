<script setup lang="ts">
import { computed } from "vue";
import { data as market, type MarketPlugin } from "../../plugins.data";

const props = withDefaults(
  defineProps<{
    locale?: "en" | "zh";
  }>(),
  {
    locale: "en",
  },
);

const isZh = computed(() => props.locale === "zh");

function resolveDescription(plugin: MarketPlugin): string {
  const i18n = plugin.description_i18n;
  if (!i18n) return plugin.description;
  if (isZh.value) {
    return i18n["zh-CN"] || i18n.zh || plugin.description;
  }
  return i18n.en || plugin.description;
}

function kindLabel(plugin: MarketPlugin): string {
  if (plugin.kind === "builtin") return isZh.value ? "内置" : "Built-in";
  if (plugin.runtime === "js" && plugin.kind === "channel") {
    return isZh.value ? "JS · 通道" : "JS · Channel";
  }
  if (plugin.runtime === "js") {
    return isZh.value ? "JS · 处理" : "JS · Processor";
  }
  if (plugin.kind === "coming_soon") return isZh.value ? "即将支持" : "Coming Soon";
  return isZh.value ? "外部扩展" : "External";
}

function kindClass(plugin: MarketPlugin): string {
  if (plugin.kind === "builtin") return "fc-badge-builtin";
  if (plugin.runtime === "js") return "fc-badge-external";
  if (plugin.kind === "coming_soon") return "fc-badge-soon";
  return "fc-badge-external";
}

function resolveHomepage(plugin: MarketPlugin): string | undefined {
  if (!plugin.homepage) return undefined;
  let target = plugin.homepage.replace("/plugins/bridge-channels", "/plugins/js-plugins");
  if (isZh.value && target.startsWith("https://feedcourier.ellery.me/plugins/")) {
    return target.replace(
      "https://feedcourier.ellery.me/plugins/",
      "/zh/plugins/",
    );
  }
  if (!isZh.value && target.startsWith("https://feedcourier.ellery.me/plugins/")) {
    return target.replace("https://feedcourier.ellery.me", "");
  }
  return target;
}

function isExternalUrl(url?: string): boolean {
  return Boolean(url && /^https?:\/\//.test(url));
}
</script>

<template>
  <div class="fc-market">
    <div class="fc-market-meta">
      <span>
        {{
          isZh
            ? `共 ${market.plugins.length} 个插件 · 协议 v${market.schema_version}`
            : `${market.plugins.length} plugin(s) · Schema v${market.schema_version}`
        }}
      </span>
      <span v-if="market.generated_at">
        {{
          isZh
            ? `索引更新时间：${market.generated_at}`
            : `Index updated: ${market.generated_at}`
        }}
      </span>
      <span>
        {{
          market.source === "remote"
            ? isZh
              ? "来源：远端注册表"
              : "Source: Remote Registry"
            : isZh
              ? "来源：内置快照"
              : "Source: Bundled Snapshot"
        }}
      </span>
    </div>

    <div class="fc-plugin-grid">
      <article v-for="plugin in market.plugins" :key="plugin.id" class="fc-plugin-card">
        <div class="fc-plugin-header">
          <div>
            <h3 class="fc-plugin-title">{{ plugin.name }}</h3>
            <code class="fc-plugin-id">{{ plugin.id }}</code>
          </div>
          <span class="fc-badge" :class="kindClass(plugin)">
            {{ kindLabel(plugin) }}
          </span>
        </div>

        <p class="fc-plugin-desc">{{ resolveDescription(plugin) }}</p>

        <div class="fc-plugin-tags">
          <span class="fc-tag">v{{ plugin.latest || plugin.version }}</span>
          <span v-if="plugin.min_core" class="fc-tag">
            {{ isZh ? `核心 >= ${plugin.min_core}` : `Core >= ${plugin.min_core}` }}
          </span>
          <span class="fc-tag">{{ plugin.category }}</span>
          <span v-for="hook in plugin.hooks" :key="hook" class="fc-tag fc-tag-hook">
            {{ hook }}
          </span>
          <span v-for="host in plugin.allowed_hosts" :key="host" class="fc-tag fc-tag-perm">
            {{ host }}
          </span>
          <span v-for="perm in plugin.permissions" :key="perm" class="fc-tag">
            {{ isZh ? `权限:${perm}` : `perm:${perm}` }}
          </span>
        </div>

        <div class="fc-plugin-links">
          <a
            v-if="resolveHomepage(plugin)"
            :href="resolveHomepage(plugin)"
            :target="isExternalUrl(resolveHomepage(plugin)) ? '_blank' : undefined"
            :rel="isExternalUrl(resolveHomepage(plugin)) ? 'noopener noreferrer' : undefined"
            class="fc-link-primary"
          >
            {{ isZh ? "使用文档 →" : "Documentation →" }}
          </a>
          <a
            v-if="plugin.repository"
            :href="plugin.repository"
            target="_blank"
            rel="noopener noreferrer"
            class="fc-link-secondary"
          >
            {{ isZh ? "源码仓库 ↗" : "Repository ↗" }}
          </a>
        </div>
      </article>
    </div>
  </div>
</template>
