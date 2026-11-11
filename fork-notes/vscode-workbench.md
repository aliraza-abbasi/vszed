# VS Code workbench: upstream hooks

All workbench code lives in `crates/vscode_workbench`. These are the only edits to
upstream files. Check them first when a rebase conflicts.

| File | Purpose | Lines |
|---|---|---|
| `README.md` | Review banner required by `.rules` (removed by hand before any PR) | +2 |
| `crates/workspace/src/workspace.rs` | `leading_item` field, `set_leading_item` / `leading_item`, rendered left of `div#workspace`. The status bar and toast layer move up one level so the slot shares a row with `div#workspace` (its bounds still drive dock resizing) | +19 -6 |
| `crates/workspace/src/dock.rs` | `Dock::panels()` iterator | +5 |
| `crates/workspace/src/dock.rs` | `header: Option<AnyView>` field, `set_header` / `header`. While set, the panel wrapper in `Dock::render` becomes a flex column with the header above the active panel. Unset, the render tree is unchanged | +15 |
| `crates/settings_content/src/workbench.rs` | New file: `WorkbenchSettingsContent` (`activity_bar`, `dock_headers`), `ActivityBarSettingsContent` | +27 (new) |
| `crates/settings_content/src/settings_content.rs` | `mod workbench`, re-export, `workbench` field, `flattened_deserialize!` entry | +6 -1 |
| `crates/settings/src/vscode_import.rs` | `workbench: None` in the exhaustive `SettingsContent` literal | +1 |
| `Cargo.toml` | Workspace member and dependency | +2 |
| `crates/zed/Cargo.toml` | Dependency | +1 |
| `crates/zed/src/main.rs` | `vscode_workbench::init(cx)` after `collab_ui::init` | +1 |

`Cargo.lock` also changes; regenerate it instead of resolving conflicts by hand.

## Settings

```json
"workbench": {
  "activity_bar": { "visible": true },
  "dock_headers": true
}
```

Defaults come from `unwrap_or_default()` in `WorkbenchSettings::from_settings`, so
`assets/settings/default.json` is untouched.

## Behavior notes

- The activity bar switches only the left dock (VS Code's primary side bar), in
  VS Code's fixed order: Explorer, Search, Source Control, Run and Debug,
  Extensions, then other left dock panels by activation priority. Right and bottom
  dock panels are reached through the dock headers.
- Dock headers are hidden while their dock's panel is zoomed, because a zoomed
  panel is rendered by the workspace outside of its dock.
- `workbench: use VS Code layout` remembers the user's previous values in the KV
  store under `vscode_workbench_previous_layout`. Applying it again keeps that
  snapshot; `workbench: restore previous layout` writes it back and deletes it.
- The preset installs the `vscode-classic-themes` (themes `Dark 2026 (VS Code)` /
  `Light 2026 (VS Code)`) and `seti-icons` (`Seti Icon Theme`) extensions from the
  Zed extension registry and selects each one once it is installed.
