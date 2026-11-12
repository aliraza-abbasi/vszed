# Agent UI styles: upstream hooks

Track B (`feat/agent-ui-styles`). All new code lives in `crates/agent_ui/src/ui_styles/`
and `crates/settings_content/src/agent/ui_style.rs`. These are the only edits to
upstream files. Use this list when resolving rebase conflicts and when folding in.

| File | Purpose | Approx. lines |
|---|---|---|
| `README.md` | `.rules` review banner (first two lines). Removing it is a manual step for the human author. | +2 |
| `crates/settings_content/src/agent.rs` (~15, ~378) | `mod ui_style; pub use ui_style::*;` and the `ui_style` / `thread_location` fields on `AgentSettingsContent` | +11 |
| `crates/agent_ui/src/agent_ui.rs` (~39, ~714) | `mod ui_styles;` and `ui_styles::init(cx)` after `agent_panel::init` | +2 |
| `crates/agent_ui/src/conversation_view/thread_view.rs` (~64) | `#[path = "../ui_styles/claude_code/thread.rs"] mod claude_code_thread;` so the renderer can use `ThreadView` internals | +3 |
| `crates/agent_ui/src/conversation_view/thread_view.rs` (`render_entries`, ~7411) | early return to `render_claude_code_entries` when `agent.ui_style` is `claude_code` | +3 |
| `crates/agent_ui/src/conversation_view/thread_view.rs` (`render_message_editor`, ~5570) | early return to `render_claude_code_message_editor` (after the subagent case) | +3 |
| `crates/agent_ui/src/conversation_view/thread_view.rs` (`activity_bar_bg` ~3997, `render_activity_bar` ~4009) | `activity_bar_bg` returns the editor background in the Claude Code style, and `render_activity_bar` returns early to `render_claude_code_activity_bar` (same rows, drawn like the prompt box) | +6 |
| `crates/agent_ui/src/message_editor.rs` (`MessageEditor::render`, ~2094) | `let text_style = crate::ui_styles::message_editor_text_style(text_style, cx);`, which swaps in the UI font family and size in the Claude Code style (prompt box and editable sent messages) | +1 |
| `crates/agent_ui/src/config_options.rs` (~33, ~1016) | `pub(crate)` on `ConfigOptionsView::{config_options, agent_server, fs}` and `set_config_option_and_default`, so the model and mode chips can read and set the adapter's options | 4 changed |
| `crates/agent_ui/src/agent_panel.rs` (imports, `NewThread` handler ~366, `open_thread` ~1680, `new_thread` ~1770, `load_agent_thread` ~4530) | route thread opens to editor tabs when `agent.thread_location` is `editor` (`ui_styles::thread_item::route_to_thread_tab`); `load_agent_thread` only routes user-initiated opens (`focus`), so the panel's startup restore stays in the panel | +20 |
| `assets/keymaps/default-{macos,linux,windows}.json` (end) | `Workspace` block: `cmd-escape` / `ctrl-escape` → `agent::ToggleThreadTabFocus`, `cmd-shift-escape` / `ctrl-shift-escape` → `agent::NewThreadTab`. Both actions call `cx.propagate()` unless thread tabs or the Claude Code style are on, so the stock `menu::Cancel` binding still gets the keystroke | +10 each |
| `crates/acp_thread/src/acp_thread.rs` (`usage_meta` field, `update_usage` ~6299, `usage_meta()` getter ~4989) | generic hook, no Claude parsing: keeps the latest value of each `_meta` key from `usage_update` (ACP v1 and v2), merged per key because claude-agent-acp sends `_claude/rateLimit` only when it changes but `_claude/model` on every update. Read by the Account & usage section. Own commit (`feat(acp_thread): keep the latest usage update _meta on the thread`); upstream candidate | +15 (+58 test) |
| `crates/agent_ui/src/conversation_view/thread_view.rs` (`render_activity_bar` ~4011, `render_message_editor` ~5575, `ThreadView::render` ~14343) | floating prompt box: in the Claude Code style `render_activity_bar` returns `None` and `render_message_editor` returns `Empty` (after the subagent case), and `render` passes the conversation through `render_ui_style_conversation` (`prompt_box.rs`), which lays the transcript over the whole area and floats the activity bar and prompt box over its bottom; Zed's style gets the conversation back unchanged | 5 changed |
| `crates/agent_servers/src/acp.rs` (after `ClientContext`) | `AgentExtNotification` and the global `AgentExtNotifications` entity, which rebroadcasts agents' `_`-prefixed extension notifications | +29 |
| `crates/agent_servers/src/acp/v1.rs` (`client_builder`, `new_session`, `load_session`, `resume_session`, tests) | an untyped notification handler (last in the chain, `Handled::No` for non-`_` methods) that forwards extension notifications to `AgentExtNotifications`; `claude_session_meta` adds `_meta.claudeCode.emitRawSDKMessages` (task lifecycle + `result`) to new, load and resume requests of `claude-acp` sessions | +100 |
| `crates/agent_servers/src/agent_servers.rs` | re-export `AgentExtNotification`, `AgentExtNotifications` | 2 changed |
| `crates/acp_thread/src/terminal.rs` (display terminals) | bug fix, not a style hook: agent display terminal output with bare LF line endings (claude-agent-acp over ACP v2) gets CRLF before it reaches the emulator, so lines no longer "staircase". Upstream candidate on its own | +45 |
| `crates/agent_servers/src/acp/v1.rs` (test `v2_terminal_sdk_snapshots_chunks_and_metadata`, ~4852) | test expectation follows the line-ending fix above: the snapshot's last chunk `"\nZ"` now reads `"new\n¢\nZ"` instead of `"new\n¢\n Z"`. Own commit (`test(agent_servers): expect normalized line endings in the display terminal snapshot`); goes with the terminal fix if that is upstreamed | 2 changed, +1 comment |

