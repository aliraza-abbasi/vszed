# VS Code workbench: upstream hooks

All workbench code lives in `crates/vscode_workbench`. These are the only edits to
upstream files. Check them first when a rebase conflicts.

| File | Purpose | Lines |
|---|---|---|
| `README.md` | Review banner required by `.rules` (removed by hand before any PR) | +2 |
| `crates/workspace/src/workspace.rs` | `leading_item` field, `set_leading_item` / `leading_item`, rendered left of `div#workspace`. The status bar and toast layer move up one level so the slot shares a row with `div#workspace` (its bounds still drive dock resizing) | +19 -6 |
| `crates/workspace/src/dock.rs` | `Dock::panels()` iterator | +5 |
| `crates/workspace/src/dock.rs` | `header: Option<AnyView>` field, `set_header` / `header`. While set, the panel wrapper in `Dock::render` becomes a flex column with the header above the active panel. Unset, the render tree is unchanged | +15 |
| `crates/workspace/src/notifications.rs` | `Workspace::has_notifications()`, for the status bar bell | +4 |
| `crates/title_bar/src/title_bar.rs` | `center_item` / `trailing_item` fields with setters and getters, plus a `workspace()` getter so `observe_new::<TitleBar>` can reach the workspace. The center item is pushed after the left items inside an absolutely positioned, full width `h_flex` that centers it over the title bar; the trailing item is the first child of the right side group. The children `ArrayVec` grows from 5 to 6. Unset, the render tree is unchanged | +41 -1 |
| `crates/settings_content/src/workbench.rs` | New file: `WorkbenchSettingsContent` (`activity_bar`, `dock_headers`, `command_center`, `layout_controls`, `status_bar`), `ActivityBarSettingsContent` | +43 (new) |
| `crates/settings_content/src/settings_content.rs` | `mod workbench`, re-export, `workbench` field, `flattened_deserialize!` entry | +6 -1 |
| `crates/settings/src/vscode_import.rs` | `workbench: None` in the exhaustive `SettingsContent` literal | +1 |
| `Cargo.toml` | Workspace member and dependency | +2 |
| `crates/zed/Cargo.toml` | Dependency | +1 |
| `crates/zed/src/main.rs` | `vscode_workbench::init(cx)` after `collab_ui::init` | +1 |

`Cargo.lock` also changes; regenerate it instead of resolving conflicts by hand.

New assets live in `assets/icons/codicons/` (embedded by the existing
`icons/**/*` glob), so they never conflict.

## Settings

```json
"workbench": {
  "activity_bar": { "visible": true },
  "dock_headers": true,
  "command_center": true,
  "layout_controls": true,
  "status_bar": true
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
- The preset also turns on `workbench.command_center`, `layout_controls` and
  `status_bar`, and turns off `title_bar.show_project_items` / `show_branch_name`
  (VS Code shows neither in its title bar). All of them are in the snapshot.

### Title bar

- `command_center` and `layout_controls` attach to every `TitleBar` through
  `cx.observe_new::<TitleBar>` and follow the settings live. The title bar is
  rebuilt when `title_bar.show` or full screen changes; the new one gets the items
  again.
- The command center's back and forward buttons use the active pane's history
  (`Workspace::go_back` / `go_forward`), because Zed's navigation history is per
  pane rather than global like VS Code's. They are disabled when that pane has no
  history.
- The search box is 38% of the title bar wide (at most 600px) and opens
  `file_finder::Toggle`. Its chevron menu has Go to File, Show and Run Commands,
  Go to Symbol in Workspace and Go to Line/Column; the menu dispatches from the
  active editor so Go to Line reaches it.
- Layout controls: Customize Layout (Visibility: Activity Bar, Primary Side Bar,
  Secondary Side Bar, Panel; Bottom Panel Alignment: Contained, Full, Left
  Aligned, Right Aligned; Use VS Code Layout; Restore Previous Layout, disabled
  without a snapshot), then Toggle Primary Side Bar, Toggle Panel and Toggle
  Secondary Side Bar. The toggles call `Workspace::toggle_dock` directly, so they
  work while the activity bar hides the status bar's dock buttons. Their icons
  switch to the `-off` codicons while the dock is closed.
- New action: `workbench: toggle activity bar`.

### Status bar

- `workbench.status_bar` only reorders and adds items through the `StatusBar` API.
  Moved and hidden items leave a placeholder in their slot (the same mechanism the
  activity bar uses for the dock buttons), so turning the setting off puts every
  item back where it was.
- Left: remote indicator, branch with sync, problems, right after the left dock
  buttons. Zed's own diagnostics indicator is hidden while the setting is on (it
  shows a check mark at zero and the message under the cursor, which VS Code
  doesn't).
- The remote indicator is always shown. Local projects get Open Remote Project…
  and Open Dev Container… (and Connect to WSL… on Windows); remote projects also
  show `SSH: host` / `WSL: distro` / `Dev Container: name`.
- Sync: `cloud-upload` publishes a branch without upstream (`git::Push`).
  Otherwise the sync icon shows `behind↓ ahead↑` and pulls when behind, pushes
  when ahead, and fetches otherwise. Zed has no combined pull and push action.
- Right, drawn left to right: cursor position, encoding, line ending, language,
  toolchain, edit prediction, bell. Zed has no indentation item.
- The status bar can't insert an item at its very end, so the bell goes right
  before the right and bottom dock buttons (which the activity bar hides). Finding
  that slot needs the number of left items, which the status bar doesn't expose; a
  temporary probe item appended to each side reveals it.
- Zed has no notification center. The bell shows `bell-dot` while notifications
  are shown and clears them all on click (`Workspace::clear_all_notifications`).

## Codicons

`assets/icons/codicons/*.svg` are unmodified icons from
[microsoft/vscode-codicons](https://github.com/microsoft/vscode-codicons)
(commit `6200d186bea39354a18737cfff73fe19dbc5fcff`), © Microsoft Corporation,
licensed under CC BY 4.0. The license text is in `assets/icons/codicons/LICENSE`
and a short attribution in `assets/icons/codicons/README.md`. Only the icons
listed in the `Codicon` enum (`crates/vscode_workbench/src/vscode_workbench.rs`)
are bundled; a test fails if the enum and the directory disagree. They are drawn
with `Icon::from_path`, except in the activity bar, which draws the same SVGs
with `svg()` to tint them on hover. All of them use `fill="currentColor"`, so Zed
renders them monochrome in the requested color.

`script/generate-licenses` only collects `assets/icons/LICENSES`, so the codicons
license isn't in the generated licenses file yet.
