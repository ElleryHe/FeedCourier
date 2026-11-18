import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import {
  defineConfig,
  type DefaultTheme,
  type HeadConfig,
  type SiteConfig,
} from "vitepress";
import { withMermaid } from "vitepress-plugin-mermaid";

const SITE_ORIGIN = "https://feedcourier.ellery.me";
const OG_IMAGE_URL = `${SITE_ORIGIN}/og-image.png`;
const currentYear = new Date().getFullYear();
const copyrightHtml = `Copyright © <span data-current-year>${currentYear}</span> FeedCourier`;

const enUserGuideSidebar: DefaultTheme.SidebarItem[] = [
  {
    text: "Getting Started",
    items: [
      { text: "Project Overview", link: "/guide/introduction" },
      { text: "Quick Start", link: "/guide/quick-start" },
      { text: "Configuration & Proxy", link: "/guide/configuration" },
      { text: "Backup & Maintenance", link: "/guide/operations" },
    ],
  },
  {
    text: "Delivery Channels",
    items: [
      {
        text: "Send Rate, Quotas & Filters",
        link: "/channels/pacing-and-quotas",
      },
      { text: "SMTP Email", link: "/channels/smtp" },
      { text: "Telegram", link: "/channels/telegram" },
      { text: "Notion", link: "/channels/notion" },
    ],
  },
  {
    text: "MCP Integration",
    items: [
      { text: "Client Setup & Security", link: "/mcp/setup" },
      { text: "Tools Overview", link: "/mcp/tools" },
    ],
  },
];

const enArchitectureSidebar: DefaultTheme.SidebarItem[] = [
  {
    text: "Architecture & Development",
    items: [
      { text: "System Architecture", link: "/reference/architecture" },
      {
        text: "Channel Protocols & Delivery",
        link: "/reference/channel-protocols",
      },
      { text: "Security & Isolation", link: "/reference/security" },
      { text: "Database Schema", link: "/reference/database-schema" },
      { text: "REST API", link: "/reference/rest-api" },
      { text: "Build & Release Pipeline", link: "/reference/releasing" },
    ],
  },
];

const zhUserGuideSidebar: DefaultTheme.SidebarItem[] = [
  {
    text: "入门与部署",
    items: [
      { text: "项目简介", link: "/zh/guide/introduction" },
      { text: "快速开始", link: "/zh/guide/quick-start" },
      { text: "环境配置与代理", link: "/zh/guide/configuration" },
      { text: "数据备份与维护", link: "/zh/guide/operations" },
    ],
  },
  {
    text: "推送通道设置",
    items: [
      {
        text: "发送频率、配额与过滤",
        link: "/zh/channels/pacing-and-quotas",
      },
      { text: "SMTP 邮件", link: "/zh/channels/smtp" },
      { text: "Telegram", link: "/zh/channels/telegram" },
      { text: "Notion", link: "/zh/channels/notion" },
    ],
  },
  {
    text: "MCP 接入",
    items: [
      { text: "客户端接入与安全说明", link: "/zh/mcp/setup" },
      { text: "工具说明", link: "/zh/mcp/tools" },
    ],
  },
];

const zhArchitectureSidebar: DefaultTheme.SidebarItem[] = [
  {
    text: "架构与开发手册",
    items: [
      { text: "系统架构总览", link: "/zh/reference/architecture" },
      {
        text: "通道协议与投递实现",
        link: "/zh/reference/channel-protocols",
      },
      { text: "安全与防注入机制", link: "/zh/reference/security" },
      { text: "数据库表结构", link: "/zh/reference/database-schema" },
      { text: "REST API 接口", link: "/zh/reference/rest-api" },
      { text: "构建与发布流程", link: "/zh/reference/releasing" },
    ],
  },
];

