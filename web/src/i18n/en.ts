export const en = {
  common: {
    refresh: "Refresh",
    retry: "Retry",
    cancel: "Cancel",
    close: "Close",
    save: "Save",
    edit: "Edit",
    delete: "Delete",
    confirm: "Confirm",
    add: "Add",
    clear: "Clear",
    all: "All",
    rows: "Rows:",
    disabled: "Disabled",
    proxy: "Proxy",
    readability: "Readability",
    unlimited: "Unlimited",
    never: "Never",
    required: "Required",
    suggestions: "Suggestions:",
    typeAndPressEnter: "Type and press Enter…",
    removeItem: (item: string) => `Remove ${item}`,
    closeDialog: "Close dialog",
    dismissNotification: "Dismiss notification",
    unexpectedError: "Unexpected error occurred",
    invalidRegex: "Invalid regular expression",
    requestFailedHttp: (status: number) => `Request failed with HTTP ${status}`,
  },

  locale: {
    consoleTitle: "FeedCourier Console",
    switchLabel: "中文",
    switchTitle: "Switch interface language to Simplified Chinese (切换为简体中文)",
  },

  theme: {
    system: "Auto",
    light: "Light",
    dark: "Dark",
    toggleTitle: "Cycle color theme (Auto / Light / Dark)",
  },

  footer: {
    sourceRepository: "Source Repository",
  },

  shell: {
    subtitle: "Delivery Service",
    openNav: "Open navigation menu",
    closeNav: "Close navigation menu",
    mainNav: "Main navigation",
    signOut: "Sign out",
    signedOut: "Signed out",
    signOutFailed: "Sign out failed",
    connecting: "Connecting to FeedCourier…",
    backendUnreachableTitle: "Backend API Unreachable",
    backendUnreachablePrefix: "Could not reach",
    retryConnection: "Retry Connection",
    nav: {
      overview: {
        label: "Overview",
        subtitle: "Status & Maintenance",
      },
      feeds: {
        label: "Feeds",
        subtitle: "RSS · Atom · JSON Feed",
      },
      channels: {
        label: "Channels",
        subtitle: "SMTP · Telegram · Notion",
      },
      routes: {
        label: "Routes",
        subtitle: "Filters & Delivery Modes",
      },
      plugins: {
        label: "Plugins",
        subtitle: "Extensions & Marketplace",
      },
      tasks: {
        label: "Outbox",
        subtitle: "Queue & Dead-Letter",
      },
      settings: {
        label: "System & MCP",
        subtitle: "Proxy · SSRF · MCP Tokens",
      },
    },
  },

  status: {
    feed: {
      active: "Active",
      backoff: "Backoff",
      circuit_open: "Circuit Open",
      paused: "Paused",
    },
    task: {
      queued: "Queued",
      delivering: "Delivering",
      deliveringLeased: "Delivering (Leased)",
      retrying: "Retrying",
      deferred: "Deferred",
      quotaDeferred: "Quota Deferred",
      dead_letter: "Dead Letter",
      delivered: "Delivered",
      deliveredWarning: "Delivered (Partial)",
    },
  },

  auth: {
    heading: "FeedCourier Console",
    subheading: "Self-hosted RSS, Atom & JSON Feed delivery service · MCP Server",
    initTitle: "Initialize Administrator Password",
    loginTitle: "Administrator Sign In",
    initDesc:
      "First-time setup: your password is hashed with Argon2id, and channel credentials are encrypted at rest with ChaCha20-Poly1305.",
    loginDesc:
      "Enter your administrator username and password to manage feeds, delivery channels, routing rules, and MCP tokens.",
    newPasswordLabel: "New Admin Password",
    passwordLabel: "Password",
    minCharsPlaceholder: "Minimum 8 characters…",
    envSeedHintPrefix: "Tip: You can also set",
    envSeedHintSuffix: "via environment variable on container startup.",
    confirmPasswordLabel: "Confirm Admin Password",
    confirmPasswordPlaceholder: "Re-enter password…",
    initSubmit: "Initialize & Sign In",
    loginSubmit: "Sign In",
    toastInitDone: "Administrator password initialized",
    toastSignedIn: "Signed in to FeedCourier",
    errMinLength: "Administrator password must be at least 8 characters.",
    errMismatch: "Password confirmation does not match.",
    errEmpty: "Please enter your administrator password.",
    usernameLabel: "Username",
    usernameHint:
      "Defaults to Courier. You can change it later under System → Account & Security.",
    errUsername: "Username must be 1–64 characters.",
    errEmptyUsername: "Please enter your administrator username.",
    mfaTitle: "Two-Factor Verification",
    mfaDesc:
      "Enter the 6-digit code from your authenticator app to finish signing in.",
    mfaRecoveryDesc:
      "Enter one of your one-time recovery codes. Each code can only be used once.",
    mfaCodeLabel: "Authentication Code",
    mfaCodeHint: "Submits automatically once all 6 digits are entered.",
    mfaRecoveryLabel: "Recovery Code",
    mfaRecoveryHint: "Format XXXX-XXXX-XXXX (case-insensitive, dashes optional).",
    mfaVerify: "Verify & Sign In",
    mfaUseRecovery: "Use a recovery code instead",
    mfaUseTotp: "Use authenticator code",
    mfaBack: "Back to password",
    mfaExpired: "The verification session expired. Please sign in again.",
  },

  overview: {
    title: "System Overview",
    subtitle:
      "Polling status, outbox task queue, channel rate-limit cooldowns, and SQLite database maintenance.",
    loadError: "Unable to load overview metrics:",
    subscriptionHealth: "Subscription Health",
    manageFeeds: "Manage feeds →",
    outboxHeading: "Outbox Task Queue (1 Task = 1 Entry × 1 Route)",
    inspectOutbox: "Inspect outbox →",
    fingerprintsTitle: "Fingerprints & Cached Payloads",
    fingerprintsDesc:
      "SHA-256 fingerprints are retained permanently to prevent duplicate sends; cached HTML and media metadata are cleaned up after the retention window.",
    permanentFingerprints: "Permanent Fingerprints",
    permanentFingerprintsSub: "SHA-256 deduplication index",
    activePayloads: "Active Cached Payloads",
    activePayloadsSub: "In-flight or within retention window",
    cooldownsTitle: "Active Channel Cooldowns",
    channelsLink: "Channels →",
    cooldownsDesc:
      "Downstream rate-limit pauses (SMTP 421/45x, Telegram retry_after, Notion 429) shared across all destinations of the same account.",
    noCooldowns:
      "All delivery channels are sending normally — no active cooldowns.",
    resumes: (rel: string) => `Resumes ${rel}`,
    maintenanceTitle: "SQLite WAL Maintenance",
    maintenanceDescPrefix: "Clean up expired article bodies immediately or download an online",
    maintenanceDescSuffix: "database snapshot.",
    runGcNow: "Clean Up Expired Payloads Now",
    downloadSnapshot: "Download Online SQLite Snapshot (.db)",
    snapshotNote:
      "Snapshots include feeds, encrypted channels, routes, tasks, and deduplication fingerprints.",
    gcDoneTitle: "Payload cleanup completed",
    gcDoneDesc: (count: number) =>
      `Cleaned cached bodies from ${count} finished ${count === 1 ? "entry" : "entries"}; SHA-256 fingerprints retained.`,
    gcFailedTitle: "Payload cleanup failed",
    backupDoneTitle: "SQLite snapshot downloaded",
    backupDoneDesc: (filename: string) => `Saved ${filename}`,
    backupFailedTitle: "Snapshot backup failed",
    queuePausedTitle: "Delivery queue is paused",
    queuePausedUntil: (at: string, rel: string) =>
      `Feeds are still polled and new tasks keep queuing, but nothing is sent until ${at} (${rel}) or until you resume.`,
    queuePausedIndefinitely:
      "Feeds are still polled and new tasks keep queuing, but nothing is sent until you resume the queue.",
    queueManage: "Queue controls",
  },

  feeds: {
    title: "Subscriptions & Full-Text Extraction",
    subtitle:
      "RSS, Atom, and JSON Feed polling with SSRF protection, conditional ETag/Last-Modified requests, optional Readability full-text extraction, and deduplication.",
    livePreview: "Live Preview",
    importOpml: "Import OPML",
    exportOpml: "Export OPML",
    addFeed: "Add Feed",
    searchPlaceholder: "Search by title or URL…",
    sortLabel: "Sort feeds",
    sortOptions: {
      created_desc: "Added: Newest",
      created_asc: "Added: Oldest",
      name_asc: "Name: A → Z",
      name_desc: "Name: Z → A",
    },
    tagFilterLabel: "Filter by tag",
    allTagsOption: "All tags",
    tagOption: (tag: string, count: number) => `#${tag} (${count})`,
    untaggedOption: (count: number) => `Untagged (${count})`,
    pagination: {
      showing: (from: number, to: number, total: number) =>
        `Showing ${from}–${to} of ${total}`,
      perPage: "Per page:",
      prev: "Previous",
      next: "Next",
      pageOf: (page: number, total: number) => `Page ${page} / ${total}`,
    },
    loading: "Loading subscriptions…",
    emptyTitle: "No feeds configured yet",
    emptyFilteredTitle: "No feeds match your current filter",
    emptyDesc:
      "Add an RSS, Atom, or JSON Feed URL, or import an existing OPML file with silent initialization.",
    emptyFilteredDesc:
      "Try clearing your search query, changing the tag filter, or switching status tabs.",
    addFirstFeed: "Add First Feed",
    failsBadge: (count: number) => `${count} ${count === 1 ? "fail" : "fails"}`,
    intervalLabel: "Interval:",
    nextPollLabel: "Next poll:",
    pausedLower: "paused",
    resetAndPoll: "Reset & Poll",
    pollNow: "Poll Now",
    pollButtonTitle:
      "Reset circuit breaker / backoff and poll this feed immediately",
    fullTextOn: "Full Text: ON",
    fullTextOff: "Full Text: OFF",
    fullTextToggleTitle: "Toggle Readability full-text extraction",
    previewButtonTitle: "Preview feed & test filter rules",
    resumePollingTitle: "Resume feed polling",
    pausePollingTitle: "Pause feed polling",
    editFeedTitle: "Edit feed settings",
    deleteFeedTitle: "Delete feed",
    toastNotModifiedTitle: "304 Not Modified",
    toastNotModifiedDesc: "Upstream returned 304 Not Modified; schedule updated.",
    toastPolledTitle: "Feed polled",
    toastPolledDesc: (entries: number, tasks: number) =>
      `Discovered ${entries} new ${entries === 1 ? "entry" : "entries"} (${tasks} ${tasks === 1 ? "task" : "tasks"} enqueued).`,
    toastPollFailed: "Immediate poll failed",
    toastUpdateFailed: "Failed to update feed",
    toastRemoved: "Feed removed",
    toastDeleteFailed: "Failed to delete feed",
    toastOpmlExported: "OPML exported",
    toastOpmlExportedDesc: (filename: string) => `Downloaded ${filename}`,
    toastOpmlExportFailed: "OPML export failed",
    toastUpdated: "Feed updated",
    toastAdded: "Feed added",
    deleteModalTitle: "Delete Subscription",
    deleteModalDesc: (name: string) =>
      `Remove "${name}"? Stored entry fingerprints and delivery tasks for this feed will also be deleted.`,
    deleteModalConfirm: "Delete Feed",
    clearCache: {
      buttonTitle: "Clear cache (entries, fingerprints, HTTP cache)",
      title: "Clear Feed Cache",
      desc: (name: string) =>
        `Delete all stored entries, deduplication fingerprints, and queued delivery tasks of "${name}" and reset its ETag / Last-Modified and failure state? Already delivered messages are not recalled.`,
      pollNowLabel: "Re-fetch and queue the full backlog now",
      pollNowDesc:
        "Poll immediately after clearing; every item currently in the feed is enqueued oldest-first (silent initialization is turned off).",
      confirm: "Clear Cache",
      toastDoneTitle: "Feed cache cleared",
      toastDoneDesc: (deleted: number) =>
        `Deleted ${deleted} cached ${deleted === 1 ? "entry" : "entries"}; the backlog will be queued on the next poll.`,
      toastDoneDescPolled: (deleted: number, entries: number, tasks: number) =>
        `Deleted ${deleted} cached ${deleted === 1 ? "entry" : "entries"}; re-fetched ${entries} and enqueued ${tasks} ${tasks === 1 ? "task" : "tasks"}.`,
      toastFailed: "Failed to clear feed cache",
    },
    editor: {
      editTitle: "Edit Subscription",
      addTitle: "Add Subscription",
      desc: "Configure polling interval, tags (used for route matching and Notion Tag sync), and full-text extraction.",
      previewUrl: "Preview URL",
      saveChanges: "Save Changes",
      subscribe: "Subscribe",
      urlLabel: "Feed URL (RSS / Atom / JSON Feed)",
      titleLabel: "Custom Title Override (Optional)",
      titlePlaceholder: "Auto-detected from feed if left blank",
      intervalLabel: "Poll Interval (Seconds)",
      tagsLabel: "Tags (Route matching & Notion Tag sync)",
      tagsPlaceholder: "e.g. rust, security, ai…",
      readabilityLabel: "Readability Full-Text Extraction",
      readabilityDesc:
        "Fetch the original article webpage for summary-only feeds; automatically falls back to the feed summary if blocked or unreachable.",
      proxyLabel: "Use Outbound Proxy",
      proxyDesc:
        "Fetch this feed and its article webpages via the configured HTTP/SOCKS5 outbound proxy.",
      silentInitLabel: "Silent Initialization (Prevent Backlog Flood)",
      silentInitDesc:
        "Record SHA-256 fingerprints for existing articles on first poll without sending historical items.",
      pushLatestLabel: "Send Latest 1 Entry on First Sync",
      pushLatestDesc:
        "Enqueue only the single newest item during silent initialization to verify delivery.",
      errInvalidUrl: "Please enter a valid http:// or https:// feed URL.",
      errMinInterval: "Poll interval must be at least 60 seconds.",
    },
    preview: {
      title: "Live Feed & Filter Preview",
      desc: "Test-fetch a feed URL, inspect Readability full-text extraction, and dry-run keyword or regex filters before subscribing.",
      subscribeWithSettings: "Subscribe with Settings",
      fetchAndPreview: "Fetch & Preview",
      readabilitySwitch: "Readability Full-Text",
      proxySwitch: "Use Outbound Proxy",
      hideFilters: "Hide Filter Dry-Run",
      showFilters: "Test Filter Rules",
      includeKeywordsLabel: "Include Keywords (Match Any)",
      includeKeywordsPlaceholder: "Add keyword…",
      excludeKeywordsLabel: "Exclude Keywords (Block Any)",
      excludeKeywordsPlaceholder: "Sponsor, Ad…",
      includeRegexLabel: "Include Regex",
      excludeRegexLabel: "Exclude Regex",
      errInvalidUrl: "Enter a valid http:// or https:// feed URL.",
      errIncludeRegex: (msg: string) => `Include regex invalid: ${msg}`,
      errExcludeRegex: (msg: string) => `Exclude regex invalid: ${msg}`,
      untitledFeed: "Untitled Feed",
      itemsCount: (count: number) => `${count} ${count === 1 ? "item" : "items"}`,
      matchedSummary: "Matched:",
      matchedBadge: "Matched",
      filteredBadge: "Filtered",
      mediaCount: (count: number) => `${count} media`,
      filterNote: (reason: string) => `Filter note: ${reason}`,
      firstSentenceTag: "[1st sentence] ",
      collapse: "Collapse",
      body: "Body",
      debugTitle: "Debug Push",
      debugHint:
        "Sends straight to the destination without touching fingerprints, quotas, or the queue.",
      debugDestination: "Destination",
      debugLanguage: "Delivery language",
      debugTimezone: "Timezone (Optional)",
      debugTimezonePlaceholder: (sysTz: string) => `Inherit system (${sysTz})`,
      debugTelegramMode: "Telegram mode",
      debugNoDestination:
        "Create a delivery account with at least one destination first to use debug push.",
      debugInvalidTimezone:
        "Select a valid IANA timezone or leave blank to inherit the system timezone.",
      pushLatest: "Push Latest",
      pushThisItem: "Push This Item",
      debugPushedTitle: "Debug push delivered",
      debugFailedTitle: "Debug push failed",
      debugPluginsLabel: "Plugins (Optional)",
      debugPluginNoProviders: "No providers configured",
    },
    opml: {
      title: "Import OPML Subscriptions",
      desc: "Upload an .opml / .xml file or paste OPML 2.0 XML directly. Nested OPML folders are mapped to feed tags.",
      importBtn: "Import Feeds",
      fileLabel: "Select OPML File",
      xmlLabel: "OPML XML Content",
      dedupHint:
        "Existing feeds with identical URLs are automatically skipped.",
      silentInitLabel: "Silent Initialization (Recommended)",
      silentInitDesc:
        "Record SHA-256 fingerprints for existing articles on initial sync so imported feeds do not flood your channels.",
      errEmptyXml: "Please upload or paste OPML XML content.",
      toastDoneTitle: "OPML import complete",
      toastDoneDesc: (imported: number, skipped: number) =>
        `Imported ${imported} ${imported === 1 ? "feed" : "feeds"} (${skipped} existing skipped).`,
    },
  },

  channels: {
    title: "Delivery Channels & Destinations",
    subtitle:
      "Configure accounts (credentials, proxy, shared rate limits) and attach one or more destinations per account. Secrets and destination targets are encrypted with ChaCha20-Poly1305 and never exposed to MCP clients.",
    newChannel: "New Account",
    loading: "Loading delivery accounts…",
    loadError: "Unable to load delivery accounts:",
    emptyTitle: "No delivery accounts configured",
    emptyDesc:
      "Configure an SMTP email account, a Telegram bot, or a Notion integration — and attach one or more destinations — before creating delivery routes.",
    emptyFilteredTitle: "No accounts match this channel type",
    emptyFilteredDesc:
      "Switch the channel type filter above or create a new delivery account.",
    configureFirst: "Configure First Account",
    kindLabels: {
      smtp: "SMTP Email",
      telegram: "Telegram Bot",
      notion: "Notion",
    },
    pacingLine: (
      minDur: string,
      jitterDur: string,
      hourly: number | null,
      daily: number | null,
    ) =>
      `Shared rate: min ${minDur} + up to ${jitterDur} jitter${hourly ? ` · ${hourly}/hr` : ""}${daily ? ` · ${daily}/day` : ""}`,
    destinationsHeading: (count: number) => `Destinations (${count})`,
    addDestination: "Add Destination",
    noDestinationsHint:
      "No destinations attached yet. Add a destination (chat, recipient list, or Notion database) so routes can deliver through this account.",
    publicUrlMissingBanner:
      "Automatic RFC 8058 one-click unsubscribe and verification links require PUBLIC_URL in System Settings. Until configured, emails fall back to mailto: unsubscribe and manual 6-digit verification codes.",
    card: {
      server: "Server:",
      fromAddress: "From:",
      senderIdentity: "Identity:",
      auth: "Auth:",
      anonymous: "anonymous",
      noPassword: "(no password)",
      botApi: "Bot API:",
      botToken: "Bot Token:",
      secret: "Secret:",
      destTo: "To:",
      destReplyTo: "Reply-To:",
      destChat: "Chat:",
      topicSuffix: (id: number) => ` (topic #${id})`,
      destDataSource: "Data Source:",
      requiredKeys: "Keys:",
      richReady: "Rich Message Supported",
      richUnsupported: "Rich Message Unsupported (Compact Fallback)",
      fallbackPrefix: (reason: string) => `Fallback: ${reason}`,
      cooldownPrefix: (rel: string) => `Cooldown ${rel}`,
      testAccount: "Verify Account",
      editAccount: "Edit Account",
      deleteTitle: "Delete account",
      testDestination: "Send Test",
      editDestinationTitle: "Edit destination",
      deleteDestinationTitle: "Delete destination",
    },
    toastAccountTestOk: "Account credentials verified",
    toastAccountTestIssue: "Account verification reported an issue",
    toastAccountTestFailed: "Account verification failed",
    toastDestTestOk: "Destination test delivered",
    toastDestTestIssue: "Destination test reported an issue",
    toastDestTestFailed: "Destination test failed",
    toastToggleFailed: "Failed to toggle account",
    toastDestToggleFailed: "Failed to toggle destination",
    toastDeleted: "Account deleted",
    toastDeleteFailed: "Failed to delete account",
    toastUpdated: "Account updated",
    toastCreated: "Account created",
    toastDestCreated: "Destination added",
    toastDestUpdated: "Destination updated",
    toastDestDeleted: "Destination deleted",
    toastDestDeleteFailed: "Failed to delete destination",
    deleteModalTitle: "Delete Delivery Account",
    deleteModalDesc: (name: string) =>
      `Delete delivery account "${name}" and all destinations under it?`,
    deleteModalConfirm: "Delete Account",
    deleteDestModalTitle: "Delete Destination",
    deleteDestModalDesc: (name: string) => `Delete destination "${name}"?`,
    deleteDestModalConfirm: "Delete Destination",
    deleteAccountConflict: {
      title: "Account Referenced by Active Routes",
      desc: (accountName: string, count: number) =>
        `Account "${accountName}" is currently used by ${count} ${count === 1 ? "route" : "routes"}. Deleting it with cascade enabled will also permanently delete its destinations, the routes below, and their pending delivery tasks.`,
      affectedRoutesLabel: "Affected Routes",
      confirmCascade: (count: number) =>
        `Delete Account & ${count} ${count === 1 ? "Route" : "Routes"}`,
    },
    deleteConflict: {
      title: "Destination Referenced by Active Routes",
      desc: (destName: string, count: number) =>
        `Destination "${destName}" is currently used by ${count} ${count === 1 ? "route" : "routes"}. Deleting it with cascade enabled will also permanently delete the routes below and their pending delivery tasks.`,
      affectedRoutesLabel: "Affected Routes",
      confirmCascade: (count: number) =>
        `Delete Destination & ${count} ${count === 1 ? "Route" : "Routes"}`,
    },
    smtpCompliance: {
      verifiedCountBadge: (verified: number, total: number) =>
        `${verified}/${total} verified`,
      autoPausedBadge: "Auto-paused",
      autoPausedHint:
        "All recipients unsubscribed or bounced. Delivery is paused and will automatically resume once any recipient re-verifies.",
      recipientsToggle: "Recipients & Compliance",
      loading: "Loading recipient compliance status…",
      loadError: "Unable to load recipients:",
      empty: "No recipients configured for this destination.",
      status: {
        unverified: "Unverified",
        pending: "Pending",
        verified: "Verified",
        unsubscribed: "Unsubscribed",
        bounced: "Bounced",
      },
      consentMethod: {
        legacy: "Legacy backfill",
        code: "Verified via code",
        link: "Verified via link",
        other: (method: string) => `Verified (${method})`,
      },
      revokedMethod: {
        one_click: "One-click unsubscribe",
        hard_bounce: "Hard bounce (5xx)",
        admin: "Admin unsubscribed",
        other: (method: string) => `Revoked (${method})`,
      },
      auditWithTime: (label: string, rel: string) => `${label} · ${rel}`,
      codeExpires: (rel: string) => `Code expires ${rel}`,
      bounceTooltipPrefix: "Bounce diagnostic: ",
      sendVerification: "Send Verification",
      resendVerification: "Resend Code",
      sendCooldown: (remaining: string) => `Send (${remaining})`,
      resendCooldown: (remaining: string) => `Resend (${remaining})`,
      codePlaceholder: "6-digit code",
      verifyBtn: "Verify",
      unsubscribeBtn: "Unsubscribe",
      reactivateBtn: "Reactivate",
      errCodeInvalid: "Enter the 6-digit verification code.",
      toastVerificationSentTitle: "Verification email sent",
      toastCooldownTitle: "Please wait before resending",
      toastCooldownDesc: (remaining: string) =>
        `Verification email cooldown active. Try again in ${remaining}.`,
      toastRateLimitedDesc:
        "Too many verification emails requested. Please wait before trying again.",
      toastVerifiedTitle: "Recipient verified",
      toastVerifiedDesc: (email: string) =>
        `${email} is now verified and eligible for delivery.`,
      toastUnsubscribedTitle: "Recipient unsubscribed",
      toastUnsubscribedDesc: (email: string) =>
        `${email} will no longer receive emails from this destination.`,
      toastReactivatedTitle: "Recipient reactivated",
      toastReactivatedDesc: (email: string) =>
        `${email} was reset to unverified. Send a fresh verification email to resume delivery.`,
      toastActionFailed: "Recipient action failed",
    },
    wizard: {
      editTitle: (name: string) => `Edit Account: ${name}`,
      newTitle: "New Channel Account",
      desc: "Credentials are encrypted with ChaCha20-Poly1305. Pacing, quotas, proxy, and cooldowns are shared across all destinations under this account.",
      saveChannel: "Save Account",
      createChannel: "Create Account",
      nameLabel: "Account Name",
      nameOptionalLabel: "Account Name (Optional)",
      namePlaceholderSmtp: "Leave blank to default to From address",
      namePlaceholderTg: "Leave blank to auto-detect @bot_username",
      namePlaceholderNotion: "Leave blank to auto-detect integration name",
      initialDestHeading: "Initial Destination",
      initialDestSwitchLabel: "Also Create First Destination",
      initialDestSwitchDesc:
        "Configure the first target chat, email recipient group, or Notion database alongside this account.",
      smtp: {
        heading: "SMTP Account Credentials",
        hostLabel: "SMTP Host",
        portLabel: "Port",
        securityLabel: "Connection Security",
        secStartTls: "STARTTLS (587)",
        secTls: "Implicit TLS / SMTPS (465)",
        secPlain: "Plain Local Relay (25)",
        usernameLabel: "SMTP Username",
        passwordLabel: "SMTP Password / App Password",
        passwordStoredPlaceholder: "Stored (••••••••) — leave blank to keep",
        clearPasswordLabel: "Clear stored password",
        fromLabel: "From Address (SPF/DKIM Verified)",
        fromHint:
          "Display name adopts each feed's title, with deterministic Message-ID and List-ID headers.",
        senderIdentityLabel: "Sender Identity / Contact Line (Optional)",
        senderIdentityPlaceholder: "Acme News · 123 Main St",
        senderIdentityHint:
          "Optional sender identity or physical contact line appended to email footers (CAN-SPAM compliance).",
        destHeading: "SMTP Recipients",
        replyToLabel: "Reply-To Address (Optional)",
        replyToPlaceholder: "Optional Reply-To address",
        toLabel: "Recipient Addresses (Press Enter to add)",
        newRecipientsHint:
          "New recipients start as Unverified and will not receive verification emails automatically. Trigger verification manually from Recipients & Compliance after saving.",
      },
      telegram: {
        heading: "Telegram Bot Credentials",
        tokenLabel: "Bot Token",
        tokenStoredPlaceholder: (hint: string) =>
          `Stored (${hint}) — leave blank to keep`,
        apiBaseLabel: "Custom Bot API Base URL (Optional)",
        apiBasePlaceholder:
          "https://api.telegram.org (supports reverse proxy or Local Bot API Server)",
        apiBaseHint:
          "Checks Rich Message support on save and falls back to Compact Card automatically if unsupported by a custom endpoint.",
        destHeading: "Telegram Target Chat",
        chatIdLabel: "Target Chat ID or @Channel",
        threadIdLabel: "Forum Topic Thread ID (Optional)",
        threadIdPlaceholder: "e.g. 42",
      },
      notion: {
        accountHeading: "Notion Integration Credentials",
        heading: "Notion Target Database",
        modeInit: "1. Auto-Create 8-Column DB",
        modeBind: "2. Bind Existing Database",
        secretLabel: "Notion Internal Integration Secret",
        secretStoredPlaceholder: (hint: string) =>
          `Stored (${hint}) — leave blank to keep`,
        usingStoredSecretHint:
          "Reuses the Notion integration secret and proxy setting stored on this account.",
        initIntro:
          "Creates a new Notion database under your authorized parent page with the 8 standard columns (Title, Link, Short URL, Source, Published, State, Tag, FC-ID) and binds its data source automatically.",
        parentPageLabel: "Authorized Parent Page ID or URL",
        dbTitleLabel: "New Database Title",
        autoCreateBtn: "Create 8-Column Database Now",
        autoCreatedBanner: (dbId: string, dsId: string) =>
          `Created 8-column database (${dbId}) → bound data_source_id ${dsId}.`,
        toastCreatedTitle: "Notion database created",
        toastCreatedDesc: (dsId: string) => `Bound data_source_id: ${dsId}`,
        existingTargetLabel: "Existing Data Source ID or Database URL",
        inspectBtn: "Inspect Schema",
        missingBanner: (count: number) =>
          `${count} standard ${count === 1 ? "column is" : "columns are"} missing on this database (including FC-ID if unmapped).`,
        patchMissingBtn: "Add Missing Columns",
        toastPatchedTitle: "Updated Notion database schema",
        toastPatchedDesc: (cols: string) => `Added columns: ${cols}`,
        toastInspectedTitle: "Schema inspected",
        toastInspectedReady: "Required columns (Title & FC-ID) verified.",
        toastInspectedReview: "Review missing or mismatched columns below.",
        colField: "Field",
        colExpectedType: "Expected Type",
        colStatus: "Status",
        colMappedProp: "Mapped Notion Property",
        foundType: (actual: string) => `(found: ${actual})`,
        statusMatched: "Matched",
        statusMismatch: "Type Mismatch",
        statusMissing: "Missing",
        statusUnverified: "Unverified",
        selectRequiredOption: (expectedType: string) =>
          `— Select ${expectedType} column —`,
        skipFieldOption: "— Skip field —",
        optionalBlankPlaceholder: "Optional (blank to skip)",
        purposes: {
          title: "Article title",
          fc_id: "SHA-256 fingerprint for deduplication",
          link: "Original article URL",
          short_url: "Shortened article URL (Link Rewrite plugin)",
          source: "Feed source title",
          published: "Publication timestamp",
          state: "UnRead / Read reading state",
          tag: "Synced subscription tags",
        },
      },
      pacing: {
        heading: "Shared Send Rate, Random Jitter & Quotas",
        sharedNote:
          "Applied at the account level and shared across all destinations under this account.",
        minIntervalLabel: "Min Interval (Secs)",
        maxJitterLabel: "Max Jitter (Secs)",
        hourlyQuotaLabel: "Hourly Quota (Opt)",
        dailyQuotaLabel: "Daily Quota (Opt)",
        proxyLabel: "Use Outbound Proxy",
        proxyDesc:
          "Route API requests (Telegram / Notion) for all destinations of this account through the global outbound proxy.",
        smtpNoProxyNote:
          "SMTP connects directly over native TCP/TLS and does not use HTTP/SOCKS outbound proxies.",
        enabledLabel: "Account Enabled",
        enabledDesc:
          "Pause delivery for all destinations under this account without deleting routes.",
      },
      destModal: {
        newTitle: (accountName: string) => `Add Destination — ${accountName}`,
        editTitle: (destName: string) => `Edit Destination: ${destName}`,
        desc: (accountName: string, kindLabel: string) =>
          `Target under account "${accountName}" (${kindLabel}). Rate limits, quotas, and proxy settings are inherited from the parent account.`,
        nameLabel: "Destination Name",
        nameOptionalLabel: "Destination Name (Optional)",
        namePlaceholderTg: "Leave blank to auto-detect chat or channel title",
        namePlaceholderSmtp: "Leave blank to default to first recipient",
        namePlaceholderNotion: "Leave blank to auto-detect database title",
        enabledLabel: "Destination Enabled",
        enabledDesc:
          "Pause delivery to this specific destination without affecting sibling destinations.",
        createBtn: "Add Destination",
        saveBtn: "Save Destination",
      },
      errors: {
        emptyName: "Account name cannot be empty.",
        emptyDestName: "Destination name cannot be empty.",
        smtpAccountRequired: "SMTP host and From address are required.",
        smtpToRequired: "At least one recipient To address is required.",
        smtpPassword:
          "SMTP password is required for TLS / STARTTLS authentication.",
        tgChatId: "Telegram target chat_id is required.",
        tgToken: "Telegram Bot Token is required when creating an account.",
        notionSecret:
          "Notion Integration Secret is required when creating an account.",
        notionInitInputs:
          "Enter both Notion Integration Secret and Parent Page ID/URL to create a database.",
        notionInitParentOnly:
          "Enter an authorized Parent Page ID or URL to create a database.",
        notionInspectSecret:
          "Enter Notion Integration Secret before inspecting schema.",
        notionInspectTarget:
          "Enter a Notion data_source_id or database ID/URL.",
        notionDataSource:
          "Notion data_source_id is required. Create a database or inspect an existing database first.",
        notionRequiredCols:
          "Notion column mapping requires both Title and FC-ID columns.",
      },
    },
  },

  routes: {
    title: "Routing & Presentation Rules",
    subtitle:
      "Connect feeds or tag groups to delivery destinations with keyword/regex filters, per-route timezone, and Telegram presentation modes.",
    newRoute: "New Route",
    loading: "Loading routing rules…",
    filterKindLabel: "Channel type",
    filterAccountLabel: "Account",
    allKinds: "All channel types",
    allAccounts: "All accounts",
    clearFilters: "Reset filters",
    emptyTitle: "No delivery routes configured",
    emptyDesc:
      "Create a route to connect subscriptions (by feed selection or #tags) to an SMTP, Telegram, or Notion destination.",
    emptyFilteredTitle: "No routes match your current filter",
    emptyFilteredDesc:
      "Try switching the channel type or account filter above.",
    createFirst: "Create First Route",
    previewOn: " · Preview ON",
    previewOff: " · Preview OFF",
    feedFallback: (id: number) => `Feed #${id}`,
    channelFallback: (id: number) => `Channel #${id}`,
    destinationFallback: (id: number) => `Destination #${id}`,
    inheritSystemTzBadge: "Inherit system timezone",
    routeTzBadgeTitle: (tz: string, inherited: boolean) =>
      inherited
        ? `Inheriting system timezone (${tz})`
        : `Route timezone override (${tz})`,
    toastToggleFailed: "Failed to toggle route",
    toastDeleted: "Route deleted",
    toastDeleteFailed: "Failed to delete route",
    toastUpdated: "Route updated",
    toastCreated: "Route created",
    deleteModalTitle: "Delete Delivery Route",
    deleteModalDesc: (name: string) =>
      `Delete route "${name}"? Pending delivery tasks for this route will also be removed.`,
    deleteModalConfirm: "Delete Route",
    allDay: "All day",
    weekdaysShort: ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
    windowBadgeTitle: (tz: string) =>
      `Daily active window${tz ? ` (${tz})` : ""} — tasks outside it stay queued until the next window opens`,
    modes: {
      rich_message: {
        label: "Rich Message",
        summary:
          "Formats full articles with headings, quotes, code blocks, lists, and inline links, plus a Read Original button.",
      },
      compact_card: {
        label: "Compact Card",
        summary:
          "Sends a concise message card with bold title, source tag, summary excerpt, and article link.",
      },
      media_mode: {
        label: "Media Mode",
        summary:
          "Sends extracted images/videos as an album with a 3-line caption (author, opening sentence, link) and stream relay fallback.",
      },
    },
    editor: {
      editTitle: (name: string) => `Edit Route: ${name}`,
      newTitle: "New Delivery Route",
      desc: "Each matched article enqueues an independent delivery task (1 Task = 1 Entry × 1 Route).",
      saveRoute: "Save Route",
      createRoute: "Create Route",
      nameLabel: "Route Name",
      namePlaceholder: "Tech Feeds → Telegram",
      destinationLabel: "Target Destination",
      noDestinationsOption: "— Configure a channel destination first —",
      sourceHeading: "Source Selector (Explicit Feeds OR Tag Match)",
      selectAll: (count: number) => `Select all (${count})`,
      matchTagsLabel: "Match Tags",
      matchTagsPlaceholder: "Match any feed carrying these tags…",
      explicitFeedsLabel: (count: number) =>
        `Explicit Subscriptions (${count} selected)`,
      noFeedsHint:
        "No feeds added yet — you can still bind by #tag above.",
      tgHeading: "Telegram Presentation Mode",
      tgFallbackNotice:
        "This Bot API endpoint reported Rich Message unsupported; deliveries will automatically fall back to Compact Card.",
      linkPreviewLabel: "Enable Webpage Link Preview",
      linkPreviewDesc:
        "Allow Telegram to display a webpage preview card below the Compact Card message.",
      filtersHeading: "Keyword & Regex Filters (Title + Cleaned Body)",
      includeKeywordsLabel: "Include Keywords (Match Any)",
      includeKeywordsPlaceholder: "Leave empty to allow all…",
      excludeKeywordsLabel: "Exclude Keywords (Block Any)",
      excludeKeywordsPlaceholder: "Sponsored, Hiring…",
      includeRegexLabel: "Include Regex (Optional)",
      regexHint: "Evaluated with linear-time Rust regex syntax.",
      excludeRegexLabel: "Exclude Regex (Optional)",
      activeLabel: "Route Active",
      activeDesc:
        "When disabled, newly fetched articles will not enqueue tasks for this route.",
      errEmptyName: "Route name is required.",
      errNoDestination: "Select a target destination first.",
      errNoSource:
        "Select at least one explicit feed or enter at least one #tag.",
      errIncludeRegex: (msg: string) => `Include regex syntax error: ${msg}`,
      errExcludeRegex: (msg: string) => `Exclude regex syntax error: ${msg}`,
      errInvalidTimezone:
        "Select a valid IANA timezone from the list, or leave blank to inherit the system timezone.",
      languageHeading: "Delivery Language",
      languageHint:
        "Language of the fixed push text (e.g. “Read original” buttons, link captions, truncation notices). New routes default to the current interface language.",
      timezoneHeading: "Delivery Timezone",
      timezoneLabel: "Route IANA Timezone (Optional)",
      timezonePlaceholder: (sysTz: string) =>
        `Leave blank to inherit system (${sysTz})`,
      timezoneInheritBtn: "Inherit system",
      timezoneHint:
        "Controls active delivery windows, SMTP article timestamps, and Telegram rich-message footer timestamps for this route.",
      windowsHeading: "Daily Active Windows",
      timezoneBadge: (tz: string, inherited: boolean) =>
        inherited ? `System TZ: ${tz}` : `Route TZ: ${tz}`,
      windowsHint: (tz: string) =>
        `Times use ${tz}. A window whose end is earlier than its start wraps past midnight; identical start and end means all day.`,
      windowsAlways: "No windows configured — this route delivers at any time.",
      windowStart: "Window start",
      windowEnd: "Window end",
      windowWraps: "Wraps midnight",
      addWindow: "Add Window",
      removeWindow: "Remove window",
      weekdaysLabel: "Active Weekdays",
      weekdaysEveryDay: "No day selected — delivers every day.",
      weekdaysHint:
        "A window belongs to the weekday on which it starts (a Monday 22:00–06:00 window ends Tuesday morning).",
      queuedOutsideHint:
        "Tasks outside the active window are not dropped: they stay queued and are sent when the next window opens.",
      errWindowTime: "Each active window needs a valid start and end time (HH:MM).",
      pluginsHeading: "Plugins",
      pluginsActiveCount: (count: number) => `${count} configured`,
      pluginsNoneEnabled:
        "No plugins are currently enabled. Enable a plugin in the Plugins tab to attach it to routes.",
      pluginsGloballyDisabled: "Globally disabled",
      pluginsNoProviders:
        "No providers configured yet — add a provider in Plugins.",
      pluginsMissingProvider: (id: number) => `Provider #${id} (missing)`,
      pluginWarningPluginMissing: "Plugin not found",
      pluginWarningPluginDisabled: "Plugin globally disabled",
      pluginWarningProviderMissing: "Selected provider missing",
      pluginWarningProviderDisabled: "Selected provider disabled",
    },
  },

  plugins: {
    title: "Plugins & Extensions",
    subtitle:
      "Extend FeedCourier's delivery pipeline with built-in plugins and discover upcoming extensions from the plugin marketplace.",
    tabs: {
      installed: "Installed",
      marketplace: "Marketplace",
    },
    loading: "Loading plugins…",
    loadError: "Unable to load plugins:",
    emptyInstalledTitle: "No plugins installed",
    emptyInstalledDesc:
      "Built-in plugins appear here automatically once synchronized with the server.",
    providerCount: (count: number) =>
      `${count} ${count === 1 ? "provider" : "providers"}`,
    configureBtn: "Configure",
    backToPlugins: "Back to Plugins",
    globallyDisabledNotice:
      "This plugin is currently globally disabled. Routes referencing its providers will keep original content until you enable the plugin.",
    hooksLabel: "Hooks",
    permissionsLabel: "Permissions",
    homepageLink: "Homepage",
    detail: {
      providersHeading: (count: number) => `Provider Instances (${count})`,
      addProvider: "Add Provider",
      emptyProvidersTitle: "No provider instances configured",
      emptyProvidersDesc:
        "Add a YOURLS short-link provider instance below, then assign it to individual routes.",
      emptyProvidersGenericDesc:
        "Add a provider instance below, then assign it to individual routes.",
      colName: "Provider",
      colConfig: "Configuration",
      noConfigSummary: "Default settings",
      colApiUrl: "API Endpoint",
      colAuth: "Auth Mode",
      colBehavior: "Mode & Options",
      colCache: "Cached Links",
      colActions: "Actions",
      authModes: {
        signature: "Signature",
        timed_signature: "Timed Signature",
        password: "Password",
      },
      strictBadge: "Strict",
      gracefulBadge: "Graceful",
      passTitleBadge: "Title",
      excludedDomainsBadge: (count: number) => `${count} excluded`,
      cachedLinksCount: (count: number) => `${count}`,
      testBtn: "Test",
      clearCacheBtn: "Clear Cache",
      editTitle: "Edit provider",
      deleteTitle: "Delete provider",
      clearCacheConfirmTitle: "Clear Short-Link Cache",
      clearCacheConfirmDesc: (name: string, count: number) =>
        `Clear ${count} cached short ${count === 1 ? "link" : "links"} for "${name}"? Future deliveries will call the YOURLS API again for previously shortened URLs.`,
      clearCacheConfirmBtn: "Clear Cache",
      deleteConfirmTitle: "Delete Provider Instance",
      deleteConfirmDesc: (name: string) =>
        `Delete provider "${name}" and its encrypted credentials and short-link cache?`,
      deleteConfirmBtn: "Delete Provider",
      conflictModalTitle: "Cannot Delete Referenced Provider",
      conflictModalDesc: (name: string, count: number) =>
        `Provider "${name}" is still assigned to ${count} ${count === 1 ? "route" : "routes"}. Unassign this provider from the routes listed below before deleting it.`,
      conflictRoutesLabel: "Referencing Routes",
      toastPluginEnabled: "Plugin enabled",
      toastPluginDisabled: "Plugin disabled",
      toastPluginToggleFailed: "Failed to toggle plugin",
      toastProviderCreated: "Provider added",
      toastProviderUpdated: "Provider updated",
      toastProviderDeleted: "Provider deleted",
      toastProviderToggleFailed: "Failed to toggle provider",
      toastProviderDeleteFailed: "Failed to delete provider",
      toastTestOk: "Provider connection verified",
      toastTestFailed: "Provider test failed",
      toastCacheClearedTitle: "Short-link cache cleared",
      toastCacheClearedDesc: (count: number) =>
        `Removed ${count} cached ${count === 1 ? "entry" : "entries"}.`,
      toastCacheClearFailed: "Failed to clear cache",
    },
    providerModal: {
      addTitle: "Add YOURLS Provider",
      editTitle: (name: string) => `Edit Provider: ${name}`,
      desc: "Provider credentials are encrypted at rest with ChaCha20-Poly1305. Secret tokens are never returned to the browser.",
      nameLabel: "Provider Name",
      namePlaceholder: "e.g. Primary YOURLS",
      apiUrlLabel: "YOURLS API URL",
      apiUrlPlaceholder: "https://sho.rt/yourls-api.php",
      authModeLabel: "Authentication Mode",
      authModeHints: {
        signature: "Passwordless YOURLS signature token (recommended).",
        timed_signature:
          "Timestamped MD5(timestamp + signature) request signing.",
        password: "Classic YOURLS username and password form authentication.",
      },
      signatureLabel: "Signature Token",
      signaturePlaceholder: "Enter YOURLS signature token…",
      signatureStoredPlaceholder: (hint: string) =>
        `Stored (${hint}) — leave blank to keep`,
      secretPreserveHint: "Leave blank to keep the currently stored secret.",
      usernameLabel: "Username",
      usernamePlaceholder: "admin",
      passwordLabel: "Password",
      passwordPlaceholder: "Enter YOURLS password…",
      passwordStoredPlaceholder: "Stored (••••••••) — leave blank to keep",
      excludeDomainsLabel: "Excluded Domains (Optional)",
      excludeDomainsPlaceholder: "example.com\n*.internal.lan",
      excludeDomainsHint:
        "Comma or newline separated domains to skip shortening (supports *.domain.com wildcards). The provider's own domain is always skipped automatically.",
      strictModeLabel: "Strict Mode",
      strictModeDesc:
        "Retry the entire delivery task if any link fails to shorten, instead of keeping the original URL with a warning.",
      passTitleLabel: "Pass Article Title",
      passTitleDesc:
        "Send the article's title to YOURLS when shortening the primary article link.",
      useProxyLabel: "Use Outbound Proxy",
      useProxyDesc:
        "Send YOURLS API calls through the global HTTP/SOCKS5 outbound proxy configured in System Settings.",
      enabledLabel: "Provider Enabled",
      enabledDesc:
        "Disabled providers keep original URLs with a warning on routes that reference them.",
      ssrfLanNotice:
        "Self-hosted LAN YOURLS note: FeedCourier blocks private/LAN IP addresses by default. Add your YOURLS host or CIDR to the SSRF Allowlist in System Settings if it runs on a private network.",
      createBtn: "Add Provider",
      saveBtn: "Save Provider",
      errEmptyName: "Provider name is required.",
      errInvalidApiUrl:
        "Enter a valid http:// or https:// YOURLS API endpoint URL.",
      errSignatureRequired:
        "Signature token is required for signature authentication.",
      errPasswordAuthRequired:
        "Both username and password are required for password authentication.",
      genericAddTitle: (pluginName: string) => `Add ${pluginName} Provider`,
      genericNoSchemaHint:
        "This plugin does not define additional configuration fields.",
      genericSecretPlaceholder: "Enter secret value…",
      genericSecretStoredPlaceholder: "Stored — leave blank to keep",
      genericArrayHint:
        "Separate multiple values with commas or new lines.",
      genericSelectPlaceholder: "— Select an option —",
      errFieldRequired: (field: string) => `${field} is required.`,
      errFieldInvalidUrl: (field: string) =>
        `${field} must be a valid http:// or https:// URL.`,
      errFieldInvalidNumber: (field: string) =>
        `${field} must be a valid number.`,
      errFieldInvalidInteger: (field: string) =>
        `${field} must be a whole number.`,
    },
    market: {
      searchPlaceholder: "Search plugins by name, category, or description…",
      allCategories: "All categories",
      sourceRemote: "Remote index",
      sourceSnapshot: "Offline snapshot",
      sourceTooltip: (url: string, generatedAt: string | null) =>
        `Index: ${url}${generatedAt ? ` · Generated ${generatedAt}` : ""}`,
      refreshBtn: "Refresh Index",
      toastRefreshed: "Marketplace index refreshed",
      toastRefreshSnapshotTitle: "Remote marketplace unreachable",
      toastRefreshSnapshotDesc:
        "Returned the bundled offline snapshot instead. Try refreshing again later.",
      toastRefreshFailed: "Failed to refresh marketplace index",
      loading: "Loading plugin marketplace…",
      loadError: "Unable to load plugin marketplace:",
      emptyTitle: "No matching plugins found",
      emptyDesc:
        "Try adjusting your search query or switching the category filter.",
      kindBuiltin: "Built-in",
      kindExternal: "External",
      kindComingSoon: "Coming Soon",
      wasmUpcomingBadge: "External / Upcoming WASM",
      installedBadge: "Installed",
      enabledBadge: "Enabled",
      enableAndConfigureBtn: "Enable & Configure",
      configureBtn: "Configure",
      homepageBtn: "Homepage",
      repositoryBtn: "Repository",
    },
  },

  tasks: {
    title: "Outbox Queue & Dead-Letter Log",
    subtitle:
      "Lease-based delivery queue, crash-recovery checkpoints, and failed task diagnostics.",
    retrySelected: (count: number) => `Retry Selected (${count})`,
    retryAllDeadLetter: "Retry All Dead-Letter",
    loading: "Loading outbox tasks…",
    loadError: "Failed to load outbox tasks:",
    emptyTitle: "No delivery tasks in this view",
    emptyDesc:
      "When subscribed feeds publish new articles matching an active route, delivery tasks appear here.",
    colSelect: "Select",
    colEntry: "Entry",
    colRouteChannel: "Route → Destination",
    colStatusTiming: "Status & Timing",
    colDiagnostics: "Diagnostics",
    colActions: "Actions",
    selectTaskAria: (id: number) => `Select task #${id}`,
    payloadPurged: (entryId: number) =>
      `Entry #${entryId} (payload cleaned up)`,
    leasePrefix: (rel: string) => `Lease ${rel}`,
    createdPrefix: (rel: string) => `Created: ${rel}`,
    donePrefix: (rel: string) => `Done: ${rel}`,
    nextPrefix: (rel: string) => `Next: ${rel}`,
    inspectTitle: "Inspect checkpoint & diagnostic log",
    checkpoints: {
      smtp: "SMTP Msg-ID",
      smtpSent: (count: number) => `SMTP ${count} sent`,
      tgCompact: "TG fallback→compact",
      tgBatch: (offset: number) => `TG batch #${offset}`,
      notionAsync: (id: string) => `Notion async:${id}`,
      notionPage: (id: string) => `Notion page:${id}`,
      notionFcId: "Notion FC-ID check",
      notionDefault: "Notion checkpoint",
    },
    modal: {
      title: (id: number) => `Task #${id} Diagnostics`,
      requeueBtn: "Requeue Task",
      status: "Status",
      attempts: "Attempts",
      lastStatusCode: "Last Status Code",
      leaseUntil: "Lease Until",
      nextAttempt: "Next Attempt",
      completedAt: "Completed At",
      resumeCheckpoint: "Resume Checkpoint",
      noCheckpoint: "// No intermediate checkpoint recorded",
      diagnosticLog: "Downstream Diagnostic Log",
      noDiagnosticLog: "No error or diagnostic output recorded.",
    },
    confirmAllTitle: "Retry All Dead-Letter Tasks",
    confirmAllDesc:
      "Requeue every dead-letter task whose article payload is still cached?",
    confirmAllBtn: "Requeue All Dead-Letter",
    toastRequeuedTitle: "Tasks requeued",
    toastRequeuedDesc: (count: number) =>
      `Requeued ${count} ${count === 1 ? "task" : "tasks"} into the outbox queue.`,
    toastRetryFailed: "Retry failed",
    scheduledPrefix: (rel: string) => `Scheduled ${rel}`,
    queue: {
      title: "Global delivery queue",
      running: "Running",
      paused: "Paused",
      runningHint:
        "Due tasks across all feeds are delivered oldest-first as one merged stream.",
      pausedUntil: (at: string, rel: string) =>
        `Paused — auto-resumes ${at} (${rel}). Feeds keep polling and tasks keep queuing.`,
      pausedIndefinitely:
        "Paused until you resume. Feeds keep polling and tasks keep queuing.",
      durationLabel: "Pause duration",
      customLabel: "Resume at",
      presets: {
        forever: "Until resumed",
        "15m": "For 15 minutes",
        "30m": "For 30 minutes",
        "1h": "For 1 hour",
        "3h": "For 3 hours",
        "6h": "For 6 hours",
        custom: "Until a custom time…",
      },
      pauseBtn: "Pause Queue",
      updatePause: "Update Pause",
      resumeBtn: "Resume Now",
      heldBadge: "Held",
      errCustomPast: "Choose a resume time in the future.",
      orderHint:
        "On resume, every due task is delivered in chronological order of the article's publish time.",
      toastPaused: "Delivery queue paused",
      toastResumed: "Delivery queue resumed",
      toastResumeFailed: "Failed to resume the delivery queue",
    },
  },

  settings: {
    title: "System, Account & MCP Server",
    subtitle:
      "Configure the system timezone, administrator account and two-factor authentication, outbound HTTP/SOCKS5 proxy settings, private-network SSRF allowlists, and scoped MCP Bearer tokens.",
    timezoneTitle: "System Timezone",
    timezoneDesc:
      "Default IANA timezone for routes that inherit system timezone (controls active windows, SMTP timestamps, and Telegram footer timestamps).",
    timezoneLabel: "IANA Timezone",
    timezonePlaceholder: "e.g. Asia/Shanghai or America/Los_Angeles",
    timezoneHint:
      "Start typing to search. Existing windows keep their wall-clock times and shift to the new timezone.",
    timezoneUseBrowser: (zone: string) => `Use browser timezone (${zone})`,
    timezoneNow: (time: string, offset: string | null) =>
      `Now ${time}${offset ? ` · ${offset}` : ""}`,
    saveTimezone: "Save Timezone",
    errInvalidTimezone: "Select a valid IANA timezone from the list.",
    toastTimezoneSaved: "System timezone saved",
    publicUrlTitle: "Public Base URL (PUBLIC_URL)",
    publicUrlDesc:
      "Externally reachable base URL for FeedCourier, used to generate RFC 8058 one-click unsubscribe links (/u/…) and SMTP recipient verification links (/v/…).",
    publicUrlLabel: "Public URL",
    publicUrlPlaceholder: "https://feeds.example.com",
    publicUrlHint:
      "Leave empty to disable public HTTP compliance links (SMTP falls back to mailto: unsubscribe and manual 6-digit verification codes).",
    publicUrlEnvManagedBadge: "Env managed",
    publicUrlEnvManagedHint:
      "Managed by the PUBLIC_URL environment variable and locked from WebUI changes.",
    publicUrlHttpWarning:
      "Non-HTTPS URL detected: Gmail, Yahoo, and other major mailbox providers require HTTPS for RFC 8058 one-click unsubscribe links in production.",
    savePublicUrl: "Save Public URL",
    errInvalidPublicUrl:
      "Public URL must start with http:// or https:// and include a valid hostname.",
    toastPublicUrlSaved: "Public URL saved",
    networkTitle: "Outbound Proxy & SSRF Allowlist",
    networkDesc:
      "FeedCourier blocks loopback, link-local, cloud metadata, and private IPs at DNS connect time across every redirect hop. Add internal CIDRs or hostnames below only for trusted self-hosted services (such as FreshRSS, RSSHub, or a Local Bot API Server).",
    proxyLabel: "Global Outbound Proxy URL (HTTP / HTTPS / SOCKS5)",
    proxyPlaceholder:
      "socks5h://127.0.0.1:1080 or http://proxy.lan:7890 (leave empty to disable)",
    proxyHint:
      "Used by feeds and channels that enable the Use Outbound Proxy option.",
    ssrfLabel: "SSRF Private-Network Allowlist (CIDRs & Hostnames)",
    ssrfPlaceholder: "e.g. 10.0.0.5/32, rsshub.internal, *.lan…",
    ssrfHint:
      "Press Enter after each CIDR block or internal hostname. Cloud metadata IPs remain blocked unless explicitly listed as /32 or /128.",
    saveNetwork: "Save Network Settings",
    errInvalidProxy:
      "Outbound proxy URL must start with http://, https://, socks5://, or socks5h://.",
    toastSettingsSaved: "Network settings saved",
    mcpTitle: "MCP Server",
    mcpDesc:
      "Streamable HTTP endpoint for AI agents. Channel secrets stay locked inside the WebUI.",
    copyEndpoint: "Copy endpoint URL",
    toastEndpointCopied: "MCP endpoint URL copied",
    clientLabel: "Token Name",
    clientPlaceholder: "e.g. Claude Desktop or Cursor",
    scopeLabel: "Scope",
    scopeReadonlyShort: "Readonly",
    scopeAdminShort: "Admin",
    scopeReadonlyHint:
      "Inspect feeds, routes, plugins, delivery failures, and preview entries without making changes.",
    scopeAdminHint:
      "Full orchestration: manage feeds & routes, pause/resume queue, retry dead letters, and debug push.",
    viewTools: (count: number) => `View tools (${count})`,
    hideTools: "Hide tools",
    issueToken: "Issue Token",
    errEmptyTokenLabel:
      "Enter a descriptive token label (e.g. Claude Desktop).",
    loadingTokens: "Loading MCP tokens…",
    emptyTokensInline:
      "No MCP tokens issued yet. Create a readonly or admin token above to connect Claude Desktop, Cursor, or another MCP client.",
    colName: "Name",
    colPrefix: "Token Prefix",
    colScope: "Scope",
    colLastUsed: "Last Used",
    colCreated: "Created",
    colRevoke: "Revoke",
    revokeTitle: "Revoke token",
    toastTokenIssued: "MCP Bearer token issued",
    toastTokenRevoked: "MCP token revoked",
    toastRevokeFailed: "Failed to revoke token",
    toastTokenCopied: "Bearer token copied",
    toastJsonCopied: "MCP client config copied",
    revealTitle: "Save Your New MCP Bearer Token",
    revealDesc:
      "Only the SHA-256 hash is stored in SQLite. This plaintext token is shown only once.",
    revealConfirm: "I Have Saved This Token",
    plaintextLabel: (scope: string) => `Plaintext Bearer Token (${scope})`,
    copyToken: "Copy Token",
    clientJsonLabel: "Client Configuration",
    clientTabClaude: "Claude Desktop",
    clientTabCursor: "Cursor",
    clientTabGeneric: "Generic JSON",
    clientTabClaudeHint:
      "Add to claude_desktop_config.json (or paste the endpoint URL directly under Settings → Connectors).",
    clientTabCursorHint: "Add to .cursor/mcp.json in your workspace or home directory.",
    clientTabGenericHint: "Standard Streamable HTTP MCP client configuration.",
    copyJson: "Copy Config",
    revokeModalTitle: "Revoke MCP Token",
    revokeModalDesc: (name: string, prefix: string) =>
      `Immediately revoke "${name}" (${prefix}…)? Connected MCP clients using this token will receive 401 Unauthorized.`,
    revokeModalConfirm: "Revoke Token",
    account: {
      title: "Account & Security",
      desc: "Change the administrator username and password, and protect sign-in with TOTP two-factor authentication.",
      loading: "Loading account…",
      usernameLabel: "Username",
      usernameEnvPrefix: "Managed by the",
      usernameEnvSuffix:
        "environment variable, which overrides the stored username on every restart; change it there.",
      newPasswordLabel: "New Password (optional)",
      newPasswordPlaceholder: "Leave blank to keep the current password",
      confirmPasswordLabel: "Confirm New Password",
      currentPasswordLabel: "Current Password",
      sessionsHint:
        "Required for every change. Changing the password signs out all other browsers.",
      codeLabel: "Authenticator or recovery code",
      codeHint: "6-digit code from your app, or a recovery code (XXXX-XXXX-XXXX).",
      saveBtn: "Save Account Changes",
      errNothingChanged: "Change the username or enter a new password first.",
      errCurrentPassword: "Enter your current password.",
      errUsername: "Username must be 1–64 characters.",
      errMinLength: "New password must be at least 8 characters.",
      errMismatch: "New password confirmation does not match.",
      errCodeRequired: "Enter an authenticator code or a recovery code.",
      toastUpdated: "Account updated",
      toastPasswordChanged: "Password changed; other sessions were signed out",
      twoFaTitle: "Two-Factor Authentication (TOTP)",
      twoFaEnabled: "Enabled",
      twoFaDisabled: "Disabled",
      twoFaDesc:
        "Works with any RFC 6238 authenticator app (1Password, Bitwarden, Google Authenticator, Authy…). Ten one-time recovery codes are issued when you enable it.",
      recoveryRemaining: (n: number) =>
        `${n} recovery ${n === 1 ? "code" : "codes"} left`,
      enable: "Enable 2FA",
      disable: "Disable 2FA",
      regenerate: "Regenerate Recovery Codes",
      continueBtn: "Continue",
      enableTitle: "Enable Two-Factor Authentication",
      enablePasswordDesc: "Confirm your current password to generate a new secret.",
      enableScanDesc:
        "Scan the QR code with your authenticator app, then enter the 6-digit code to confirm.",
      qrAlt: "TOTP setup QR code",
      manualSecret: "Can't scan? Enter this key manually",
      manualSecretHint: "Time-based (TOTP), 6 digits, 30-second period, SHA-1.",
      copySecret: "Copy secret",
      confirmCodeLabel: "6-digit code",
      verifyEnable: "Verify & Enable",
      toastEnabled: "Two-factor authentication enabled",
      codesTitle: "Save Your Recovery Codes",
      codesDesc:
        "Use a recovery code if you lose your authenticator. Each code works once.",
      codesWarning:
        "These codes are shown only once. Store them somewhere safe (a password manager is ideal).",
      codesCopy: "Copy all",
      codesDownload: "Download .txt",
      codesCopied: "Recovery codes copied",
      codesSaved: "I've Saved These Codes",
      regenTitle: "Regenerate Recovery Codes",
      regenDesc:
        "Issues 10 new codes and permanently invalidates all existing ones.",
      regenConfirm: "Regenerate",
      toastRegenerated: "New recovery codes issued",
      disableTitle: "Disable Two-Factor Authentication",
      disableDesc:
        "Sign-in will only require your password again. Confirm with your password and a current authenticator or recovery code.",
      disableConfirm: "Disable 2FA",
      toastDisabled: "Two-factor authentication disabled",
    },
  },
};

type WidenMessages<T> = {
  readonly [K in keyof T]: T[K] extends (...args: infer A) => string
    ? (...args: A) => string
    : T[K] extends object
      ? WidenMessages<T[K]>
      : string;
};

export type Messages = WidenMessages<typeof en>;