Notes:
- The resolved settings are `ui_styles::AgentUiStyleSettings` (a separate `RegisterSetting`),
  not fields on `AgentSettings`, so no exhaustive `AgentSettings { .. }` literal needs to change
  and `assets/settings/default.json` is untouched. On fold-in, move them into `AgentSettings`
  and add `default.json` and Settings UI entries.
- The style is swapped at the conversation list, the activity bar and the message editor
  area. The `ThreadView` root (action handlers, callouts) is shared by both styles.
- `ui_styles/claude_code/prompt_box.rs` and `ui_styles/claude_code/permission_card.rs` are
  declared from `thread.rs` with a nested `#[path]`, so they are also descendants of
  `thread_view` without another hook line.
- `render_claude_code_activity_bar` (in `prompt_box.rs`) repeats the composition of upstream
  `render_activity_bar` (permission rows, plan, edited files, queue) with the prompt box's
  container. When upstream changes `render_activity_bar`, port the change. It leaves out the
  permission rows while the permission card is up.
- Permission card (B4): a pending request whose options are a flat list (every ACP v2 request,
  and ACP v1 `PermissionOptions::Flat`, which is what claude-agent-acp sends) replaces the prompt
  box with the card, and the tool row drops Zed's inline buttons for it. Zed's own agent
  (dropdown choices with patterns) keeps Zed's buttons. Answers go through the same paths as
  Zed's buttons: `ThreadView::authorize_permission_request` (v1) and
  `ConversationView::select_permission_option` / `cancel_permission_request` (v2). The card's
  focus handle and highlighted option live in `ThreadActivity`.
- Preset: `ui_styles/preset.rs` registers `agent::UseClaudeCodeStyle` and
  `agent::RestorePreviousUiStyle` on every workspace. The user's previous `agent.ui_style` and
  `agent.thread_location` are kept in the KV store under `agent_ui_style_previous_settings`
  until restored.
- Per-thread timeline state (user message send times, thought durations) is kept in a
  `ClaudeCodeActivity` global keyed by `ThreadView` entity id, populated through
  `cx.observe_new::<ThreadView>` in `ui_styles::claude_code::init`. On fold-in this becomes a
  `ThreadView` field.

- Editor tabs: `ui_styles/thread_item.rs` (`AgentThreadItem`, any agent). Tabs share the agent
  panel's `AgentConnectionStore`; tabs restored before the panel loads use a per-project
  fallback store. Saved tab state lives in the scoped KV store (`agent_thread_items`,
  key `<workspace id>:<item id>`); entries of closed tabs are not deleted (the workspace never
  reads them back).
- Sessions panel: `ui_styles/claude_code/sessions_panel.rs`. Added to and removed from each
  workspace by a settings observer, so stock Zed never sees it. Rename and archive create a Zed
  `ThreadMetadataStore` row for the session when there is none yet (the same row opening the
  session would create), so those sessions then also appear in Zed's own thread history.
