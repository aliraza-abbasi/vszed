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
| `crates/workspace/src/status_bar.rs` | `StatusBar::add_rightmost_item`, which inserts at the front of `right_items` (drawn last, because right items are drawn in reverse). `insert_item_after` can never reach that slot | +18 |
| `crates/workspace/src/status_bar.rs` | `show_sidebar_toggle` field (default `true`) with `set_show_sidebar_toggle` / `show_sidebar_toggle`. `render` ANDs it into `SidebarStatus::show_toggle`, so the threads sidebar toggle is drawn exactly as before unless a caller turns it off | +14 -1 |
| `crates/title_bar/src/title_bar.rs` | `center_item` / `trailing_item` fields with setters and getters, plus a `workspace()` getter so `observe_new::<TitleBar>` can reach the workspace. The center item is pushed after the left items inside an absolutely positioned, full width `h_flex` that centers it over the title bar; the trailing item is the first child of the right side group. The children `ArrayVec` grows from 5 to 6. Unset, the render tree is unchanged | +41 -1 |
| `crates/settings_content/src/editor.rs` | `MinimapContent::line_height: Option<BufferLineHeight>` (separate `feat(editor)` commit, meant for upstream) | +7 -1 |
| `crates/editor/src/editor_settings.rs` | `Minimap::line_height`, read from the content | +4 |
| `crates/editor/src/editor.rs` | `update_minimap_configuration` puts `minimap.line_height` into the minimap editor's text style refinement. It runs when the minimap is created and on settings changes | +5 |
| `crates/editor/src/element.rs` | `get_minimap_line_height` takes the minimap editor's `TextStyleRefinement` instead of only its font size, so the layout uses the refined line height too; plus `test_minimap_line_height_setting` | +9 -12, test +53 -1 |
| `assets/settings/default.json` | `"minimap": { "line_height": null }` with a comment | +4 |
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
- The preset also sets, with restore, VS Code's editor status and minimap
  defaults: `status_bar.active_encoding_button: "enabled"`,
  `status_bar.line_endings_button: true`, `minimap.thumb: "hover"` (VS Code's
  `showSlider: "mouseover"`), `minimap.max_width_columns: 120` (VS Code's
  `maxColumn`) and `minimap.line_height: { "custom": 1.0 }` (see Minimap).
- It also sets, with restore, `minimap.display_in: "all_editors"` (VS Code draws
  a minimap in every editor group) and VS Code's editor line height,
  `buffer_line_height: { "custom": 1.5 }` on macOS and `1.35` elsewhere
  (`editor.lineHeight` 0 means the font size times `GOLDEN_LINE_HEIGHT_RATIO`
  in `src/vs/editor/common/config/fontInfo.ts`). VS Code rounds the result to
  whole pixels (14px times 1.35 is 19px); Zed doesn't (18.9px). The settings
  writer converts the `f32` through `f64`, so on Linux and Windows the file
  reads `1.350000023841858`, which parses back to 1.35.
- The preset also turns on `workbench.command_center`, `layout_controls` and
  `status_bar`, and turns off `title_bar.show_project_items` / `show_branch_name`
  (VS Code shows neither in its title bar). All of them are in the snapshot.
- It also turns off, with restore:
  - `title_bar.show_sign_in` / `show_user_menu`: VS Code keeps accounts in the
    activity bar, whose Accounts and Manage menus cover the user menu (see
    below). `title_bar.show_user_picture` is left alone; it only changes the
    user menu's trigger, which is hidden.
  - `search.button`: Search is in the activity bar. This is a preset setting
    rather than a `workbench.status_bar` item removal because the button is
    still useful with the activity bar off.
  - `tab_bar.show_nav_history_buttons`: the command center has back and
    forward buttons. The other tab bar buttons stay.
  - `global_lsp_settings.button`: Zed's language server button on the left of
    the status bar. The workbench's language status item (`{ }`) stands in for
    it. Zed still draws the button while the project has untrusted worktrees,
    as a restricted mode warning.
- A snapshot taken before a key was added to the preset doesn't have that key,
  so restoring removes it from the settings file.

### Title bar

