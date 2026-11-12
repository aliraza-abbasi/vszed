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
| `crates/acp_thread/src/terminal.rs` (display terminals) | bug fix, not a style hook: agent display terminal output with bare LF line endings (claude-agent-acp over ACP v2) gets CRLF before it reaches the emulator, so lines no longer "staircase". Upstream candidate on its own | +45 |

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
- Account & usage: `ui_styles/claude_code/usage.rs`, drawn at the top of the sessions panel
  (the panel's sections are ACCOUNT & USAGE and SESSION MANAGER). `usage::init` observes every
  `ThreadView`, reads `AcpThread::usage_meta()["_claude/rateLimit"]` on `TokenUsageUpdated`, and
  keeps one reading per limit type per open thread view in a `ClaudeUsageStore` global (dropped
  when the view is released). The panel shows the newest reading of each type across its
  project's thread views. Nothing reads `~/.claude.json`, credentials or the Keychain.
