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
| `model` | `model` | select | `default` ("Default (recommended)", description = the resolved model, e.g. "Opus 5.5"), then the SDK models by id |
| `effort` | `thought_level` | select | `default`, then the model's levels (`low`, `medium`, `high`, `xhigh`, `max`), named by title-casing the id ("Xhigh"); only when the model supports effort |
| `fast` | `model_config` | boolean (select `on`/`off` without the boolean capability) | "Fast mode"; only when the model supports it |
| `ultracode` | `model_config` | boolean (or `on`/`off`) | "Ultracode"; only when dynamic workflows are on and the model supports `xhigh` |
| `agent` | none | select | "Default" + custom main-thread agents; only when the user configured some |

There is no Thinking toggle option: per the docs Thinking lives in the `/` menu, and the adapter exposes no option for it (gap).

**Session modes** (`src/session-mode.ts`): `default` "Manual", `acceptEdits` "Accept edits", `plan` "Plan", `auto` "Auto" (unless disabled by settings), `bypassPermissions` "Bypass permissions" (only when allowed). The extension calls `acceptEdits` "Edit automatically"; vszed maps the ids to the extension's labels. A new session starts in `auto` when available, else `default`, unless `permissions.defaultMode` says otherwise; a resumed session keeps its transcript's mode.

**Permission options** (`src/permissions/options/*`): ids `allow-once` ("Yes"), `allow-with-updates` (e.g. "Yes, and don't ask again for `git status` commands", "Yes, allow all edits during this session"), `allow-skill-exact` / `allow-skill-prefix`, `reject` ("No"). Plan approval (ExitPlanMode): `exit-plan-clear-auto` / `-clear-bypass` / `-clear-accept-edits` ("Yes, clear context (N% used) and use auto mode" etc., only with a plan and a fresh-context turn), `exit-plan-auto` "Yes, and use auto mode", `exit-plan-bypass` "Yes, and bypass permissions", `exit-plan-accept-edits` "Yes, auto-accept edits" (when neither auto nor bypass is available), `exit-plan-default` "Yes, manually approve edits", and `reject` "No, keep planning". Render the names verbatim (B4).

**`session/list`**: honors `cwd` exactly (`listSessions({ dir: cwd, includeWorktrees: false })`, absolute paths only), so another git worktree of the same repository is a different project, as in `claude --resume`. Pages hold `SESSION_LIST_PAGE_SIZE` sessions with an `offset:N` cursor. Each session carries `cwd`, `title` (the transcript summary) and `updatedAt`. The v1 core hides archived sessions; the v2 core does not filter them. Rename and archive exist only as v1 extension methods (`_session/rename`, `_session/archive`, `_session/unarchive`), which Zed does not call.

**Not exposed**: prompt-cache time left (the clock chip), account usage, a Thinking toggle, a running-agents count beyond what subagent tool calls show.

## Placement and entry points

| Behavior | Extension | vszed mapping | Status |
|---|---|---|---|
| Spark icon in the activity bar opens the sessions list | yes | "Claude Code" panel in the left dock (B6, `agent::ToggleClaudeCodeSessions`), present only in the Claude Code style; the Track A activity bar picks it up | done |
| `✻ Claude Code` status bar item, bottom right | yes | status item (B6) | todo |
| Spark icon in the editor toolbar, top right, when a file is open | yes | pane tab-bar button | todo |
| Default location: editor tab (`preferredLocation: panel`), or right sidebar | yes | `agent.thread_location: editor` / `panel`; the style does not switch it yet (the preset action is still todo) | partial |
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
| Web tab (claude.ai cloud sessions), Account & usage header | yes | | gap (not in ACP) |
| Resume restores the permission mode, including plan mode | yes | ACP session mode on load | todo |

## Prompt box

