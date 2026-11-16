---
description: "Sync RSS, Atom, and JSON Feed entries into a Notion database with structured page properties, tags, and full article content converted to Notion blocks."
---

# Notion

The Notion channel saves each matched feed entry as a page in a Notion database, writing both structured database properties (title, link, source, publish date, reading state, and tags) and the full article body converted into Notion blocks.

---

## 1. Creating a Notion Integration & Connecting Your Page

1. Open the [Notion Integrations dashboard](https://www.notion.so/profile/integrations) and click **New integration** to create an **Internal Integration**.
2. Under **Capabilities**, ensure **Read content**, **Update content**, and **Insert content** are enabled, then copy the **`Internal Integration Secret`** (starts with `ntn_` or `secret_`).
3. In Notion, open the parent page (where you want FeedCourier to create a new database) or your existing reading database, click the **`•••`** menu in the top-right corner → **Connections**, and connect the integration you just created.

---

## 2. Setting Up the Channel in the WebUI

In the WebUI, go to **Channels → New Channel → Notion**. You can choose between two setup methods:

### Option 1: One-Click Database Initialization Under a Parent Page

If you want FeedCourier to create a ready-to-use reading database for you:
1. Enter your **Integration Secret** and the **Parent Page ID or URL** that you shared with the integration.
2. Click **Initialize Standard Database**.
3. FeedCourier creates a database under that parent page with **7 standard properties** and binds it to the channel automatically:

| Property Name | Notion Type | Required | Description |
| :--- | :---: | :---: | :--- |
| **`Title`** | `title` | **Required** | Article title. |
| **`FC-ID`** | `rich_text` | **Required** | Stores the article's SHA-256 content hash to prevent duplicate pages on retry. |
| **`Link`** | `url` | Optional | Canonical URL of the original article. |
| **`Source`** | `select` | Optional | Feed title. |
| **`Published`** | `date` | Optional | Original publication date and time. |
| **`State`** | `select` | Optional | Reading status (`UnRead` and `Read`); new entries default to `UnRead`. |
| **`Tag`** | `multi_select` | Optional | Synced from the feed's configured tags in FeedCourier. |

### Option 2: Bind an Existing Database with Column Mapping

If you already have a Notion database for reading or knowledge management:
1. Select **Bind Existing Data Source** and paste your **Database URL**, **Database ID**, or **Data Source ID**.
2. Click **Inspect & Repair Schema** to check the database's existing columns:
   - **Required Columns**: Only **`Title`** (`title`) and **`FC-ID`** (`rich_text`) are required.
   - **Optional Columns**: `Link`, `Source`, `Published`, `State`, and `Tag` can be mapped to your own custom column names or left unmapped (`null`) if you do not need them.
   - **One-Click Column Patching**: If your database is missing `FC-ID` or any other standard columns you want to use, enable auto-patching in the wizard to add the missing properties automatically.

---

## 3. What Gets Synced to Notion

- **Clean Page Layout**: Each created page uses the feed site's favicon as its page icon and writes the article body directly as Notion blocks (headings, paragraphs, lists, quotes, code blocks, and images) without extra banner clutter.
- **Nested Code Block Support**: Code blocks that contain triple backticks inside their content (common in technical tutorials) are automatically formatted with expanded outer fences so Notion renders them cleanly.
- **Long Article Support & Safe Truncation**: Long articles are imported asynchronously when needed, and extraordinarily large posts that exceed Notion's per-page block limit are truncated at a clean paragraph boundary with a link to the original article at the bottom.
- **Duplicate Prevention**: Before retrying an interrupted delivery, FeedCourier checks the database's `FC-ID` property to verify whether the page was already created, preventing duplicate entries.

---

## Technical Details

For the target Notion API version, data source schema calls, Markdown conversion pipeline, and asynchronous page creation workflow, see [Channel Protocols & Delivery](/reference/channel-protocols).