- `command_center` and `layout_controls` attach to every `TitleBar` through
  `cx.observe_new::<TitleBar>` and follow the settings live. The title bar is
  rebuilt when `title_bar.show` or full screen changes; the new one gets the items
  again.
- The command center's back and forward buttons use the active pane's history
  (`Workspace::go_back` / `go_forward`), because Zed's navigation history is per
  pane rather than global like VS Code's. They are disabled when that pane has no
  history.
- The search box is 38% of the title bar wide (at most 600px). Like plain VS
  Code's, it has no dropdown: a click anywhere on it opens `file_finder::Toggle`.
- Layout controls: Customize Layout (Visibility: Activity Bar, Primary Side Bar,
  Secondary Side Bar, Panel, and Threads Sidebar while AI is enabled; Bottom
  Panel Alignment: Contained, Full, Left Aligned, Right Aligned; Use VS Code
  Layout; Restore Previous Layout, disabled without a snapshot), then Toggle
  Primary Side Bar, Toggle Panel and Toggle
  Secondary Side Bar. The toggles call `Workspace::toggle_dock` directly, so they
  work while the activity bar hides the status bar's dock buttons. Their icons
  switch to the `-off` codicons while the dock is closed.
- New action: `workbench: toggle activity bar`.

### Activity bar menus

- Accounts has the account part of Zed's title bar user menu: signed out, Sign
  In (failures are shown in the workspace, like the title bar button) or a
  disabled Signing In… entry; signed in, the username, Manage Account (the
  account page), the organization switcher and Sign Out.
- Manage has the rest: Command Palette, Settings, Keymap, Themes…, Icon
  Themes…, Extensions, Use VS Code Layout, Panel Layout (Classic / Agentic,
  while AI is enabled), and Check for Updates, which becomes Restart to Update
  once an update is downloaded.

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
- Right, drawn left to right: current line blame, cursor position,
  indentation, encoding, line ending, language status, language, toolchain,
  edit prediction, bell. Zed's vim mode, pending keystroke and image info items stay
  left of them.
- Cursor position (`CursorPositionItem`) replaces Zed's item, which leaves a
  placeholder like the diagnostics indicator. It uses VS Code's formats:
  `Ln 4, Col 2`, `Ln 4, Col 2 (12 selected)`, `3 selections` and
  `3 selections (12 characters selected)`. The column counts a tab as the
  columns it takes up and selected CRLF line breaks count as two characters,
  as in VS Code. It honors `status_bar.cursor_position_button` and its Hide
  Button, and a click toggles go to line.
- Indentation (`IndentationItem`) shows `Spaces: N` or `Tab Size: N` from the
  active file's `tab_size` and `hard_tabs`. Zed has no indentation size
  separate from the tab size, so VS Code's `Spaces: 2 (Tab Size: 4)` never
  appears. Its menu has only Convert Indentation to Spaces / to Tabs, which
  convert the whole file like VS Code (Zed's actions convert the selected
  lines, so the item selects everything, converts, and puts the cursors back
  shifted by the indentation change). VS Code's Indent Using Spaces / Tabs,
  Change Tab Display Size, Detect Indentation and Trim Trailing Whitespace are
  left out: Zed has no per-file indentation or trim action.
- Language status (`LanguageStatusItem`) is VS Code's `{ }`: shown while
  language servers serve the active file (the servers that opened the buffer
  locally; on remote projects, servers of the buffer's language), with the
  `loading` codicon spinning while one reports progress. Its menu lists the
  servers, with the progress title when busy, and Restart / Stop Language
  Servers. VS Code fills its hover from extension status items, so the content
  differs. Server health and failed starts aren't shown (the LSP store doesn't
  keep them); Zed's own language server button, which the preset hides, has
  them.