- Account & usage: `ui_styles/claude_code/usage.rs`, drawn at the top of the sessions panel. Two sources:
  - (1) Live `AcpThread::usage_meta()["_claude/rateLimit"]`, read on `TokenUsageUpdated` and stored per thread view with its arrival time (wall-clock ms) in the `ClaudeUsageStore` global. When a view is released, its readings move to a per-project map kept in memory, so the last known limits survive closing every Claude tab.
  - (2) Claude Code's own account file, `$CLAUDE_CONFIG_DIR/.claude.json` or `~/.claude.json`. Decision: this is an undocumented Claude Code file that may change, so parsing is lenient (unknown keys are skipped unread, a malformed field drops only that field) and fails closed to the empty state.
  - Fields read, and nothing else: `oauthAccount.emailAddress`; `cachedUsageUtilization.fetchedAtMs`; `cachedUsageUtilization.utilization.limits[].{kind, percent, severity, resets_at, scope.model.display_name, scope.surface.display_name}`.
  - The file is read and parsed on the background executor through the project's `Fs`, and watched with `Fs::watch` (500 ms latency plus a 500 ms debounce) while any sessions panel is open (one shared `Rc` task). Its contents and values are never logged or put in errors; a parse failure logs a fixed debug line.
  - Rows: the cache's rows in the server's order, each replaced by a live reading of the same window that arrived after `fetchedAtMs`, then live-only windows. Credentials and the Keychain are never read.
- Session manager: a plain "+ New session" row, a Local / Web toggle (Web disabled), and a toolbar with the status filter (funnel), Active toggle, search icon, New group and refresh. Filters persist in the KV store under `claude_code_session_filters`. Session groups are saved in the scoped KV namespace `claude_code_session_groups`, keyed by the project's sorted root paths, as the extension saves them per workspace folder. The Active filter's "unread" state reads `AgentThreadItem::finished_while_hidden()` (`ui_styles/thread_item.rs`).
- Timeline state in `ThreadActivity` (`ui_styles/claude_code.rs`), all keyed by entry index and shifted on `EntriesRemoved`: thought timings (a thought ends when the next step arrives), one open/closed flag for every thought summary of the thread (the extension toggles them all at once), the wall-clock time each user message and each run of assistant text first appeared (for the timestamps and date lines), and which long user messages overflow their 60px box (measured with `on_children_prepainted`) or were expanded with "Show more". Tool output tabs (`ToolOutputTab`: weak editor and buffer plus the thread and terminal subscriptions that stream into it) are kept there by tool call id.
- Tool output tabs: a read-only buffer with no file, titled "<Tool> tool output" (the extension's title), added to the pane right of the active pane, or to a new split there. It follows the output (appends when the old text is a prefix, else replaces) on the tool call's `EntryUpdated` and on its terminals' notifications; display terminals grow without touching the entry.
- Transcript text size: assistant markdown, thought summaries and compaction summaries use `transcript_markdown_style`, the agent markdown style scaled by 14/16 (the size Zed's labels reach), so the transcript matches VS Code's 13px workbench text when `ui_font_size` is 15. `ui_font_text_style` (prompt and sent messages) uses the same ratio.
- Floating prompt box: `render_ui_style_conversation` (`prompt_box.rs`) overlays a 150px fade
  (transparent to the editor background, full width) and the activity bar plus prompt box
  (centered, at most 680px, opaque) on the transcript. The overlay's height is measured with a
  `canvas` into `ThreadActivity::floating_prompt_height` and used as the transcript list's bottom
  padding (`render_claude_code_entries`), so the last entry scrolls clear of the box. The prompt
  editor is switched to `AutoHeight { min_lines: 1, max_lines: 9 }` while the style is on, and
  `ThreadView::sync_editor_mode` restores Zed's mode when it is switched off.
- Model picker (`ui_styles/claude_code/model_picker.rs`): its own view, opened above the prompt
  box (kept in `ThreadActivity::model_picker` as an `AnyView`), not a `PopoverMenu`, so it can
  take the box's width like the extension's. `ConfigOptionsHandle` moved there.
- Agents and background tasks: ACP v2 sessions (the `acp-beta` path, which debug and staff
  builds use) read subagents from `AcpThread::subagents()` (nested through each child's own
  subagents) and background work from `AcpThread::background_work()`; claude-agent-acp 0.106.0
  ignores `emitRawSDKMessages` over v2. ACP v1 sessions read Claude Code's `task_*` and
  `background_tasks_changed` system messages from `_claude/sdkMessage`, through
  `AgentExtNotifications`. Both feed `agent_summaries` / `background_task_summaries` in
  `claude_code.rs`; subagent run times over v2 are measured locally (announcement to last
  activity). The agent map is a workspace modal (`ui_styles/claude_code/agent_map.rs`).
- Prompt cache clock: `PromptCache` in `ThreadActivity`, anchored at the latest model request
  (a new assistant or tool entry while a turn runs), reset to "compacted" by a finished
  compaction. The lifetime comes from v1 `result` usage (`cache_creation` split), else one hour.
- Status bar item: `ClaudeCodeStatusItem` (`claude_code.rs`), added to and removed from every
  workspace's status bar by a settings observer, like the sessions panel. Clicking it runs
  `thread_item::focus_or_open_thread_tab`.
