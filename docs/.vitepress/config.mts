import { defineConfig } from "vitepress";
import { withMermaid } from "vitepress-plugin-mermaid";

export default withMermaid(
  defineConfig({
    title: "FeedCourier",
    description:
      "Lightweight pure-Rust RSS / Atom / JSON Feed streaming delivery gateway for SMTP, Telegram Bot API 10.4, Notion 2026-03-11, and MCP.",
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
          "Lightweight pure-Rust RSS / Atom / JSON Feed streaming delivery gateway for SMTP, Telegram Bot API 10.4, Notion 2026-03-11, and MCP.",
        themeConfig: {
          nav: [
            { text: "Guide", link: "/guide/introduction", activeMatch: "/guide/" },
            {
              text: "Channels",
              link: "/channels/pacing-and-quotas",
              activeMatch: "/channels/",
            },
            { text: "MCP", link: "/mcp/setup", activeMatch: "/mcp/" },
            {
              text: "Reference",
              link: "/reference/architecture",
              activeMatch: "/reference/",
            },
          ],
          sidebar: [
            {
              text: "Getting Started & Operations",
              items: [
                { text: "Introduction", link: "/guide/introduction" },
                { text: "Quick Start", link: "/guide/quick-start" },
                { text: "Configuration & Proxy", link: "/guide/configuration" },
                { text: "Operations & Backups", link: "/guide/operations" },
              ],
            },
            {
              text: "Delivery Channels",
              items: [
                {
                  text: "Pacing, Quotas & Filters",
                  link: "/channels/pacing-and-quotas",
                },
                { text: "SMTP Newsletters", link: "/channels/smtp" },
                { text: "Telegram (Bot API 10.4)", link: "/channels/telegram" },
                {
                  text: "Notion (2026-03-11 & v2)",
                  link: "/channels/notion",
                },
              ],
            },
            {
              text: "Native MCP Server",
              items: [
                { text: "Client Setup", link: "/mcp/setup" },
                { text: "Tools Reference", link: "/mcp/tools" },
                { text: "Security Boundaries", link: "/mcp/security" },
              ],
            },
            {
              text: "Architecture & Reference",
              items: [
                { text: "Core Architecture", link: "/reference/architecture" },
                {
                  text: "SQLite Database Schema",
                  link: "/reference/database-schema",
                },
                { text: "REST API (/api/v1/*)", link: "/reference/rest-api" },
                {
                  text: "Release & CI/CD Pipeline",
                  link: "/reference/releasing",
                },
              ],
            },
          ],
          editLink: {
            pattern: "https://github.com/ElleryHe/FeedCourier/edit/main/docs/:path",
            text: "Edit this page on GitHub",
          },
          footer: {
            message: "Released under the AGPL-3.0-only License.",
            copyright: "Copyright © 2026 ElleryHe",
          },
        },
      },
      zh: {
        label: "简体中文",
        lang: "zh-CN",
        link: "/zh/",
        description:
          "轻量级纯 Rust RSS / Atom / JSON Feed 流式订阅投递网关，原生支持 SMTP、Telegram Bot API 10.4、Notion 2026-03-11 与 MCP。",
        themeConfig: {
          nav: [
            { text: "指南", link: "/zh/guide/introduction", activeMatch: "/zh/guide/" },
            {
              text: "推送通道",
              link: "/zh/channels/pacing-and-quotas",
              activeMatch: "/zh/channels/",
            },
            { text: "MCP 接入", link: "/zh/mcp/setup", activeMatch: "/zh/mcp/" },
            {
              text: "参考手册",
              link: "/zh/reference/architecture",
              activeMatch: "/zh/reference/",
            },
          ],
          sidebar: [
            {
              text: "快速开始与运维",
              items: [
                { text: "项目定位与设计哲学", link: "/zh/guide/introduction" },
                { text: "快速开始", link: "/zh/guide/quick-start" },
                { text: "环境变量与代理分流", link: "/zh/guide/configuration" },
                { text: "持久化运维与热备份", link: "/zh/guide/operations" },
              ],
            },
            {
              text: "三大推送通道配置",
              items: [
                {
                  text: "步进流控、配额与过滤",
                  link: "/zh/channels/pacing-and-quotas",
                },
                { text: "SMTP 邮件信纸通道", link: "/zh/channels/smtp" },
                {
                  text: "Telegram 通道 (Bot API 10.4)",
                  link: "/zh/channels/telegram",
                },
                {
                  text: "Notion 通道 (2026-03-11)",
                  link: "/zh/channels/notion",
                },
              ],
            },
            {
              text: "原生 MCP 智能接入",
              items: [
                { text: "客户端接入配置", link: "/zh/mcp/setup" },
                { text: "7 大工具详解", link: "/zh/mcp/tools" },
                { text: "纵深安全护栏", link: "/zh/mcp/security" },
              ],
            },
            {
              text: "架构原理与参考手册",
              items: [
                { text: "核心架构深潜", link: "/zh/reference/architecture" },
                {
                  text: "SQLite 表结构参考",
                  link: "/zh/reference/database-schema",
                },
                { text: "REST API 接口速查", link: "/zh/reference/rest-api" },
                {
                  text: "自动化发布流水线",
                  link: "/zh/reference/releasing",
                },
              ],
            },
          ],
          editLink: {
            pattern: "https://github.com/ElleryHe/FeedCourier/edit/main/docs/:path",
            text: "在 GitHub 上编辑此页",
          },
          footer: {
            message: "基于 AGPL-3.0-only 开源协议发布。",
            copyright: "Copyright © 2026 ElleryHe",
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