export default withMermaid(
  defineConfig({
    title: "FeedCourier",
    description:
      "RSS, Atom, and JSON Feed subscription delivery service built with Rust, SQLite, and React. Delivers articles to SMTP Email, Telegram, and Notion, with an embedded web console and MCP server.",
    cleanUrls: true,
    lastUpdated: true,
    sitemap: {
      hostname: SITE_ORIGIN,
      transformItems(items) {
        return items.filter(
          (item) =>
            !item.url.endsWith("/404") &&
            !item.url.endsWith("/404.html") &&
            !/\/v\d+\//.test(item.url),
        );
      },
    },
    head: [
      ["link", { rel: "icon", type: "image/svg+xml", href: "/favicon.svg" }],
      [
        "link",
        {
          rel: "alternate",
          type: "text/plain",
          href: `${SITE_ORIGIN}/llms.txt`,
          title: "LLMs Documentation Index",
        },
      ],
      ["meta", { name: "theme-color", content: "#10b981" }],
      ["meta", { property: "og:image", content: OG_IMAGE_URL }],
      ["meta", { property: "og:image:type", content: "image/png" }],
      ["meta", { property: "og:image:width", content: "1280" }],
      ["meta", { property: "og:image:height", content: "640" }],
      [
        "meta",
        {
          property: "og:image:alt",
          content: "FeedCourier — Self-Hosted RSS, Atom & JSON Feed Delivery Service",
        },
      ],
      ["meta", { property: "og:type", content: "website" }],
      ["meta", { property: "og:site_name", content: "FeedCourier" }],
      ["meta", { name: "twitter:card", content: "summary_large_image" }],
      ["meta", { name: "twitter:image", content: OG_IMAGE_URL }],
    ],
    transformHead({ pageData, title, description }) {
      if (pageData.isNotFound || pageData.relativePath === "404.md") {
        return [["meta", { name: "robots", content: "noindex" }]];
      }

      const rel = pageData.relativePath.replace(/\.md$/, "");
      const isZh = rel === "zh/index" || rel.startsWith("zh/");
      const rawSubPath = isZh ? rel.replace(/^zh\//, "") : rel;
      const isLegacyVersion =
        pageData.frontmatter.noindex === true ||
        pageData.frontmatter.version === "legacy" ||
        /^v\d+(?:\.\d+)*\//.test(rawSubPath);
      const subPath = rawSubPath.replace(/^v\d+(?:\.\d+)*\//, "");
      const isHome = subPath === "index";

      const enPath = isHome ? "/" : `/${subPath}`;
      const zhPath = isHome ? "/zh/" : `/zh/${subPath}`;
      const canonicalUrl = `${SITE_ORIGIN}${isZh ? zhPath : enPath}`;
      const enUrl = `${SITE_ORIGIN}${enPath}`;
      const zhUrl = `${SITE_ORIGIN}${zhPath}`;

      const pageTitle = title || "FeedCourier";
      const pageDesc =
        (typeof pageData.frontmatter.description === "string" &&
          pageData.frontmatter.description) ||
        description;

      const tags: HeadConfig[] = [
        ["link", { rel: "canonical", href: canonicalUrl }],
        ["link", { rel: "alternate", hreflang: "en-US", href: enUrl }],
        ["link", { rel: "alternate", hreflang: "zh-CN", href: zhUrl }],
        ["link", { rel: "alternate", hreflang: "x-default", href: enUrl }],
        ["meta", { property: "og:title", content: pageTitle }],
        ["meta", { property: "og:description", content: pageDesc }],
        ["meta", { property: "og:url", content: canonicalUrl }],
        ["meta", { property: "og:locale", content: isZh ? "zh_CN" : "en_US" }],
        [
          "meta",
          {
            property: "og:locale:alternate",
            content: isZh ? "en_US" : "zh_CN",
          },
        ],
        ["meta", { name: "twitter:title", content: pageTitle }],
        ["meta", { name: "twitter:description", content: pageDesc }],
      ];

      if (isLegacyVersion) {
        tags.push(["meta", { name: "robots", content: "noindex, follow" }]);
      }

      if (isHome) {
        const jsonLd = {
          "@context": "https://schema.org",
          "@type": "SoftwareApplication",
          name: "FeedCourier",
          applicationCategory: "DeveloperApplication",
          operatingSystem: "Linux, macOS, Docker",
          license: "https://www.gnu.org/licenses/agpl-3.0.html",
          url: canonicalUrl,
          inLanguage: isZh ? "zh-CN" : "en-US",
          codeRepository: "https://github.com/ElleryHe/FeedCourier",
          downloadUrl: "https://github.com/ElleryHe/FeedCourier/releases",
          softwareRequirements:
            "Docker or standalone 64-bit Linux/macOS binary",
          offers: {
            "@type": "Offer",
            price: "0",
            priceCurrency: "USD",
          },
          description: pageDesc,
        };
        tags.push([
          "script",
          { type: "application/ld+json" },
          JSON.stringify(jsonLd),
        ]);
      }

      return tags;
    },
    buildEnd(siteConfig: SiteConfig) {
      generateIndexNowKeyFile(siteConfig);
      generateLlmsArtifacts(siteConfig);
    },
    themeConfig: {
      logo: {
        light: "/logo-light.svg",
        dark: "/logo-dark.svg",
        alt: "FeedCourier",
      },
      socialLinks: [
        { icon: "github", link: "https://github.com/ElleryHe/FeedCourier" },
      ],
      search: {
        provider: "local",
        options: {
          locales: {
            zh: {
              translations: {
                button: {
                  buttonText: "搜索文档",
                  buttonAriaLabel: "搜索文档",
                },
                modal: {
                  noResultsText: "无法找到相关结果",
                  resetButtonTitle: "清除查询条件",
                  footer: {
                    selectText: "选择",
                    navigateText: "切换",
                    closeText: "关闭",
                  },
                },
              },
            },
          },
        },
      },
    },
    locales: {
      root: {
        label: "English",
        lang: "en-US",
        description:
          "RSS, Atom, and JSON Feed subscription delivery service built with Rust, SQLite, and React. Delivers articles to SMTP Email, Telegram, and Notion, with an embedded web console and MCP server.",
        themeConfig: {
          nav: [
            {
              text: "Overview",
              link: "/guide/introduction",
            },
            {
              text: "Quick Start",
              link: "/guide/quick-start",
            },
            {
              text: "User Guide",
              link: "/guide/configuration",
              activeMatch: "^/(guide/(configuration|operations)|channels/|mcp/)",
            },
            {
              text: "Architecture",
              link: "/reference/architecture",
              activeMatch: "^/reference/",
            },
          ],
          sidebar: {
            "/guide/": enUserGuideSidebar,
            "/channels/": enUserGuideSidebar,
            "/mcp/": enUserGuideSidebar,
            "/reference/": enArchitectureSidebar,
          },
          editLink: {
            pattern: "https://github.com/ElleryHe/FeedCourier/edit/main/docs/:path",
            text: "Edit this page on GitHub",
          },
          footer: {
            message: "Released under the AGPL-3.0 License.",
            copyright: copyrightHtml,
          },
        },
      },
      zh: {
        label: "简体中文",
        lang: "zh-CN",
        link: "/zh/",
        description:
          "基于 Rust、SQLite 与 React 构建的 RSS / Atom / JSON Feed 订阅投递服务，支持投递至 SMTP 邮件、Telegram 与 Notion，内置 Web 控制台与 MCP 服务端。",
        themeConfig: {
          nav: [
            {
              text: "项目简介",
              link: "/zh/guide/introduction",
            },
            {
              text: "快速开始",
              link: "/zh/guide/quick-start",
            },
            {
              text: "使用手册",
              link: "/zh/guide/configuration",
              activeMatch: "^/zh/(guide/(configuration|operations)|channels/|mcp/)",
            },
            {
              text: "架构手册",
              link: "/zh/reference/architecture",
              activeMatch: "^/zh/reference/",
            },
          ],
          sidebar: {
            "/zh/guide/": zhUserGuideSidebar,
            "/zh/channels/": zhUserGuideSidebar,
            "/zh/mcp/": zhUserGuideSidebar,
            "/zh/reference/": zhArchitectureSidebar,
          },
          editLink: {
            pattern: "https://github.com/ElleryHe/FeedCourier/edit/main/docs/:path",
            text: "在 GitHub 上编辑此页",
          },
          footer: {
            message: "基于 AGPL-3.0 开源协议发布。",
            copyright: copyrightHtml,
          },
          docFooter: {
            prev: "上一页",
            next: "下一页",
          },
          outline: {
            label: "本页目录",
          },
          lastUpdated: {
            text: "最后更新于",
          },
        },
      },
    },
    mermaid: {
      theme: "default",
    },
    vite: {
      build: {
        chunkSizeWarningLimit: 1600,
      },
    },
  }),
);

function generateIndexNowKeyFile(siteConfig: SiteConfig): void {
  const key = process.env.INDEXNOW_KEY?.trim();
  if (!key || !/^[a-zA-Z0-9-]{8,128}$/.test(key)) {
    return;
  }
  writeFileSync(join(siteConfig.outDir, `${key}.txt`), `${key}\n`, "utf-8");
}

function parseMarkdownDoc(
  srcDir: string,
  relLink: string,
): { title: string; description: string; body: string } {
  const filePath = join(srcDir, `${relLink.replace(/^\//, "")}.md`);
  const raw = readFileSync(filePath, "utf-8");

  let description = "";
  let content = raw;
  const fmMatch = raw.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n+/);
  if (fmMatch) {
    content = raw.slice(fmMatch[0].length);
    const descMatch = fmMatch[1].match(/^description:\s*["']?(.+?)["']?\s*$/m);
    if (descMatch) {
      description = descMatch[1].trim();
    }
  }

  // Convert VitePress custom container blocks `::: warning Title` -> `> **Warning: Title**`
  content = content
    .replace(
      /^:::\s*(tip|info|warning|danger|details)(?:\s+(.*))?$/gim,
      (_, kind: string, label?: string) => {
        const cap = kind.charAt(0).toUpperCase() + kind.slice(1).toLowerCase();
        return label?.trim() ? `> **${cap}: ${label.trim()}**` : `> **${cap}**`;
      },
    )
    .replace(/^:::\s*$/gm, "")
    .trim();

  const titleMatch = content.match(/^#\s+(.+?)(?:\s*\{#.*\})?\s*$/m);
  const title = titleMatch ? titleMatch[1].trim() : relLink;

  return { title, description, body: content };
}

function generateLlmsArtifacts(siteConfig: SiteConfig): void {
  const sections = [...enUserGuideSidebar, ...enArchitectureSidebar];
  const llmsIndexLines: string[] = [
    "# FeedCourier",
    "",
    "> Self-hosted RSS, Atom, and JSON Feed subscription delivery service built with Rust, SQLite, and React. Polls feeds, extracts full text via Readability, filters articles, and delivers them to SMTP Email, Telegram (Bot API 10.4 Rich Message / Compact Card / Media Mode), and Notion (2026-03-11 Markdown v2), with an embedded web console and a Streamable HTTP `/mcp` server.",
    "",
    `- Complete single-file English documentation for LLMs: [llms-full.txt](${SITE_ORIGIN}/llms-full.txt)`,
    `- Source repository: [github.com/ElleryHe/FeedCourier](https://github.com/ElleryHe/FeedCourier)`,
    `- License: AGPL-3.0`,
    "",
  ];

  const fullDocChunks: string[] = [
    "# FeedCourier — Complete Documentation",
    "",
    "> Canonical documentation source: https://feedcourier.ellery.me",
    "> License: AGPL-3.0 | Repository: https://github.com/ElleryHe/FeedCourier",
    "",
  ];

  for (const group of sections) {
    if (!group.text || !group.items?.length) continue;
    llmsIndexLines.push(`## ${group.text}`, "");

    for (const item of group.items) {
      if (!item.link) continue;
      const parsed = parseMarkdownDoc(siteConfig.srcDir, item.link);
      const url = `${SITE_ORIGIN}${item.link}`;
      const label = item.text || parsed.title;
      const summary = parsed.description ? `: ${parsed.description}` : "";
      llmsIndexLines.push(`- [${label}](${url})${summary}`);

      fullDocChunks.push(
        "---",
        "",
        `Source: ${url}`,
        ...(parsed.description ? [`Description: ${parsed.description}`] : []),
        "",
        parsed.body,
        "",
      );
    }
    llmsIndexLines.push("");
  }

  writeFileSync(
    join(siteConfig.outDir, "llms.txt"),
    `${llmsIndexLines.join("\n").trimEnd()}\n`,
    "utf-8",
  );
  writeFileSync(
    join(siteConfig.outDir, "llms-full.txt"),
    `${fullDocChunks.join("\n").trimEnd()}\n`,
    "utf-8",
  );
}
