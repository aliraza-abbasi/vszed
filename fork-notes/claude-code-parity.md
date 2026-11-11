# Claude Code VS Code extension parity checklist

Target: the `claude_code` agent UI style in vszed must match the Claude Code VS Code extension in look and behavior. Sources are the official docs (fetched 2026-11-11) plus the user's reference screenshot of the current extension. Where they disagree, the screenshot (the version the user runs) wins. Re-check exact strings against the raw pages before hard-coding them:
- https://code.claude.com/docs/en/vs-code.md
- https://code.claude.com/docs/en/permission-modes.md
- https://code.claude.com/docs/en/checkpointing.md
- https://code.claude.com/docs/en/sessions.md

Status per row: `todo` / `done` / `gap` (not exposed through ACP or the claude-agent-acp adapter; record it, do not fake it).

## Placement and entry points

| Behavior | Extension | vszed mapping | Status |
|---|---|---|---|
| Spark icon in the activity bar opens the sessions list | yes | Session Manager panel in the left dock (B6), surfaced by the Track A activity bar | todo |
| `✻ Claude Code` status bar item, bottom right | yes | status item (B6) | todo |
| Spark icon in the editor toolbar, top right, when a file is open | yes | pane tab-bar button | todo |
| Default location: editor tab (`preferredLocation: panel`), or right sidebar | yes | `agent.thread_location: editor` (default for this style) / `panel` | todo |
| Multiple conversations as separate tabs | yes | one `AgentThreadItem` per session (B5) | todo |
| Tab status dot: blue = permission pending, orange = finished while hidden | yes | tab icon indicator (B5) | todo |
| Clicking a session that is already open focuses its tab | yes | B5/B6 | todo |

## Sessions

| Behavior | Extension | vszed mapping | Status |
|---|---|---|---|
| Session list scoped to the current project (user requirement; matches `claude --resume`) | per screenshot and user | ACP `session/list` with `cwd` = each worktree root, filtered by `work_dirs` (B6) | todo |
| Search by keyword, browse by time | yes | B6 | todo |
| AI-generated titles from the first message | yes | use the session title from ACP | todo |
| Hover actions: Rename, Archive; "Archived sessions" group with "Unarchive session" | yes | B6 (archive state in `ThreadMetadataStore`) | todo |
| Auto-archive inactive sessions after 14 days (setting, 0 = off) | yes | setting + B6 | todo |
| Sessions started in the terminal CLI appear and resume | yes | comes from ACP `session/list` | todo |
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
| Paste images; Shift+drag files in | yes | `MessageEditor` (images supported) | todo |
| `/` button and typed `/` open the slash menu (Switch model, Output styles, Hooks, Permissions, Memory, Status, Thinking, ...) | yes | ACP `available_commands` + local items | todo |
| Model name button at the bottom; shows effort level, e.g. "Opus 5.5 Extra high" | yes | `ConfigOptionsView` Model + ThoughtLevel | todo |
| Thinking switch in the model menu | yes | ThoughtLevel config option | todo |
| Permission mode indicator labels: **Manual**, **Edit automatically**, **Plan**, **Auto**, **Bypass permissions** (Bypass only when allowed) | yes | ACP session modes / Mode config option; use these labels | todo |
| Mode cycle shortcut | not documented for VS Code (Shift+Tab is CLI only) | click only, unless the screenshot version shows otherwise | todo |
| Context usage indicator; `/compact` | yes | `thread.token_usage()` | todo |
| Clock chip with prompt-cache time left (e.g. "60m"; red when expired) | yes (the "60m" chip in the screenshot) | | gap? (check adapter `_meta`) |
| "N agents" indicator, opens the agent map | yes | subagent list from `AcpThread` | todo |
| Stop button and Esc end the turn | yes | `cancel_generation` | todo |
| Sending while busy queues the message: "Message queued" | yes | existing message queue | todo |
| Placeholder text | not documented | screenshot: "Queue another message..." while busy | todo |

## Conversation

| Behavior | Extension | vszed mapping | Status |
|---|---|---|---|
| Timestamps on messages (`showMessageTimestamps`, default on) | yes | B2 | todo |
| Scroll to bottom on send (`scrollToBottomOnSend`) | yes | B2 | todo |
| Timeline: dot per step, bold tool name + description, IN/OUT blocks, "Thought for Ns" | screenshot | B2 | todo |
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
| Focus input / toggle between editor and Claude | Cmd+Esc / Ctrl+Esc | todo |
| Open in new tab | Cmd+Shift+Esc / Ctrl+Shift+Esc | todo |
| New conversation (opt-in setting) | Cmd+N / Ctrl+N | todo |
| Reopen closed session (default on) | Cmd+Shift+T / Ctrl+Shift+T | todo (conflicts with Zed reopen closed item: scope to the Claude tab context) |
| Insert @-mention of the selection | Option+K / Alt+K | todo |
| Stop current turn | Esc | todo |

## Extension settings to mirror (under `agent.claude_code.*` or similar)

`preferredLocation`, `initialPermissionMode`, `attachSelection`, `attachOpenFile`, `useCtrlEnterToSend`, `scrollToBottomOnSend`, `showMessageTimestamps`, `enableNewConversationShortcut`, `enableReopenClosedSessionShortcut`, `archiveInactiveSessions`, `allowDangerouslySkipPermissions`, `spinnerVerbs`. Shared Claude settings (`~/.claude/settings.json`: `modelSettings`, `showThinkingSummaries`, `permissions.defaultMode`) are read by Claude itself, not by vszed.

## Out of scope / gaps to confirm with the adapter

Account & usage dialog and header, Web sessions, prompt-cache clock, Remote Control, Claude in Chrome (`@browser`), dictation, Focus view, terminal mode (`useTerminal`).
