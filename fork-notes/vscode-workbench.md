# VS Code workbench: upstream hooks

All workbench code lives in `crates/vscode_workbench`. These are the only edits to
upstream files. Check them first when a rebase conflicts.

| File | Purpose | Lines |
|---|---|---|
| `README.md` | Review banner required by `.rules` (removed by hand before any PR) | +2 |
| `crates/workspace/src/workspace.rs` | `leading_item` field, `set_leading_item` / `leading_item`, rendered left of `div#workspace`. The status bar and toast layer move up one level so the slot shares a row with `div#workspace` (its bounds still drive dock resizing) | +19 -6 |
| `crates/workspace/src/dock.rs` | `Dock::panels()` iterator | +5 |
| `crates/settings_content/src/workbench.rs` | New file: `WorkbenchSettingsContent`, `ActivityBarSettingsContent` | +21 (new) |
| `crates/settings_content/src/settings_content.rs` | `mod workbench`, re-export, `workbench` field, `flattened_deserialize!` entry | +6 -1 |
| `crates/settings/src/vscode_import.rs` | `workbench: None` in the exhaustive `SettingsContent` literal | +1 |
| `Cargo.toml` | Workspace member and dependency | +2 |
| `crates/zed/Cargo.toml` | Dependency | +1 |
| `crates/zed/src/main.rs` | `vscode_workbench::init(cx)` after `collab_ui::init` | +1 |

`Cargo.lock` also changes; regenerate it instead of resolving conflicts by hand.

## Settings

`"workbench": { "activity_bar": { "visible": true } }`. Defaults come from
`unwrap_or_default()` in `WorkbenchSettings::from_settings`, so
`assets/settings/default.json` is untouched.
