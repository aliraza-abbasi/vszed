//! The status bar items of VS Code's `editorStatus.ts` and `languageStatus.ts`
//! that Zed has no equivalent for, and GitLens's current line blame.

use std::time::{SystemTime, UNIX_EPOCH};

use editor::{
    Editor, EditorEvent, EditorMode, MBTextSummary, MultiBufferSnapshot, RowInfo, SelectionEffects,
    ToPoint as _,
    actions::{
        ConvertIndentationToSpaces, ConvertIndentationToTabs, OpenGitBlameCommit,
        RestartLanguageServer, SelectAll, StopLanguageServer, ToggleGoToLine,
    },
};
use git::blame::BlameEntry;
use gpui::{Action, Anchor, Empty, Entity, Focusable as _, Subscription, WeakEntity};
use language::{LineEnding, Point, language_settings::LanguageSettings};
use project::{LspStoreEvent, Project};
use settings::{Settings as _, SettingsStore};
use ui::{
    ButtonLike, CommonAnimationExt as _, ContextMenu, ContextMenuEntry, PopoverMenu, Tooltip,
    prelude::*,
};
use workspace::{HideStatusItem, ItemHandle, StatusBarSettings, StatusItemView, Workspace};

use crate::Codicon;

/// VS Code's editor selection item: `Ln 4, Col 2 (12 selected)`, or
/// `3 selections (12 characters selected)` with several cursors. It stands in
/// for Zed's cursor position while the VS Code status bar is on.
pub struct CursorPositionItem {
    label: Option<SharedString>,
    editor: Option<WeakEntity<Editor>>,
    _editor_subscription: Option<Subscription>,
}

impl CursorPositionItem {
    pub(crate) fn new() -> Self {
        Self {
            label: None,
            editor: None,
            _editor_subscription: None,
        }
    }

    fn update_label(&mut self, editor: &Entity<Editor>, cx: &mut Context<Self>) {
        let label = editor
            .update(cx, |editor, cx| selection_label(editor, cx))
            .map(SharedString::from);
        if label != self.label {
            self.label = label;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub(crate) fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }
}

/// The text of VS Code's selection item. Like VS Code, the column counts a tab
/// as the columns it takes up (`getStatusbarColumn`), and selected line breaks
/// count as the length of the file's line ending.
fn selection_label(editor: &Editor, cx: &mut App) -> Option<String> {
    if !matches!(editor.mode(), EditorMode::Full { .. }) {
        return None;
    }
    let display_snapshot = editor.display_snapshot(cx);
    let snapshot = display_snapshot.buffer_snapshot();
    let selections = editor.selections.all_adjusted(&display_snapshot);
    let multi_buffer = editor.buffer().read(cx);
    // Zed stores line breaks as `\n`, so a CRLF line break is one character short.
    let extra_characters_per_line_break = multi_buffer
        .as_singleton()
        .is_some_and(|buffer| buffer.read(cx).line_ending() == LineEnding::Windows)
        as usize;
    let characters_selected: usize = selections
        .iter()
        .map(|selection| {
            let line_breaks = (selection.end.row - selection.start.row) as usize;
            snapshot
                .text_summary_for_range::<MBTextSummary, _>(selection.start..selection.end)
                .chars
                + line_breaks * extra_characters_per_line_break
        })
        .sum();

    match selections.as_slice() {
        [] => None,
        [selection] => {
            let head = selection.head();
            let (buffer, position) = snapshot.point_to_buffer_point(head)?;
            let tab_size = multi_buffer.language_settings_at(head, cx).tab_size.get();
            let line = position.row + 1;
            let column = visual_column(
                buffer
                    .text_for_range(Point::new(position.row, 0)..position)
                    .flat_map(str::chars),
                tab_size,
            ) + 1;
            Some(if characters_selected > 0 {
                format!("Ln {line}, Col {column} ({characters_selected} selected)")
            } else {
                format!("Ln {line}, Col {column}")
            })
        }
        selections => {
            let selection_count = selections.len();
            Some(if characters_selected > 0 {
                format!("{selection_count} selections ({characters_selected} characters selected)")
            } else {
                format!("{selection_count} selections")
            })
        }
    }
}

/// The zero-based column after `characters`, with tabs advancing to the next
/// tab stop (VS Code's `CursorColumns.toStatusbarColumn`).
fn visual_column(characters: impl Iterator<Item = char>, tab_size: u32) -> u32 {
    characters.fold(0, |column, character| {
        if character == '\t' {
            column + tab_size - column % tab_size
        } else {
            column + 1
        }
    })
}

impl Render for CursorPositionItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self.label.clone();
        let editor = self.editor.as_ref().and_then(WeakEntity::upgrade);
        let (Some(label), Some(editor)) = (label, editor) else {
            return Empty.into_any_element();
        };
        if !StatusBarSettings::get_global(cx).cursor_position_button {
            return Empty.into_any_element();
        }
        let focus_handle = editor.focus_handle(cx);