| Behavior | Extension | vszed mapping | Status |
|---|---|---|---|
| Enter sends, Shift+Enter newline; `useCtrlEnterToSend` option | yes | `MessageEditor` + setting | todo |
| `@` files (fuzzy), `@folder/`, `@file#5-10`, `@terminal:name` | yes | `PromptCompletionProvider` (files/folders exist; line ranges and terminal need checking) | todo |
| Option+K / Alt+K inserts an @-mention for the current selection | yes | action + keybinding | todo |
| Selection and open file shared automatically, shown in the footer (selected line count, file name); settings `attachSelection`, `attachOpenFile` | yes | add context mention automatically on send | todo |
| Paste images; Shift+drag files in | yes | `MessageEditor` inside the prompt box (images supported); `+` opens Zed's add-context menu | done |
| `/` button and typed `/` open the slash menu (Switch model, Output styles, Hooks, Permissions, Memory, Status, Thinking, ...) | yes | `/` button types a `/` (with a leading space after text) and opens the completions: ACP `available_commands` + Zed's local items. Extension-only entries (Switch model, Thinking, ...) are not added | partial |
| Model name button at the bottom; shows effort level, e.g. "Opus 5.5 Extra high", and "· Ultracode" when on | yes | model chip from the `model` + `effort` options; `xhigh` shows as "Extra high"; the `default` model shows the model it resolves to | done |
| Model menu: model list, Effort row, Fast mode and Ultracode switches | yes | model chip popover (checkmark toggles rather than switches) | done |
| Thinking switch (docs: in the Model section of the `/` menu) | yes | | gap (no adapter option) |
| Permission mode indicator labels: **Manual**, **Edit automatically**, **Plan**, **Auto**, **Bypass permissions** (Bypass only when allowed) | yes | mode chip from the `mode` option; ids `default`, `acceptEdits`, `plan`, `auto`, `bypassPermissions` mapped to these labels; the adapter only offers Bypass when allowed | done |
| Mode cycle shortcut | not documented for VS Code (Shift+Tab is CLI only) | chip is click-only; Zed's own `agent::CycleModeSelector` binding still works because it is stock behavior | done |
| Context usage indicator; `/compact` | yes | Zed's token usage ring in the footer, from `thread.token_usage()` (fed by the adapter's `usage_update`); `/compact` is an adapter command | done |
| Clock chip with prompt-cache time left (e.g. "60m"; red when expired) | yes (the "60m" chip in the screenshot) | | gap (the adapter reports no cache expiry) |
| "N agents" indicator, opens the agent map | yes | subagent list from `AcpThread` | todo |
| Stop button and Esc end the turn | yes | send button turns into an accent stop square while a turn runs and the editor is empty; Esc is Zed's `editor::Cancel` | done |
| Sending while busy queues the message: "Message queued" | yes | existing message queue (Zed's queue list above the box) | done |
| Placeholder text | not documented | "Queue another message…" while a turn runs (screenshot); idle: "Message <agent name>…" (neutral choice, the extension's idle text is undocumented) | done |

## Conversation

User messages render as plain UI-font text in the bordered box (the read-only message editor drawn with the UI font and no background); clicking switches to the full editor to edit and resend.

| Behavior | Extension | vszed mapping | Status |
|---|---|---|---|
| Timestamps on messages (`showMessageTimestamps`, default on) | yes | B2 (always on; no setting yet) | done |
| Scroll to bottom on send (`scrollToBottomOnSend`) | yes | B2 | todo |
| Timeline: dot per step, bold tool name + description, IN/OUT blocks, "Thought for Ns" | screenshot | B2; tool names from the adapter (see above); IN/OUT laid out one row per line | done |
| Thinking summaries only when `showThinkingSummaries` is on | yes | B2 | todo |
| "Conversation compacted" row | yes | `ContextCompaction` entry | todo |
| "Run in background" under long tool calls / subagents | yes | | gap? |
| Error texts ("Claude Code process exited with code N", ...) | yes | map ACP errors | todo |

## Permissions and plan

| Behavior | Extension | vszed mapping | Status |
|---|---|---|---|
| Manual mode edits: side-by-side diff of original vs proposed in the editor, then accept / reject / tell Claude what to do instead; editing the proposed side is reported to Claude | yes | B4 + B7 (diff tab driven by the pending tool call) | todo |
| Option labels end with where approval is saved ("this session", "all projects") | yes | render ACP permission option names verbatim | todo |
| Bash prompt: "Yes, and switch to auto mode" | yes | from adapter options | todo |
| Plan approval: "Yes, and use auto mode" (or "Yes, auto-accept edits"), "Yes, manually approve edits", "No, keep planning" | yes | B4 | todo |
| Plan opens as a full Markdown document with inline comments before Claude starts | yes | open plan as a markdown editor tab | todo |
| Left/Right arrows change a permission prompt's destination | yes (unconfirmed) | B4 | todo |

## Edits, diffs, checkpoints

| Behavior | Extension | vszed mapping | Status |
|---|---|---|---|
| Per-change "Accept this change" / "Reject this change" in the diff (none when more than 100 changes) | yes | `AgentDiffPane` hunk keep/reject | todo |
| "Accept/Reject Change at Cursor" commands | yes | actions | todo |
| Hover a message for rewind: "Fork conversation from here", "Rewind code to here", "Fork conversation and rewind code" | yes | `AcpThread::rewind` + `ActionLog`; fork needs adapter support | todo / gap |

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

Account & usage dialog and header, Web sessions, prompt-cache clock, Remote Control, Claude in Chrome (`@browser`), dictation, Focus view, terminal mode (`useTerminal`).
