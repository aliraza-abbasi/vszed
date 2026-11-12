# Claude Code VS Code extension parity checklist

Target: the `claude_code` agent UI style in vszed must match the Claude Code VS Code extension in look and behavior. Sources are the official docs (fetched 2026-11-11) plus the user's reference screenshot of the current extension. Where they disagree, the screenshot (the version the user runs) wins. Re-check exact strings against the raw pages before hard-coding them:
- https://code.claude.com/docs/en/vs-code.md
- https://code.claude.com/docs/en/permission-modes.md
- https://code.claude.com/docs/en/checkpointing.md
- https://code.claude.com/docs/en/sessions.md

Status per row: `todo` / `done` / `gap` (not exposed through ACP or the claude-agent-acp adapter; record it, do not fake it).

## Adapter ground truth (claude-agent-acp 0.106.0)

Read from the adapter source (github.com/agentclientprotocol/claude-agent-acp, tag `v0.106.0`, the version the dev profile installed under `~/.vszed-dev/external_agents/registry/claude-acp`). Zed speaks ACP v2 to it and falls back to v1.

**Tool calls** (`src/tool-calls/renderer.ts`, `src/tool-calls/reporters/*`)
- The Claude tool name is `_meta.claudeCode.toolName` over v1. Over v2 the adapter moves it into the tool call's `name` field (`src/v2/tool-call-meta.ts`) and drops it from `_meta`, so Zed sees it as `ToolCall::name` / `tool_name`. The timeline now reads `_meta.claudeCode.toolName`, then a known Claude Code tool name (or `mcp__*`) from `name`, then the `ToolKind` fallback, because other agents (Zed's own) put internal names like `terminal` in `name`.
- Titles: Bash/PowerShell = the command itself (kind `execute`; the `description` input goes into a display text block); Read = `Read <path relative to cwd>` plus ` (start - end)` or ` (from line N)`; Write = `Write <path>`; Edit = `Edit <path>`; Glob = ``Find `path` `pattern` ``; Grep = an equivalent `grep -i -n ... "pattern" path` line (kind `search`); WebFetch = `Fetch <url>` (prompt as display text); WebSearch = `Search "query"` (kind `fetch`); Agent/Task = the task description (kind `think`); TodoWrite/Task* = plan tools (kind `think`); ExitPlanMode = `Approve Plan` / `Exited Plan Mode` (kind `switch_mode`); Skill = `Load skill: name`; MCP and unknown tools = the tool name (kind `other`).
- Content: Bash output goes to a display terminal (`terminal_info` / `terminal_output` / `terminal_exit` `_meta` over v1, display-only terminal updates over v2), not to `rawOutput`. Read output is the line-numbered file text (`N<TAB>line`) wrapped in a code fence that is longer than any backtick run inside it. Edit/Write send `diff` content (old/new text) or a patch.
- Over v2 the terminal output is plain captured stdout with bare LF line endings. Zed fed it to the emulator raw, so each line started where the previous one ended (the "staircase" OUT block). Fixed in `acp_thread` (bare LF becomes CRLF for agent display terminals); the timeline also lays out IN/OUT one row per line.

**Config options** (`src/session-config-options.ts` and friends), in this order:
| id | category | type | values |
|---|---|---|---|
| `mode` | `mode` | select | the session modes below |
| `model` | `model` | select | `default` ("Default (recommended)", description = the SDK display name of the model it resolves to, e.g. "Opus"), then the SDK models: value = SDK value, name = the SDK display name with a version added ("Opus 5.5"), description = the SDK description ("Opus 5.5 · Best for everyday, complex tasks") |
| `effort` | `thought_level` | select | `default`, then the model's levels (`low`, `medium`, `high`, `xhigh`, `max`), named by title-casing the id ("Xhigh"); only when the model supports effort |
| `fast` | `model_config` | boolean (select `on`/`off` without the boolean capability) | "Fast mode"; only when the model supports it |
| `ultracode` | `model_config` | boolean (or `on`/`off`) | "Ultracode"; only when dynamic workflows are on and the model supports `xhigh` |
| `agent` | none | select | "Default" + custom main-thread agents; only when the user configured some |