        Button::new("vscode-cursor-position", label.clone())
            .label_size(LabelSize::Small)
            .tab_index(0isize)
            .aria_label(label)
            .tooltip({
                let focus_handle = focus_handle.clone();
                move |_window, cx| {
                    Tooltip::for_action_in("Go to Line/Column", &ToggleGoToLine, &focus_handle, cx)
                }
            })
            .on_click(move |_, window, cx| {
                focus_handle.dispatch_action(&ToggleGoToLine, window, cx);
            })
            .into_any_element()
    }
}

impl StatusItemView for CursorPositionItem {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.label = None;
        self.editor = None;
        self._editor_subscription = None;
        if let Some(editor) = active_pane_item.and_then(|item| item.act_as::<Editor>(cx)) {
            self._editor_subscription = Some(cx.subscribe(&editor, |this, editor, event, cx| {
                if let EditorEvent::SelectionsChanged { .. } = event {
                    this.update_label(&editor, cx);
                }
            }));
            self.editor = Some(editor.downgrade());
            self.update_label(&editor, cx);
        }
        cx.notify();
    }

    fn hide_setting(&self, _cx: &App) -> Option<HideStatusItem> {
        Some(HideStatusItem::new(|settings| {
            settings
                .status_bar
                .get_or_insert_default()
                .cursor_position_button = Some(false);
        }))
    }
}

/// VS Code's indentation item, `Spaces: 4` or `Tab Size: 4`, from the active
/// file's language settings. Zed has no indentation size separate from the tab
/// size, so VS Code's `Spaces: 2 (Tab Size: 4)` form never applies.
pub struct IndentationItem {
    label: Option<SharedString>,
    editor: Option<WeakEntity<Editor>>,
    _editor_subscription: Option<Subscription>,
    _settings_subscription: Subscription,
}

impl IndentationItem {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            label: None,
            editor: None,
            _editor_subscription: None,
            _settings_subscription: cx.observe_global::<SettingsStore>(Self::update_label),
        }
    }

    fn update_label(&mut self, cx: &mut Context<Self>) {
        let label = self
            .editor
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .and_then(|editor| indentation_label(editor.read(cx), cx))
            .map(SharedString::from);
        if label != self.label {
            self.label = label;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub(crate) fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }
}

fn indentation_label(editor: &Editor, cx: &App) -> Option<String> {
    if !matches!(editor.mode(), EditorMode::Full { .. }) {
        return None;
    }
    let buffer = editor.active_buffer(cx)?;
    let settings = LanguageSettings::for_buffer(buffer.read(cx), cx);
    let tab_size = settings.tab_size;
    Some(if settings.hard_tabs {
        format!("Tab Size: {tab_size}")
    } else {
        format!("Spaces: {tab_size}")
    })
}

/// Converts the indentation of the whole file, like VS Code's commands of the
/// same name. Zed's actions only convert the selected lines, so the selections
/// cover the file while converting. Zed replaces the converted lines as a
/// whole, which would move anchors, so the selections are put back by column,
/// shifted by how much each line's indentation changed.
pub(crate) fn convert_file_indentation(
    editor: &Entity<Editor>,
    to_tabs: bool,
    window: &mut Window,
    cx: &mut App,
) {
    window.focus(&editor.focus_handle(cx), cx);
    editor.update(cx, |editor, cx| {
        let display_snapshot = editor.display_snapshot(cx);
        let old_snapshot = display_snapshot.buffer_snapshot();
        let selections = editor
            .selections
            .all::<Point>(&display_snapshot)
            .into_iter()
            .map(|selection| {
                let start_indentation = indentation_length(old_snapshot, selection.start.row);
                let end_indentation = indentation_length(old_snapshot, selection.end.row);
                (selection, start_indentation, end_indentation)
            })
            .collect::<Vec<_>>();

        editor.select_all(&SelectAll, window, cx);
        if to_tabs {
            editor.convert_indentation_to_tabs(&ConvertIndentationToTabs, window, cx);
        } else {
            editor.convert_indentation_to_spaces(&ConvertIndentationToSpaces, window, cx);
        }

        let new_snapshot = editor.buffer().read(cx).snapshot(cx);
        let move_point = |point: Point, old_indentation: u32| {
            let new_indentation = indentation_length(&new_snapshot, point.row);
            let column = if point.column >= old_indentation {
                point.column - old_indentation + new_indentation
            } else {
                point.column.min(new_indentation)
            };
            Point::new(point.row, column)
        };
        let selections = selections
            .into_iter()
            .map(|(mut selection, start_indentation, end_indentation)| {
                selection.start = move_point(selection.start, start_indentation);
                selection.end = move_point(selection.end, end_indentation);
                selection
            })
            .collect::<Vec<_>>();
        editor.change_selections(
            SelectionEffects::no_scroll(),
            window,
            cx,
            |selections_collection| {
                selections_collection.select(selections);
            },
        );
    });
}

/// The length in bytes of the spaces and tabs that start `row`.
fn indentation_length(snapshot: &MultiBufferSnapshot, row: u32) -> u32 {
    snapshot
        .chars_at(Point::new(row, 0))
        .take_while(|character| matches!(character, ' ' | '\t'))
        .count() as u32
}

