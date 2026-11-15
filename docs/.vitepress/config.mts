import { defineConfig, type DefaultTheme } from "vitepress";
import { withMermaid } from "vitepress-plugin-mermaid";

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
      hostname: "https://feedcourier.ellery.me",
    },
    head: [
      ["link", { rel: "icon", type: "image/svg+xml", href: "/favicon.svg" }],
      ["meta", { name: "theme-color", content: "#10b981" }],
      [
        "meta",
        {
          property: "og:image",
          content: "https://feedcourier.ellery.me/logo.png",
        },
      ],
      ["meta", { property: "og:type", content: "website" }],
      ["meta", { property: "og:site_name", content: "FeedCourier" }],
    ],
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
  })
);