There is no Thinking toggle option: per the docs Thinking lives in the `/` menu, and the adapter exposes no option for it (gap).

Captured `session/new` (stdio, no prompt, 2026-11-12, Claude subscription, both ACP v1 and v2) gave model options `default` / `opus` "Opus 5.5" / `claude-fable-5-1[1m]` "Fable 5.1" / `sonnet` "Sonnet 5.5" / `haiku` "Haiku 5.5", effort `default`, `low`…`max` (current `xhigh`), `fast` and `ultracode` (booleans over v2). Other sessions list pinned versions too (`claude-opus-4-8`, `claude-haiku-4-5-20251001`, …) next to the family aliases `opus`, `sonnet`, `haiku`, `fable` (the adapter's `MODEL_FAMILY_ALIASES`; the CLI lists its catalog's overflow rows in the same SDK list). The extension (webview `index.js`, 2.1.322) titles a row with the SDK display name and puts the SDK description under it; vszed shows Default plus one row per family (its alias, else its newest version), titles it with the family ("Opus"), and keeps the current row even when it is a pinned version.

The extension's own strings and rules used here come from its installed webview (`~/.vscode-oss/extensions/anthropic.claude-code-2.1.322/webview/index.js` and `index.css`).

**Session modes** (`src/session-mode.ts`): `default` "Manual", `acceptEdits` "Accept edits", `plan` "Plan", `auto` "Auto" (unless disabled by settings), `bypassPermissions` "Bypass permissions" (only when allowed). The extension calls `acceptEdits` "Edit automatically"; vszed maps the ids to the extension's labels. A new session starts in `auto` when available, else `default`, unless `permissions.defaultMode` says otherwise; a resumed session keeps its transcript's mode.

**Permission options** (`src/permissions/options/*`): ids `allow-once` ("Yes"), `allow-with-updates` (e.g. "Yes, and don't ask again for `git status` commands", "Yes, allow all edits during this session"), `allow-skill-exact` / `allow-skill-prefix`, `reject` ("No"). Plan approval (ExitPlanMode): `exit-plan-clear-auto` / `-clear-bypass` / `-clear-accept-edits` ("Yes, clear context (N% used) and use auto mode" etc., only with a plan and a fresh-context turn), `exit-plan-auto` "Yes, and use auto mode", `exit-plan-bypass` "Yes, and bypass permissions", `exit-plan-accept-edits` "Yes, auto-accept edits" (when neither auto nor bypass is available), `exit-plan-default` "Yes, manually approve edits", and `reject` "No, keep planning". EnterPlanMode: "Yes, enter plan mode" / "No, start implementing now". Bash and PowerShell: "Yes", the optional `allow-with-updates` choice, "No" (no "Yes, and switch to auto mode" in this version). Render the names verbatim (B4).

**Permission request presentation** (`src/permissions/presentation.ts`): over ACP v2 the request `title` is the tool call's title (the command itself for Bash/PowerShell), or "Ready to code?" for ExitPlanMode, and `description` is "Reason: ..." when the SDK gives a decision reason. ACP v1 requests carry no title. The plan travels as the ExitPlanMode tool call's display text (a plan file path only for JetBrains AIR clients). `defaultToNo` (preselect "No") is only sent in the JetBrains AIR `_meta`. After approval the adapter publishes the plan as an ACP `plan_update` of type `markdown` with plan id `plan-mode` (`planModeUpdate`), which Zed shows as the "Plan <heading>" row above the prompt.

**`session/list`**: honors `cwd` exactly (`listSessions({ dir: cwd, includeWorktrees: false })`, absolute paths only), so another git worktree of the same repository is a different project, as in `claude --resume`. Pages hold `SESSION_LIST_PAGE_SIZE` sessions with an `offset:N` cursor. Each session carries `cwd`, `title` (the transcript summary) and `updatedAt`. The v1 core hides archived sessions; the v2 core does not filter them. Rename and archive exist only as v1 extension methods (`_session/rename`, `_session/archive`, `_session/unarchive`), which Zed does not call.