impl Render for IndentationItem {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let label = self.label.clone();
        let editor = self.editor.clone();
        let Some(label) = label else {
            return Empty.into_any_element();
        };

        // VS Code's indentation picker also changes the view's indentation, tab
        // display size and detection, and trims trailing whitespace. Zed has no
        // per-file indentation or trim action, so only the conversions are here.
        PopoverMenu::new("vscode-indentation-menu")
            .anchor(Anchor::BottomLeft)
            .attach(Anchor::TopLeft)
            .trigger_with_tooltip(
                Button::new("vscode-indentation", label.clone())
                    .label_size(LabelSize::Small)
                    .tab_index(0isize)
                    .aria_label(label),
                Tooltip::text("Select Indentation"),
            )
            .menu(move |window, cx| {
                let editor = editor.as_ref()?.upgrade()?;
                let read_only = editor.read(cx).read_only(cx);
                Some(ContextMenu::build(window, cx, move |menu, _window, _cx| {
                    let convert_entry = |label: &'static str, to_tabs: bool| {
                        let editor = editor.clone();
                        ContextMenuEntry::new(label).disabled(read_only).handler(
                            move |window, cx| {
                                convert_file_indentation(&editor, to_tabs, window, cx)
                            },
                        )
                    };
                    menu.item(convert_entry("Convert Indentation to Spaces", false))
                        .item(convert_entry("Convert Indentation to Tabs", true))
                }))
            })
            .into_any_element()
    }
}

impl StatusItemView for IndentationItem {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor = None;
        self._editor_subscription = None;
        if let Some(editor) = active_pane_item.and_then(|item| item.act_as::<Editor>(cx)) {
            // The editor notifies when its buffer or language changes.
            self._editor_subscription =
                Some(cx.observe(&editor, |this, _editor, cx| this.update_label(cx)));
            self.editor = Some(editor.downgrade());
        }
        self.update_label(cx);
    }

    fn hide_setting(&self, _cx: &App) -> Option<HideStatusItem> {
        None
    }
}

/// VS Code's `{ }` language status item. VS Code fills it from language status
/// items that extensions report for the active file; here it lists the language
/// servers that serve the active file, and spins while one of them reports
/// progress, like a busy VS Code status.
pub struct LanguageStatusItem {
    project: Entity<Project>,
    editor: Option<WeakEntity<Editor>>,
    language_servers: Vec<LanguageServerEntry>,
    _editor_subscription: Option<Subscription>,
    _lsp_store_subscription: Subscription,
}

#[derive(Clone, Debug, PartialEq)]
struct LanguageServerEntry {
    name: SharedString,
    busy: bool,
    progress_title: Option<SharedString>,
}

impl LanguageServerEntry {
    fn menu_label(&self) -> SharedString {
        match &self.progress_title {
            Some(progress_title) => format!("{}: {progress_title}", self.name).into(),
            None => self.name.clone(),
        }
    }
}

impl LanguageStatusItem {
    pub(crate) fn new(workspace: &Workspace, cx: &mut Context<Self>) -> Self {
        let project = workspace.project().clone();
        let lsp_store = project.read(cx).lsp_store();
        let lsp_store_subscription = cx.subscribe(&lsp_store, |this, _, event, cx| match event {
            LspStoreEvent::LanguageServerAdded(..)
            | LspStoreEvent::LanguageServerRemoved(_)
            | LspStoreEvent::LanguageServerUpdate { .. } => this.update_language_servers(cx),
            _ => {}
        });
        Self {
            project,
            editor: None,
            language_servers: Vec::new(),
            _editor_subscription: None,
            _lsp_store_subscription: lsp_store_subscription,
        }
    }

    fn update_language_servers(&mut self, cx: &mut Context<Self>) {
        let language_servers = self
            .editor
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .map(|editor| active_language_servers(&self.project, editor.read(cx), cx))
            .unwrap_or_default();
        if language_servers != self.language_servers {
            self.language_servers = language_servers;
            cx.notify();
        }
    }
}

fn active_language_servers(
    project: &Entity<Project>,
    editor: &Editor,
    cx: &App,
) -> Vec<LanguageServerEntry> {
    if !matches!(editor.mode(), EditorMode::Full { .. }) {
        return Vec::new();
    }
    let Some(buffer) = editor.active_buffer(cx) else {
        return Vec::new();
    };
    let buffer = buffer.read(cx);
    let project = project.read(cx);
    let lsp_store = project.lsp_store().read(cx);
    let opened_in_servers = lsp_store.language_server_ids_for_opened_buffer(buffer.remote_id());
    let language_name = buffer.language().map(|language| language.name());
    let is_local = project.is_local();
    lsp_store
        .language_server_statuses()
        .filter(|(server_id, status)| {
            if is_local {
                opened_in_servers.is_some_and(|server_ids| server_ids.contains(server_id))
            } else {
                // Only the host knows which buffers a server has opened.
                language_name.is_some() && status.language_name == language_name
            }
        })
        .map(|(_, status)| LanguageServerEntry {
            name: status.name.0.clone(),
            busy: !status.pending_work.is_empty(),
            progress_title: status
                .pending_work
                .values()
                .find_map(|progress| progress.title.clone().filter(|title| !title.is_empty()))
                .map(SharedString::from),
        })
        .collect()
}

