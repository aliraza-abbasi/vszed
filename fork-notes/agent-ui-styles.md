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
| `crates/agent_ui/src/config_options.rs` (~33, ~1016) | `pub(crate)` on `ConfigOptionsView::{config_options, agent_server, fs}` and `set_config_option_and_default`, so the model and mode chips can read and set the adapter's options | 4 changed |
| `crates/agent_ui/src/agent_panel.rs` (imports, `NewThread` handler ~366, `open_thread` ~1680, `new_thread` ~1770, `load_agent_thread` ~4530) | route thread opens to editor tabs when `agent.thread_location` is `editor` (`ui_styles::thread_item::route_to_thread_tab`); `load_agent_thread` only routes user-initiated opens (`focus`), so the panel's startup restore stays in the panel | +20 |
| `assets/keymaps/default-{macos,linux,windows}.json` (end) | `Workspace` block: `cmd-escape` / `ctrl-escape` → `agent::ToggleThreadTabFocus`, `cmd-shift-escape` / `ctrl-shift-escape` → `agent::NewThreadTab`. Both actions call `cx.propagate()` unless thread tabs or the Claude Code style are on, so the stock `menu::Cancel` binding still gets the keystroke | +10 each |
| `crates/acp_thread/src/terminal.rs` (display terminals) | bug fix, not a style hook: agent display terminal output with bare LF line endings (claude-agent-acp over ACP v2) gets CRLF before it reaches the emulator, so lines no longer "staircase". Upstream candidate on its own | +45 |

Notes:
- The resolved settings are `ui_styles::AgentUiStyleSettings` (a separate `RegisterSetting`),
  not fields on `AgentSettings`, so no exhaustive `AgentSettings { .. }` literal needs to change
  and `assets/settings/default.json` is untouched. On fold-in, move them into `AgentSettings`
  and add `default.json` and Settings UI entries.
- The style is swapped at the conversation list and the message editor area. The `ThreadView`
  root (action handlers, callouts, queue, activity bar) is shared by both styles.
- `ui_styles/claude_code/prompt_box.rs` is declared from `thread.rs` with a nested `#[path]`,
  so it is also a descendant of `thread_view` without another hook line.
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