**Rate limits** (`rate_limit_event` handling in the adapter; shape from the bundled CLI's `SDKRateLimitInfo` schema): on each Claude `rate_limit_event` the adapter sends a `usage_update` whose `_meta["_claude/rateLimit"]` is the CLI's `rate_limit_info`, verbatim. Before the first assistant message has a usage total it holds the info and attaches it to the next usage update. Every usage update also carries `_meta["_claude/model"]`. Fields: `status` (`allowed` / `allowed_warning` / `rejected`, the only required one), `rateLimitType` (`five_hour`, `seven_day`, `seven_day_opus`, `seven_day_sonnet`, `seven_day_overage_included`, `overage`), `utilization` (a fraction, usually 0..1, above 1 when usage ran past the cap), `resetsAt` (unix seconds, integer), the overage fields (`overageStatus`, `overageResetsAt`, `overageDisabledReason`, `isUsingOverage`, ...), and `unifiedWindows` (`five_hour`, `seven_day`, `seven_day_overage_included`, each `{ utilization, resetsAt, status?, surpassedThreshold? }`, marked `@internal` but sent to SDK hosts). The top-level `rateLimitType` / `utilization` / `resetsAt` describe the window that currently limits requests and are mostly absent while the status is a plain `allowed`; `unifiedWindows` has every window on every event. The CLI emits the event when this state changes (status, limiting window, reset time, or a window's rounded percentage or threshold), not on every response; a new CLI process (one per session) emits on its first response that carries the limit headers. Only claude.ai subscription sign-ins get these headers: API key, Bedrock and Vertex sessions never get a rate limit event. The CLI calls `seven_day_overage_included` "Fable limit".

**Auth status** (`src/auth-status.ts`): the adapter pushes `_auth/status_update` `{authStatus: {kind: account|api_key|gateway|external|none, label, detail?, account?: {email, organization, plan}}}`. It is connection-scoped, advertised by `agentCapabilities._meta.authStatus`, and the adapter also answers ACP `auth/status`. Over ACP v1 vszed rebroadcasts `_`-prefixed notifications through `AgentExtNotifications` (`crates/agent_servers`), but nothing reads `_auth/status_update` yet, so the email and sign-in kind don't reach the UI.

**Thinking** (`src/sdk-to-acp.ts`, `src/v1/acp-agent.ts`, `src/v2/reports.ts`): the adapter sends an `agent_thought_chunk` only for thinking text. Recent models default `thinking.display` to "omitted", which streams signature-only blocks with empty text, and the adapter drops those, so without `showThinkingSummaries` (or `CLAUDE_CODE_THINKING_DISPLAY=summarized`) Zed receives nothing for a thought: no row, no duration. With summaries on, the summary text arrives as thought chunks, which gives the row, its text and a client-measured duration (no start or end time is sent). The extension's live "Thinking... · 1.3k tokens" counter comes from the CLI's `thinking_tokens` system messages (`estimated_tokens`); the v1 core drops them (`case "thinking_tokens": // Todo`) and the v2 core doesn't handle them. `usage_update` is sent only on `message_start` and `message_delta`, so its `used` doesn't move while Claude thinks.

**Fork and rewind**: the adapter implements ACP `session/fork` (v1 `unstable_forkSession`, v2 `session/fork`, advertised in session capabilities). Without `_meta` it copies the whole transcript; a fork point is only read from `_meta.jetbrains.air.fork.messageId` (an ACP assistant `messageId`, resolved to the SDK uuid through the session's `messageIdToUuid` map or the transcript, then `forkSession(id, { upToMessageId })`). Zed's ACP client has no fork request at all (no `fork` in `acp_thread` or `agent_servers`), and its `AcpThread::rewind` truncates the conversation through `AgentConnection::truncate`, which external ACP agents don't provide. File rewind exists only as the SDK's `Query.rewindFiles(userMessageUuid)`, which the adapter calls with `dryRun` for its own change audit and exposes through no ACP method or extension. Bookmarks are the extension's own feature (its webview's bookmark host keyed by message uuid), not in the adapter or SDK.

**Raw SDK messages**: `_meta.claudeCode.emitRawSDKMessages` (a list of `{type, subtype?}` filters) on `session/new`, `session/load` and `session/resume` makes the v1 core send `_claude/sdkMessage` notifications (`{ sessionId, message }`). The v2 core ignores it; over v2 subagents are child sessions (`subagent_update`) and background work is the session state's `background` list (`BackgroundWork::Terminal` / `ToolCall` / `Subagent`).

**Not exposed**: prompt-cache time left (vszed estimates it, like the extension, from the last model request; the lifetime only over v1), account identity through ACP (the adapter sends it as `_auth/status_update`, which reaches `AgentExtNotifications` over ACP v1 but nothing reads yet; the email is taken from Claude Code's `~/.claude.json` instead), the `/usage` dialog's data (the CLI's `get_usage` control request), a Thinking toggle.

## Placement and entry points

| Behavior | Extension | vszed mapping | Status |
|---|---|---|---|
| Spark icon in the activity bar opens the sessions list | yes | "Claude Code" panel in the left dock (B6, `agent::ToggleClaudeCodeSessions`), present only in the Claude Code style; the Track A activity bar picks it up | done |
| `✻ Claude Code` status bar item, bottom right | yes | `ClaudeCodeStatusItem`, present only in the Claude Code style; opens the visible or first Claude tab, or a new one (the agent panel with `thread_location: panel`) | done |
| Spark icon in the editor toolbar, top right, when a file is open | yes | pane tab-bar button | todo |
| Default location: editor tab (`preferredLocation: panel`), or right sidebar | yes | `agent.thread_location: editor` / `panel`. `agent: use Claude Code style` sets `agent.ui_style: claude_code` and `agent.thread_location: editor` (remembering the previous values); `agent: restore previous ui style` puts them back | done |
| Multiple conversations as separate tabs | yes | one `AgentThreadItem` per session (B5); tabs restore after restart from the saved session id | done |
| Tab status dot: blue = permission pending, orange = finished while hidden | yes | dot on the tab's agent icon (B5) | done |
| Clicking a session that is already open focuses its tab | yes | B5/B6: tabs are matched by session id or Zed thread id | done |

## Sessions

Closing a thread tab while a turn runs asks for confirmation when closed with `pane::CloseActiveItem` (Cmd+W); the tab's close button closes without asking (gap: Zed has no per-item close veto).

| Behavior | Extension | vszed mapping | Status |
|---|---|---|---|
| Session list scoped to the current project (user requirement; matches `claude --resume`) | per screenshot and user | ACP `session/list` with `cwd` = each visible worktree root (the adapter matches it exactly), results filtered again by `work_dirs` (B6) | done |
| Search by keyword, browse by time | yes | title search; rows sorted by update time with "now", "5m", "2h", "Yesterday", "3d", dates (B6) | done |
| AI-generated titles from the first message | yes | the session title from `session/list` (Claude's transcript summary); a Zed rename overrides it | done |
| Hover actions: Rename, Archive; "Archived sessions" group with "Unarchive session" | yes | B6: Zed-side rename (title override) and archive in `ThreadMetadataStore`; not written back to Claude's store (the adapter's `_session/rename` / `_session/archive` are v1 extensions Zed doesn't call) | partial |
| Auto-archive inactive sessions after 14 days (setting, 0 = off) | yes | setting + B6 | todo |
| Sessions started in the terminal CLI appear and resume | yes | from ACP `session/list`; resume through `ConversationView::new` with the session id | done (not verified against a live CLI session) |
| "Open here anyway" notice when the session is open in another Claude process | yes | | gap? (check adapter) |
| Account & usage header (docs: "Check account and usage"; screenshot: collapsible ACCOUNT & USAGE above SESSION MANAGER) | yes | top section of the sessions panel. ACCOUNT: Email from `oauthAccount.emailAddress` in Claude Code's `~/.claude.json` ("Not available" when absent). USAGE: shown immediately from the file's `cachedUsageUtilization.utilization.limits` in the server's order with its labels ("Weekly (7 day)", "<model> limit"; "Session (5 hour)" for `session` is a guess), percent, bar, reset time, and warning / critical / limit colours; a live `_claude/rateLimit` reading replaces a row when it is newer than the cache's `fetchedAtMs`; readings of closed threads are kept per project in memory; "Usage appears after Claude's next reply" only when neither source has data. "View details" left out | partial (API-key / third-party cost header and the dialog are gaps) |
| API key / third-party sign-ins: header shows the active session's cost and tokens (docs) | yes | | gap: telling the sign-in apart needs the adapter's `_auth/status_update` kind, which now reaches `AgentExtNotifications` (ACP v1) but is not read yet; cost alone can't decide it, since it may be present for subscription sessions too |
| "+ New session" row, Local / Web toggle | yes | plain row; Web disabled with a tooltip (claude.ai cloud sessions are not in ACP) | done (Web gap) |
| Filter the sessions list: Active toggle ("⚡ Active · N"), funnel with Needs input / Working / Completed / Open / Closed; archived hidden while filtering; persists across reloads | yes | as documented; Open = has a tab in this window (sessions running in other Claude processes can't be seen); filters persist in the KV store | done |
| Organize sessions into named, collapsible groups (create from a session, move, remove, rename, delete; saved per workspace folder; search is flat) | yes | context menus on rows and group headers plus "+ New group"; saved per set of project folders in the KV store; "Add Session Tab to Group" command not added | done (command todo) |
| Search behind the search icon | screenshot | search field toggled by the toolbar icon, Esc closes it | done |
| Web tab (claude.ai cloud sessions) | yes | | gap (not in ACP) |
| Resume restores the permission mode, including plan mode | yes | ACP session mode on load | todo |

## Prompt box

| Behavior | Extension | vszed mapping | Status |
|---|---|---|---|
| Enter sends, Shift+Enter newline; `useCtrlEnterToSend` option | yes | `MessageEditor` + setting | todo |
| `@` files (fuzzy), `@folder/`, `@file#5-10`, `@terminal:name` | yes | `PromptCompletionProvider` (files/folders exist; line ranges and terminal need checking) | todo |
| Option+K / Alt+K inserts an @-mention for the current selection | yes | action + keybinding | todo |
| Selection and open file shared automatically, shown in the footer (selected line count, file name); settings `attachSelection`, `attachOpenFile` | yes | add context mention automatically on send | todo |
| Paste images; Shift+drag files in | yes | `MessageEditor` inside the prompt box (images supported); `+` opens Zed's add-context menu | done |
| Prompt and sent messages in the UI font | screenshot | `MessageEditor` text style swapped to the UI font family and agent UI font size in this style | done |
| `+` attachment menu contents | the docs only mention an "attachment menu" with Chrome browser tools; its entries are undocumented | Zed's menu kept as is (Files & Directories, Symbols, Threads, Image, Selection, Branch Diff) | gap (undocumented) |
| Approved plan above the prompt | not documented | the adapter's `plan-mode` markdown plan in Zed's "Plan <heading>" row; the activity bar (plan, edited files, queue) is drawn with the prompt box's width, radius and colors, behavior unchanged | done (look only) |
| `/` button and typed `/` open the slash menu (Switch model, Output styles, Hooks, Permissions, Memory, Status, Thinking, ...) | yes | `/` button types a `/` (with a leading space after text) and opens the completions: ACP `available_commands` + Zed's local items. Extension-only entries (Switch model, Thinking, ...) are not added | partial |
| Model name button at the bottom; shows effort level, e.g. "Opus 5.5 Extra high", and "· Ultracode" when on | yes | model chip from the `model` + `effort` options; `xhigh` shows as "Extra high"; the `default` model shows the model it resolves to | done |
| Model menu: "Select a model", Default + one row per family (title, muted "<model> · <use>", check on the current one), Effort row with the dot slider and "(Extra high)", Ultracode switch | yes (webview) | `ClaudeCodeModelPicker` above the prompt box, at its width; Up/Down/Enter/Esc; clicking Effort cycles levels, clicking a dot picks one; pinned versions left out | done |
| Fast mode | the extension's picker has no Fast mode; it is "Toggle fast mode" (`/fast`) in the `/` menu's Model section | not in the picker; vszed's `/` menu only lists the adapter's commands | gap |
| Thinking switch (docs: in the Model section of the `/` menu) | yes | | gap (no adapter option) |
| Permission mode indicator labels: **Manual**, **Edit automatically**, **Plan**, **Auto**, **Bypass permissions** (Bypass only when allowed) | yes | mode chip from the `mode` option; ids `default`, `acceptEdits`, `plan`, `auto`, `bypassPermissions` mapped to these labels; the adapter only offers Bypass when allowed | done |
| Mode cycle shortcut | not documented for VS Code (Shift+Tab is CLI only) | chip is click-only; Zed's own `agent::CycleModeSelector` binding still works because it is stock behavior | done |
| Context usage indicator; `/compact` | yes | Zed's token usage ring in the footer, from `thread.token_usage()` (fed by the adapter's `usage_update`); `/compact` is an adapter command | done |
| Clock chip with prompt-cache time left (e.g. "58m"; red icon when expired, tooltip "Prompt cache likely expired (idle 1h 18m)."; "Prompt cache warm, about N min left."; red after a compaction) | yes | `CacheClockElement`: counts down from the latest model request; lifetime from v1 `result` usage, else one hour; hidden until this view sees a response (a resumed session has no request time) | done (estimate) |
| "● N agents" pill (shown once the session has agents; N = agents still working; dot: blue waiting, green running, red failed, grey idle; tooltips as the extension's), opens the agent map | yes | ACP v2 subagent sessions, or v1 `task_*` SDK messages | done |
| Agent map: "Agent map", "N agents · click an agent for details", the session ("<model> · N tokens in context") with its agents and their agents as a tree, "N background tasks · click a task for details" with "shell · 971m 37s" rows, click for details | yes | `AgentMap` modal; details show status, run time, tokens, spawned by, latest activity; no transcript, Stop or prompt (gap) | done / partial |
| Background agent line: "● Working in the background: <activity> · type /tasks to see its transcript" under the call that started it | yes | under the Agent/Task tool call (v1) or the subagent card (v2) of an agent that runs in the background; `/tasks` itself is not handled by vszed (gap) | done |
| Stop button and Esc end the turn | yes | send button turns into an accent stop square while a turn runs and the editor is empty; Esc is Zed's `editor::Cancel` | done |
| Sending while busy queues the message: "Message queued" | yes | existing message queue (Zed's queue list above the box) | done |
| Placeholder text | webview | "Queue another message…" while a turn runs; idle "⌘ Esc to focus or unfocus Claude" ("ctrl esc …" off macOS). The extension's selection ("⌘ Esc to attach selected text") and prompt-suggestion placeholders are not mirrored | done |
| Prompt box floats over the transcript: no band, centered, at most 680px, opaque, one-line input that grows to ~200px, 150px fade of the transcript behind and beside it; Zed's plan / edits / queue rows float above it at its width | screenshot + webview CSS | `render_ui_style_conversation` | done |
| Focus ring and send button colors follow the mode: Claude orange (#d97757 ring, #c6613f button) in manual, text color for Edit automatically, focus color for Plan, error color for Auto and Bypass; send button dimmed when empty | webview CSS | `prompt_accent` | done |
| Boxed `/` button, "⚡ Auto" mode icon, footer pills | screenshot | drawn in `prompt_box.rs` | done |
| Orange ✻ on Claude tabs | screenshot | Claude thread tabs tint their icon in the Claude Code style | done |

## Conversation

The transcript spans the full editor width (Zed's `max_content_width` is not applied) and its text is drawn at 14/16 of the agent UI font size, VS Code's 13px with the layout's `ui_font_size: 15`.

Sent user messages, from the extension's webview (2.1.322): attachments (images) as a chip row with a thumbnail, the file name ("image.png" for pasted images, as VS Code names them) and the pixel size; then the text in the UI font with mentions as "@name", clamped to 60px with a 50px fade; "Show more" (a light pill at the bottom right) only while hovered, and "Show less" under an expanded message. Clicking the box focuses the message editor, which switches to the full editor to edit and resend, as before (only where the agent supports rewinding).

| Behavior | Extension | vszed mapping | Status |
|---|---|---|---|
| Timestamps on messages (`showMessageTimestamps`, default on): muted time right-aligned above each user message and each run of assistant text | yes | always on (no setting yet). Times are recorded when a message first appears in the view; messages replayed from a loaded session have none, because the adapter sends no timestamps (the extension reads them from the transcript) | done (live messages) / gap (loaded history) |
| Date line where the day changes (wavy rule, "Sun, Nov 8", year added for other years), also above the first message with a time | yes | drawn above the time of the first message of each day | done (live messages) |
| Hover a response: **Copy response** | yes | copy icon under each finished turn, lined up with the reply text, shown on hover; Zed's scroll-to-prompt, scroll-to-top, feedback and turn stats are not shown in this style | done |
| Hover a response: **Bookmark response**, Bookmarks panel | yes | not rendered (no dead button) | gap (the extension's own feature, not in the adapter or SDK; could be done locally later) |
| Sent message: attachment chips, text on its own line, clamp with "Show more" / "Show less" | webview | see above | done |
| Hover a message: rewind button (top-right corner) with "Fork conversation from here", "Rewind code to here", "Fork conversation and rewind code" | yes | button and menu drawn; the three entries are disabled with the reason beside them (see Fork and rewind above) | gap (fork: Zed's client has no `session/fork`; rewind code: not exposed by the adapter) |
| Scroll to bottom on send (`scrollToBottomOnSend`) | yes | B2 | todo |
| Timeline: dot per step, bold tool name + description, IN/OUT blocks | screenshot | B2; tool names from the adapter (see above); IN/OUT laid out one row per line | done |
| OUT: clipped to 60px, lines unwrapped and cut at the block edge, fading at the bottom | webview | same | done |
| OUT click (only for output over 250 characters or 3 lines): opens the full text in a read-only "<Tool> tool output" tab (aria: "Press Enter to open the full text in an editor tab"); it lands beside the conversation because the extension locks the conversation's editor group | webview, user | read-only tab in the pane right of the active one (or a new split); clicking again focuses it while open; unlike the extension's snapshot, it follows the output while the tool call streams (display terminals included) | done |
| Bash IN: copy button at the top right while hovered | webview | copy button on execute-kind rows | done |
| IN clipped to 60px; long IN opens as a "command" / "<Tool> tool input" tab | webview | IN left showing every line | todo |
| Thinking row: "Thinking..." while streaming, "Thought for Ns" (rounded, 0s included), "Thinking" without a duration; muted grey dot | webview | same labels; duration measured from the previous activity until the next step arrives | done |
| Thinking summary: chevron (rotates when open) only when the thought has text; a click opens or closes every summary in the conversation; summary in muted text | docs, webview | same; summary indented with a guide line from the dot (the extension's line is its timeline connector) | done |
| Thinking rows without summaries ("Thought for 12s" with no text) | docs | | gap (claude-agent-acp drops signature-only thinking, so nothing arrives) |
| Live "Thinking... · 1.3k tokens" counter | webview, user | | gap (the adapter drops the CLI's `thinking_tokens` messages; no live usage during thinking) |
| `Ctrl+O` expands every thinking block | docs | | todo |
| "Conversation compacted" row | yes | `ContextCompaction` entry | todo |
| "Run in background" under long tool calls / subagents | yes | | gap? |
| Error texts ("Claude Code process exited with code N", ...) | yes | map ACP errors | todo |

## Permissions and plan

| Behavior | Extension | vszed mapping | Status |
|---|---|---|---|
| Permission prompt in the prompt box's place: bold title, numbered options, highlighted option (docs screenshot) | yes | B4 card: replaces the prompt box while a request is pending and takes focus from the prompt; `1`-`9` pick an option, Up/Down move the highlight, Enter picks it, Esc picks the adapter's "No" (`reject`). Title is the adapter's request title (ACP v2) or the tool call title (ACP v1); the extension's own wording ("Make this edit to utils.py?") is not sent by the adapter | done |
| "Tell Claude what to do instead" field in the prompt | yes | | gap (ACP permission outcomes carry no text; reject, then send a message) |
| "No" preselected for risky requests | not documented | | gap (`defaultToNo` only reaches JetBrains AIR clients) |
| Manual mode edits: side-by-side diff of original vs proposed in the editor, then accept / reject / tell Claude what to do instead; editing the proposed side is reported to Claude | yes | B4 card done; B7 (diff tab driven by the pending tool call) | todo |
| Option labels end with where approval is saved ("this session", "all projects") | yes | ACP permission option names rendered verbatim (B4) | done |
| Bash prompt: "Yes, and switch to auto mode" | yes | | gap (claude-agent-acp 0.106.0 offers no such option) |
| Plan approval: "Yes, and use auto mode" (or "Yes, auto-accept edits"), "Yes, manually approve edits", "No, keep planning" | yes | B4 card with the adapter's option names and the plan markdown above them | done |
| Plan opens as a full Markdown document with inline comments before Claude starts | yes | the card shows the plan inline; a markdown editor tab is still to do, and inline comments have no ACP channel | todo / gap |
| Left/Right arrows change a permission prompt's destination | yes | | gap (ACP options are fixed choices; the destination is not exposed) |

## Edits, diffs, checkpoints

| Behavior | Extension | vszed mapping | Status |
|---|---|---|---|
| Per-change "Accept this change" / "Reject this change" in the diff (none when more than 100 changes) | yes | `AgentDiffPane` hunk keep/reject | todo |
| "Accept/Reject Change at Cursor" commands | yes | actions | todo |
| Hover a message for rewind: "Fork conversation from here", "Rewind code to here", "Fork conversation and rewind code" | yes | menu drawn, entries disabled; fork needs a `session/fork` request in Zed's ACP client plus the fork point through the adapter's `_meta.jetbrains.air.fork.messageId`; file rewind is not exposed by the adapter | gap |

## Keyboard shortcuts (macOS / Windows-Linux)

| Command | Binding | Status |
|---|---|---|
| Focus input / toggle between editor and Claude | Cmd+Esc / Ctrl+Esc | done (`agent::ToggleThreadTabFocus`; overrides `menu::Cancel` on Cmd/Ctrl+Esc only while the feature is on; on Windows Ctrl+Esc opens the Start menu) |
| Open in new tab | Cmd+Shift+Esc / Ctrl+Shift+Esc | done (`agent::NewThreadTab`; on Windows Ctrl+Shift+Esc opens Task Manager before Zed sees it) |
| New conversation (opt-in setting) | Cmd+N / Ctrl+N | todo |
| Reopen closed session (default on) | Cmd+Shift+T / Ctrl+Shift+T | todo (conflicts with Zed reopen closed item: scope to the Claude tab context) |
| Insert @-mention of the selection | Option+K / Alt+K | todo |
| Stop current turn | Esc | done (stock `editor::Cancel` in the message editor) |

## Extension settings to mirror (under `agent.claude_code.*` or similar)

`preferredLocation`, `initialPermissionMode`, `attachSelection`, `attachOpenFile`, `useCtrlEnterToSend`, `scrollToBottomOnSend`, `showMessageTimestamps`, `enableNewConversationShortcut`, `enableReopenClosedSessionShortcut`, `archiveInactiveSessions`, `allowDangerouslySkipPermissions`, `spinnerVerbs`. Shared Claude settings (`~/.claude/settings.json`: `modelSettings`, `showThinkingSummaries`, `permissions.defaultMode`) are read by Claude itself, not by vszed.

## Out of scope / gaps to confirm with the adapter

Account & usage dialog (`/usage`, "View details") and account identity, Web sessions, prompt-cache clock, Remote Control, Claude in Chrome (`@browser`), dictation, Focus view, terminal mode (`useTerminal`).