impl Render for LanguageStatusItem {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let editor = self.editor.clone();
        if self.language_servers.is_empty() {
            return Empty.into_any_element();
        }
        let busy = self.language_servers.iter().any(|server| server.busy);
        let server_names = self
            .language_servers
            .iter()
            .map(|server| server.name.as_ref())
            .collect::<Vec<_>>()
            .join(", ");
        let language_servers = self.language_servers.clone();
        let icon = if busy {
            Codicon::Loading
                .icon()
                .size(IconSize::Small)
                .with_rotate_animation(1)
                .into_any_element()
        } else {
            Codicon::Json
                .icon()
                .size(IconSize::Small)
                .into_any_element()
        };

        PopoverMenu::new("vscode-language-status-menu")
            .anchor(Anchor::BottomLeft)
            .attach(Anchor::TopLeft)
            .trigger_with_tooltip(
                ButtonLike::new("vscode-language-status")
                    .tab_index(0isize)
                    .aria_label(format!("Editor Language Status: {server_names}"))
                    .child(icon),
                move |_window, cx| {
                    Tooltip::with_meta("Editor Language Status", None, server_names.clone(), cx)
                },
            )
            .menu(move |window, cx| {
                let focus_handle = editor.as_ref()?.upgrade()?.focus_handle(cx);
                let language_servers = language_servers.clone();
                Some(ContextMenu::build(window, cx, move |menu, _window, _cx| {
                    let menu = language_servers
                        .iter()
                        .fold(menu, |menu, server| menu.label(server.menu_label()));
                    let editor_action_entry = |label: &'static str, action: Box<dyn Action>| {
                        let focus_handle = focus_handle.clone();
                        ContextMenuEntry::new(label).handler(move |window, cx| {
                            focus_handle.dispatch_action(&*action, window, cx);
                        })
                    };
                    menu.separator()
                        .item(editor_action_entry(
                            "Restart Language Servers",
                            RestartLanguageServer.boxed_clone(),
                        ))
                        .item(editor_action_entry(
                            "Stop Language Servers",
                            StopLanguageServer.boxed_clone(),
                        ))
                }))
            })
            .into_any_element()
    }
}

impl StatusItemView for LanguageStatusItem {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor = None;
        self._editor_subscription = None;
        if let Some(editor) = active_pane_item.and_then(|item| item.act_as::<Editor>(cx)) {
            // The editor notifies when its buffer or language changes.
            self._editor_subscription = Some(cx.observe(&editor, |this, _editor, cx| {
                this.update_language_servers(cx)
            }));
            self.editor = Some(editor.downgrade());
        }
        self.update_language_servers(cx);
    }

    fn hide_setting(&self, _cx: &App) -> Option<HideStatusItem> {
        None
    }
}

/// GitLens's current line blame, `$(git-commit) Michael Sloan, 18 months ago`,
/// for the newest cursor of the active editor. It reads the blame that the
/// editor keeps for its inline and gutter blame, so it shows next to Zed's
/// inline blame and is hidden whenever the editor has no blame for the line:
/// outside a repository, in untracked files, on uncommitted lines, and in
/// editors that never turned on inline blame or the blame gutter.
pub struct CurrentLineBlameItem {
    blame: Option<CurrentLineBlame>,
    editor: Option<WeakEntity<Editor>>,
    _editor_subscription: Option<Subscription>,
}

#[derive(Clone, Debug, PartialEq)]
struct CurrentLineBlame {
    label: SharedString,
    summary: SharedString,
}

impl CurrentLineBlame {
    fn new(entry: &BlameEntry, now: i64) -> Self {
        let author = entry.author.clone().unwrap_or_default();
        let label = match entry.author_time {
            Some(author_time) => format!("{author}, {}", relative_time(author_time, now)),
            None => author,
        };
        let summary = match &entry.summary {
            Some(summary) => summary.clone(),
            None => entry.sha.display_short(),
        };
        Self {
            label: label.into(),
            summary: summary.into(),
        }
    }
}

impl CurrentLineBlameItem {
    pub(crate) fn new() -> Self {
        Self {
            blame: None,
            editor: None,
            _editor_subscription: None,
        }
    }

    fn update_blame(&mut self, editor: &Entity<Editor>, cx: &mut Context<Self>) {
        let blame = cursor_line_blame_entry(editor, cx)
            .map(|entry| CurrentLineBlame::new(&entry, unix_now()));
        if blame != self.blame {
            self.blame = blame;
            cx.notify();
        }
    }
}