- Current line blame (`CurrentLineBlameItem`) is GitLens's status bar blame:
  the `git-commit` codicon and `Author, 18 months ago` for the newest cursor's
  line, with GitLens's relative dates (`fromNow` in `packages/utils/src/date.ts`:
  one unit only, months up to 728 days, `yesterday` / `last week` / `last
  month` / `last year` for one unit). It reads the editor's own `GitBlame`
  (`blame_for_rows` at the cursor), so it shows next to Zed's inline blame and
  never runs git itself. It also shows on empty lines and ignores the inline
  blame delay, as GitLens does (`Editor::active_git_blame_entry` does
  neither). The tooltip is the commit summary; a click runs
  `editor::OpenGitBlameCommit` (Zed's commit view). It is hidden when the
  line has no blame: no repository, untracked files, uncommitted lines, and
  editors that never turned on inline blame or the blame gutter (with
  `git.inline_blame.enabled: false`, Zed loads no blame).
- Differences from GitLens: no `You` for the user's own commits (Zed's blame
  doesn't know the git user), nothing on uncommitted lines (GitLens shows
  `You, now` and Uncommitted changes), a plain summary tooltip instead of
  GitLens's hover (avatar, date, full message, commands), the commit view
  instead of GitLens's Show Commit quick pick, English only, and no pull
  request suffix.
- Zed's git blame item (`GitBlameStatus`) is hidden while the setting is on,
  leaving a placeholder like the diagnostics indicator. It only has text with
  `git.inline_blame.location: "status_bar"`, where it would repeat the current
  line blame. The preset leaves the location alone, so inline blame stays.
- The bell is added with `StatusBar::add_rightmost_item`, so it is the last
  item even when the right and bottom dock buttons are shown.
- The threads sidebar toggle, which the status bar draws before every left item
  while AI is enabled, is hidden (`set_show_sidebar_toggle(false)`) so the remote
  indicator starts the status bar as in VS Code. No setting hides it: moving the
  sidebar to the right (`agent.threads_sidebar.position`) would put the toggle
  after the bell and move the sidebar too, and `agent.enabled: false` turns off
  AI. The sidebar stays reachable from Customize Layout, its keybinding
  (`multi_workspace: toggle workspace sidebar`) and the agent panel's menu.
- Zed has no notification center. The bell shows `bell-dot` while notifications
  are shown and clears them all on click (`Workspace::clear_all_notifications`).

## Minimap

VS Code (`EditorLayoutInfoComputer._computeMinimapLayout` in
`src/vs/editor/common/config/editorOptions.ts`, defaults `size:
"proportional"`, `scale: 1`, `maxColumn: 120`, `renderCharacters: true`):

- Device pixel scale `minimapScale = round(scale * 2)` at pixel ratio 2 and
  `max(scale, floor(scale * pixelRatio + 0.125))` below it. Line height is
  `2 * minimapScale` device pixels and a character `minimapScale / pixelRatio`
  CSS pixels wide, so 2px by 1px at scale 1 on any display.
- `minimapWidth = min(floor(maxColumn * charWidth), floor((remainingWidth -
  verticalScrollbarWidth - 2) * charWidth / (typicalHalfwidthCharacterWidth +
  charWidth))) + 8`. The minimap shows as many columns as the editor viewport
  (about 12% of the width for 12px Menlo) up to 120 columns plus an 8px gutter.
- The slider is the viewport's lines times the minimap line height.

Zed (`get_minimap_width` / `layout_minimap` in `crates/editor/src/element.rs`):

- The minimap editor uses a 2px `.ZedMono` font (`MINIMAP_FONT_SIZE`), about
  1.2px per column. Its line height is the buffer line height ratio applied to
  2px and rounded, so 3px for both `comfortable` (1.618) and `standard` (1.3).
- `minimap_width = min(text_width * 0.15, 1.2px * max_width_columns)`, hidden
  below 20 columns. Proportional like VS Code, but the default cap of 80
  columns (96px) is reached by editors wider than about 640px, so the minimap
  barely shrinks in a split.
- The thumb is the visible editor lines times the minimap line height, so it
  was 1.5 times VS Code's slider (3px against 2px lines).

The preset uses `max_width_columns: 120` and the new `minimap.line_height:
{ "custom": 1.0 }` (2px lines). What remains different: Zed's columns are
1.2px instead of 1px (the minimap is up to 144px wide instead of 128px), its
width ratio is a fixed 15% instead of depending on the editor font, and it has
no 8px gutter. Matching those would need a minimap font size or scale setting.

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
renders them monochrome in the requested color. `json.svg` is the icon VS Code
names `bracket` (an alias in the codicons mapping), used for the language
status item.

`script/generate-licenses` only collects `assets/icons/LICENSES`, so the codicons
license isn't in the generated licenses file yet.