/// Unlike `Editor::active_git_blame_entry`, this doesn't wait for inline blame's
/// delay and also covers empty lines, as GitLens does.
fn cursor_line_blame_entry(editor: &Entity<Editor>, cx: &mut App) -> Option<BlameEntry> {
    let (blame, row_info) = {
        let editor = editor.read(cx);
        if !matches!(editor.mode(), EditorMode::Full { .. }) {
            return None;
        }
        let blame = editor.blame()?.clone();
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        let cursor = editor.selections.newest_anchor().head().to_point(&snapshot);
        let (buffer, position) = snapshot.point_to_buffer_point(cursor)?;
        let row_info = RowInfo {
            buffer_id: Some(buffer.remote_id()),
            buffer_row: Some(position.row),
            ..RowInfo::default()
        };
        (blame, row_info)
    };
    blame
        .update(cx, |blame, cx| {
            blame.blame_for_rows(&[row_info], cx).next().flatten()
        })
        .map(|(_, entry)| entry)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_secs() as i64)
}

/// GitLens's `fromNow` (`packages/utils/src/date.ts`), with the English text of
/// the `Intl.RelativeTimeFormat` it uses (`numeric: "auto"`). Unlike Zed's
/// relative dates it never combines units, and years only start at 729 days, so
/// a commit from a year and a half ago is `18 months ago`.
fn relative_time(timestamp: i64, now: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    const YEAR: i64 = 365 * DAY;
    const MONTH: i64 = YEAR / 12;
    // Unit, the elapsed seconds from which it is used, the seconds per unit, and
    // the words for one unit ago and ahead.
    const UNITS: [(&str, i64, i64, Option<(&str, &str)>); 7] = [
        ("year", 729 * DAY, YEAR, Some(("last year", "next year"))),
        ("month", MONTH, MONTH, Some(("last month", "next month"))),
        ("week", WEEK, WEEK, Some(("last week", "next week"))),
        ("day", DAY, DAY, Some(("yesterday", "tomorrow"))),
        ("hour", HOUR, HOUR, None),
        ("minute", MINUTE, MINUTE, None),
        ("second", 1, 1, None),
    ];

    let elapsed = timestamp - now;
    let (unit, seconds_per_unit, adjacent_words) = UNITS
        .into_iter()
        .find(|(_, threshold, _, _)| elapsed.abs() >= *threshold)
        .map(|(unit, _, seconds_per_unit, adjacent_words)| (unit, seconds_per_unit, adjacent_words))
        .unwrap_or(("second", 1, None));
    // Truncates toward zero, like GitLens's `Math.trunc`.
    let value = elapsed / seconds_per_unit;
    match (value, adjacent_words) {
        (0, _) => "now".to_string(),
        (-1, Some((ago, _))) => ago.to_string(),
        (1, Some((_, ahead))) => ahead.to_string(),
        (value, _) => {
            let count = value.unsigned_abs();
            let plural = if count == 1 { "" } else { "s" };
            if value < 0 {
                format!("{count} {unit}{plural} ago")
            } else {
                format!("in {count} {unit}{plural}")
            }
        }
    }
}

impl Render for CurrentLineBlameItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let blame = self.blame.clone();
        let editor = self.editor.as_ref().and_then(WeakEntity::upgrade);
        let (Some(blame), Some(editor)) = (blame, editor) else {
            return Empty.into_any_element();
        };
        let focus_handle = editor.focus_handle(cx);
        let summary = blame.summary;

        ButtonLike::new("vscode-current-line-blame")
            .tab_index(0isize)
            .aria_label(blame.label.clone())
            .child(
                h_flex()
                    .gap_1()
                    .child(Codicon::GitCommit.icon().size(IconSize::Small))
                    .child(Label::new(blame.label).size(LabelSize::Small)),
            )
            .tooltip({
                let focus_handle = focus_handle.clone();
                move |_window, cx| {
                    Tooltip::for_action_in(summary.clone(), &OpenGitBlameCommit, &focus_handle, cx)
                }
            })
            .on_click(move |_, window, cx| {
                focus_handle.dispatch_action(&OpenGitBlameCommit, window, cx);
            })
            .into_any_element()
    }
}

impl StatusItemView for CurrentLineBlameItem {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.blame = None;
        self.editor = None;
        self._editor_subscription = None;
        if let Some(editor) = active_pane_item.and_then(|item| item.act_as::<Editor>(cx)) {
            // The editor notifies when its selections change and when its blame
            // is loaded or updated.
            self._editor_subscription =
                Some(cx.observe(&editor, |this, editor, cx| this.update_blame(&editor, cx)));
            self.editor = Some(editor.downgrade());
            self.update_blame(&editor, cx);
        }
        cx.notify();
    }

    fn hide_setting(&self, _cx: &App) -> Option<HideStatusItem> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use futures::StreamExt as _;
    use git::{blame::Blame, repository::RepoPath};
    use gpui::{TestAppContext, VisualTestContext};
    use std::{num::NonZeroU32, ops::Range, path::Path};
    use util::{path, rel_path::rel_path};
    use workspace::MultiWorkspace;

    use crate::test_support::init_test;

    async fn create_project(
        file_name: &str,
        text: &str,
        cx: &mut TestAppContext,
    ) -> Entity<Project> {
        init_test(cx);
        cx.update(editor::init);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/dir"), serde_json::json!({ file_name: text }))
            .await;
        Project::test(fs, [path!("/dir").as_ref()], cx).await
    }

    async fn open_file<'a>(
        text: &str,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Workspace>, Entity<Editor>, &'a mut VisualTestContext) {
        let project = create_project("file.txt", text, cx).await;
        open_project_file(project, "file.txt", cx).await
    }

    async fn open_project_file<'a>(
        project: Entity<Project>,
        file_name: &str,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Workspace>, Entity<Editor>, &'a mut VisualTestContext) {
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
        let worktree_id = workspace.read_with(cx, |workspace, cx| {
            workspace
                .project()
                .read(cx)
                .worktrees(cx)
                .next()
                .expect("the project should have a worktree")
                .read(cx)
                .id()
        });
        let editor = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path((worktree_id, rel_path(file_name)), None, true, window, cx)
            })
            .await
            .expect("the file should open")
            .downcast::<Editor>()
            .expect("the file should open in an editor");
        cx.run_until_parked();
        (workspace, editor, cx)
    }

    fn add_status_item<T: StatusItemView>(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
        build: impl FnOnce(&Workspace, &mut Context<T>) -> T,
    ) -> Entity<T> {
        workspace.update_in(cx, |workspace, window, cx| {
            let item = cx.new(|cx| build(workspace, cx));
            workspace.status_bar().update(cx, |status_bar, cx| {
                status_bar.add_right_item(item.clone(), window, cx);
            });
            item
        })
    }

    fn select(editor: &Entity<Editor>, ranges: Vec<Range<Point>>, cx: &mut VisualTestContext) {
        editor.update_in(cx, |editor, window, cx| {
            editor.change_selections(SelectionEffects::no_scroll(), window, cx, |selections| {
                selections.select_ranges(ranges);
            });
        });
        cx.run_until_parked();
    }

    fn update_language_defaults(
        cx: &mut VisualTestContext,
        update: impl FnOnce(&mut settings::LanguageSettingsContent),
    ) {
        cx.update(|_window, cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |settings| {
                    update(&mut settings.project.all_languages.defaults);
                });
            });
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn test_cursor_position_uses_vscode_format(cx: &mut TestAppContext) {
        let (workspace, editor, cx) = open_file("\tone\ntwo three\nfour\n", cx).await;
        let item = add_status_item(&workspace, cx, |_, _| CursorPositionItem::new());
        let label = |cx: &mut VisualTestContext| {
            item.read_with(cx, |item, _| item.label().map(ToOwned::to_owned))
        };
        assert_eq!(label(cx).as_deref(), Some("Ln 1, Col 1"));

        // A tab counts as the columns it takes up, 4 by default.
        select(&editor, vec![Point::new(0, 1)..Point::new(0, 1)], cx);
        assert_eq!(label(cx).as_deref(), Some("Ln 1, Col 5"));

        select(&editor, vec![Point::new(1, 0)..Point::new(1, 3)], cx);
        assert_eq!(label(cx).as_deref(), Some("Ln 2, Col 4 (3 selected)"));

        // The line break counts as one character in a LF file.
        select(&editor, vec![Point::new(1, 4)..Point::new(2, 2)], cx);
        assert_eq!(label(cx).as_deref(), Some("Ln 3, Col 3 (8 selected)"));

        select(
            &editor,
            vec![
                Point::new(0, 0)..Point::new(0, 0),
                Point::new(2, 0)..Point::new(2, 0),
            ],
            cx,
        );
        assert_eq!(label(cx).as_deref(), Some("2 selections"));

        select(
            &editor,
            vec![
                Point::new(1, 0)..Point::new(1, 3),
                Point::new(2, 0)..Point::new(2, 4),
            ],
            cx,
        );
        assert_eq!(
            label(cx).as_deref(),
            Some("2 selections (7 characters selected)")
        );

        update_language_defaults(cx, |defaults| defaults.tab_size = NonZeroU32::new(2));
        select(&editor, vec![Point::new(0, 1)..Point::new(0, 1)], cx);
        assert_eq!(label(cx).as_deref(), Some("Ln 1, Col 3"));
    }

    #[gpui::test]
    async fn test_cursor_position_counts_crlf_line_breaks(cx: &mut TestAppContext) {
        let (workspace, editor, cx) = open_file("one\r\ntwo\r\n", cx).await;
        let item = add_status_item(&workspace, cx, |_, _| CursorPositionItem::new());
        select(&editor, vec![Point::new(0, 0)..Point::new(1, 3)], cx);
        assert_eq!(
            item.read_with(cx, |item, _| item.label().map(ToOwned::to_owned))
                .as_deref(),
            Some("Ln 2, Col 4 (8 selected)")
        );
    }

    #[gpui::test]
    async fn test_indentation_follows_language_settings(cx: &mut TestAppContext) {
        let (workspace, _editor, cx) = open_file("one\n", cx).await;
        let item = add_status_item(&workspace, cx, |_, cx| IndentationItem::new(cx));
        let label = |cx: &mut VisualTestContext| {
            item.read_with(cx, |item, _| item.label().map(ToOwned::to_owned))
        };
        assert_eq!(label(cx).as_deref(), Some("Spaces: 4"));

        update_language_defaults(cx, |defaults| defaults.tab_size = NonZeroU32::new(2));
        assert_eq!(label(cx).as_deref(), Some("Spaces: 2"));

        update_language_defaults(cx, |defaults| defaults.hard_tabs = Some(true));
        assert_eq!(label(cx).as_deref(), Some("Tab Size: 2"));
    }

    #[gpui::test]
    async fn test_convert_file_indentation(cx: &mut TestAppContext) {
        let (_workspace, editor, cx) = open_file("\tone\n\t\ttwo\nthree\n", cx).await;
        let selections = |editor: &Entity<Editor>, cx: &mut VisualTestContext| {
            editor.update(cx, |editor, cx| {
                let snapshot = editor.display_snapshot(cx);
                editor
                    .selections
                    .all::<Point>(&snapshot)
                    .into_iter()
                    .map(|selection| selection.range())
                    .collect::<Vec<_>>()
            })
        };
        select(
            &editor,
            vec![
                Point::new(1, 3)..Point::new(1, 3),
                Point::new(2, 1)..Point::new(2, 1),
            ],
            cx,
        );

        cx.update(|window, cx| convert_file_indentation(&editor, false, window, cx));
        cx.run_until_parked();
        assert_eq!(
            editor.update(cx, |editor, cx| editor.text(cx)),
            "    one\n        two\nthree\n"
        );
        assert_eq!(
            selections(&editor, cx),
            vec![
                Point::new(1, 9)..Point::new(1, 9),
                Point::new(2, 1)..Point::new(2, 1),
            ],
            "the cursors stay after the same characters"
        );

        cx.update(|window, cx| convert_file_indentation(&editor, true, window, cx));
        cx.run_until_parked();
        assert_eq!(
            editor.update(cx, |editor, cx| editor.text(cx)),
            "\tone\n\t\ttwo\nthree\n"
        );
        assert_eq!(
            selections(&editor, cx),
            vec![
                Point::new(1, 3)..Point::new(1, 3),
                Point::new(2, 1)..Point::new(2, 1),
            ]
        );
    }

    #[gpui::test]
    async fn test_language_status_hidden_without_language_servers(cx: &mut TestAppContext) {
        let (workspace, _editor, cx) = open_file("one\n", cx).await;
        let item = add_status_item(&workspace, cx, |workspace, cx| {
            LanguageStatusItem::new(workspace, cx)
        });
        item.read_with(cx, |item, _| assert!(item.language_servers.is_empty()));
    }

    #[gpui::test]
    async fn test_language_status_lists_language_servers(cx: &mut TestAppContext) {
        let project = create_project("main.rs", "fn main() {}\n", cx).await;
        let mut fake_servers = project.read_with(cx, |project, _| {
            project.languages().add(language::rust_lang());
            project
                .languages()
                .register_fake_lsp("Rust", language::FakeLspAdapter::default())
        });
        let (workspace, _editor, cx) = open_project_file(project, "main.rs", cx).await;
        let item = add_status_item(&workspace, cx, |workspace, cx| {
            LanguageStatusItem::new(workspace, cx)
        });
        let fake_server = fake_servers
            .next()
            .await
            .expect("the language server should start");
        cx.run_until_parked();
        let language_servers = |cx: &mut VisualTestContext| {
            item.read_with(cx, |item, _| item.language_servers.clone())
        };
        assert_eq!(
            language_servers(cx),
            vec![LanguageServerEntry {
                name: "the-fake-language-server".into(),
                busy: false,
                progress_title: None,
            }]
        );

        fake_server.start_progress("indexing").await;
        cx.run_until_parked();
        assert!(
            language_servers(cx).iter().all(|server| server.busy),
            "a server reporting progress is busy"
        );

        fake_server.end_progress("indexing");
        cx.run_until_parked();
        assert!(language_servers(cx).iter().all(|server| !server.busy));
    }

    const DAY: i64 = 24 * 60 * 60;

    fn blame_entry(
        sha: &str,
        rows: Range<u32>,
        author: &str,
        author_time: i64,
        summary: &str,
    ) -> BlameEntry {
        BlameEntry {
            sha: sha.parse().expect("the sha should be valid"),
            original_line_number: rows.start,
            range: rows,
            author: Some(author.to_string()),
            author_mail: None,
            author_time: Some(author_time),
            author_tz: Some("+0000".to_string()),
            committer_name: None,
            committer_email: None,
            committer_time: Some(author_time),
            committer_tz: Some("+0000".to_string()),
            summary: Some(summary.to_string()),
            previous: None,
            filename: "blamed.txt".to_string(),
            boundary: false,
        }
    }

    /// A repository with a blamed file and a file that git doesn't know about.
    async fn create_repository_project(now: i64, cx: &mut TestAppContext) -> Entity<Project> {
        init_test(cx);
        cx.update(editor::init);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/repo"),
            serde_json::json!({
                ".git": {},
                "blamed.txt": "one\ntwo\n\nfour\n",
                "untracked.txt": "new\n",
            }),
        )
        .await;
        fs.set_blame_for_repo(
            Path::new(path!("/repo/.git")),
            vec![(
                RepoPath::new("blamed.txt").expect("the path should be valid"),
                Blame {
                    entries: vec![
                        blame_entry(
                            "1b1b1b",
                            0..1,
                            "Michael Sloan",
                            now - 3 * DAY,
                            "Fix parsing",
                        ),
                        blame_entry(
                            "0d0d0d",
                            1..4,
                            "Max Brunsfeld",
                            now - 548 * DAY,
                            "Add the editor",
                        ),
                    ],
                    ..Blame::default()
                },
            )],
        );
        Project::test(fs, [path!("/repo").as_ref()], cx).await
    }

    fn current_line_blame(
        item: &Entity<CurrentLineBlameItem>,
        cx: &mut VisualTestContext,
    ) -> Option<(String, String)> {
        item.read_with(cx, |item, _| {
            item.blame
                .as_ref()
                .map(|blame| (blame.label.to_string(), blame.summary.to_string()))
        })
    }

    #[gpui::test]
    async fn test_current_line_blame_follows_cursor(cx: &mut TestAppContext) {
        let now = unix_now();
        let project = create_repository_project(now, cx).await;
        let (workspace, editor, cx) = open_project_file(project, "blamed.txt", cx).await;
        let item = add_status_item(&workspace, cx, |_, _| CurrentLineBlameItem::new());
        assert_eq!(
            current_line_blame(&item, cx),
            Some((
                "Michael Sloan, 3 days ago".to_string(),
                "Fix parsing".to_string()
            ))
        );

        // Unlike Zed's inline blame, the blame of empty lines is shown too.
        select(&editor, vec![Point::new(2, 0)..Point::new(2, 0)], cx);
        assert_eq!(
            current_line_blame(&item, cx),
            Some((
                "Max Brunsfeld, 18 months ago".to_string(),
                "Add the editor".to_string()
            ))
        );

        let worktree_id = workspace.read_with(cx, |workspace, cx| {
            workspace
                .project()
                .read(cx)
                .worktrees(cx)
                .next()
                .expect("the project should have a worktree")
                .read(cx)
                .id()
        });
        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(
                    (worktree_id, rel_path("untracked.txt")),
                    None,
                    true,
                    window,
                    cx,
                )
            })
            .await
            .expect("the file should open");
        cx.run_until_parked();
        assert_eq!(
            current_line_blame(&item, cx),
            None,
            "a file without blame has no current line blame"
        );
    }

    #[gpui::test]
    async fn test_current_line_blame_hidden_outside_repository(cx: &mut TestAppContext) {
        let (workspace, _editor, cx) = open_file("one\n", cx).await;
        let item = add_status_item(&workspace, cx, |_, _| CurrentLineBlameItem::new());
        assert_eq!(current_line_blame(&item, cx), None);
    }

    #[test]
    fn test_relative_time() {
        let now = 1_700_000_000;
        let ago = |seconds: i64| relative_time(now - seconds, now);
        assert_eq!(ago(0), "now");
        assert_eq!(ago(1), "1 second ago");
        assert_eq!(ago(59), "59 seconds ago");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(60 * 60 - 1), "59 minutes ago");
        assert_eq!(ago(60 * 60), "1 hour ago");
        assert_eq!(ago(DAY - 1), "23 hours ago");
        assert_eq!(ago(DAY), "yesterday");
        assert_eq!(ago(6 * DAY), "6 days ago");
        assert_eq!(ago(7 * DAY), "last week");
        assert_eq!(ago(30 * DAY), "4 weeks ago");
        assert_eq!(ago(31 * DAY), "last month");
        assert_eq!(ago(548 * DAY), "18 months ago");
        assert_eq!(ago(728 * DAY), "23 months ago");
        assert_eq!(ago(729 * DAY), "last year");
        assert_eq!(ago(730 * DAY), "2 years ago");
        assert_eq!(ago(-90), "in 1 minute");
        assert_eq!(ago(-DAY), "tomorrow");
        assert_eq!(ago(-2 * DAY), "in 2 days");
    }

    #[test]
    fn test_visual_column() {
        assert_eq!(visual_column("".chars(), 4), 0);
        assert_eq!(visual_column("ab".chars(), 4), 2);
        assert_eq!(visual_column("\t".chars(), 4), 4);
        assert_eq!(visual_column("a\t".chars(), 4), 4);
        assert_eq!(visual_column("abcd\tx".chars(), 4), 9);
        assert_eq!(visual_column(" \t\t".chars(), 2), 4);
        assert_eq!(visual_column("é\t".chars(), 8), 8);
    }
}
