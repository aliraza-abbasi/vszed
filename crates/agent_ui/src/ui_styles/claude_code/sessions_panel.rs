use std::path::{Path, PathBuf};

use acp_thread::{AgentSessionInfo, AgentSessionList, AgentSessionListRequest};
use agent_client_protocol::v2::schema as acp_v2;
use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use collections::HashSet;
use editor::{Editor, EditorEvent};
use gpui::{
    Action, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    Subscription, Task, WeakEntity, Window, actions, px,
};
use project::{AgentId, Project};
use std::rc::Rc;
use ui::{Tooltip, prelude::*};
use util::ResultExt as _;
use workspace::{
    PathList, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

use crate::agent_panel::AgentPanel;
use crate::thread_metadata_store::{ThreadId, ThreadMetadata, ThreadMetadataStore};
use crate::ui_styles::claude_code_style_enabled;
use crate::ui_styles::thread_item::{
    AgentThreadItem, ThreadTabRequest, connection_store, global_thread_store, new_thread_tab,
    open_thread_tab, panel_connection_store, threads_open_in_editor,
};
use crate::{Agent, AgentThreadSource, NewExternalAgentThread};

actions!(
    agent,
    [
        /// Toggles the Claude Code sessions panel.
        ToggleClaudeCodeSessions,
    ]
);

const PANEL_KEY: &str = "ClaudeCodeSessionsPanel";
const DEFAULT_WIDTH: Pixels = px(300.);

fn claude_agent() -> Agent {
    Agent::Custom {
        id: AgentId::new(agent_servers::CLAUDE_AGENT_ID),
    }
}

/// A Claude session of this project, as Claude's own session store lists it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ListedSession {
    pub session_id: acp_v2::SessionId,
    pub cwd: PathBuf,
    pub title: Option<SharedString>,
    pub updated_at: Option<DateTime<Utc>>,
}

/// Keeps the sessions whose working directory is exactly one of the project's
/// roots. Claude keys history by the exact directory, so another git worktree
/// of the same repository is a different project.
pub(crate) fn sessions_for_roots(
    sessions: Vec<AgentSessionInfo>,
    root: &Path,
) -> Vec<ListedSession> {
    sessions
        .into_iter()
        .filter_map(|session| {
            let work_dirs = session.work_dirs.as_ref();
            // The adapter filters by `cwd` itself; the returned directories are
            // checked too so that an adapter ignoring `cwd` can't leak other
            // projects into the list.
            let matches_root = work_dirs.is_none_or(|work_dirs| {
                work_dirs.paths().iter().any(|path| path.as_path() == root)
            });
            matches_root.then(|| ListedSession {
                session_id: session.session_id,
                cwd: root.to_path_buf(),
                title: session.title,
                updated_at: session.updated_at,
            })
        })
        .collect()
}

/// "now", "5m", "2h", "Yesterday", "3d", then the date.
pub(crate) fn relative_time(now: DateTime<Local>, then: DateTime<Local>) -> String {
    let elapsed = now.signed_duration_since(then);
    if elapsed.num_minutes() < 1 {
        return "now".to_string();
    }
    if elapsed.num_hours() < 1 {
        return format!("{}m", elapsed.num_minutes());
    }
    let days = now
        .date_naive()
        .signed_duration_since(then.date_naive())
        .num_days();
    if days == 0 {
        return format!("{}h", elapsed.num_hours());
    }
    if days == 1 {
        return "Yesterday".to_string();
    }
    if days < 7 {
        return format!("{days}d");
    }
    then.format("%b %-d").to_string()
}

#[derive(Clone, Debug, PartialEq)]
enum LoadState {
    Loading,
    Loaded,
    Unsupported,
    Error(SharedString),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionStatus {
    Idle,
    Running,
    WaitingForPermission,
}

struct SessionRow {
    session: ListedSession,
    title: SharedString,
    archived: bool,
    status: SessionStatus,
}

pub(crate) struct ClaudeCodeSessionsPanel {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    search_editor: Entity<Editor>,
    sessions: Vec<ListedSession>,
    load_state: LoadState,
    show_archived: bool,
    renaming: Option<(acp_v2::SessionId, Entity<Editor>)>,
    position: DockPosition,
    _refresh_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl ClaudeCodeSessionsPanel {
    pub(crate) fn new(workspace: &Workspace, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Search sessions…", window, cx);
            editor
        });
        let project = workspace.project().clone();
        let mut subscriptions = vec![
            cx.subscribe(&search_editor, |_, _, event: &EditorEvent, cx| {
                if matches!(event, EditorEvent::BufferEdited) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&project, window, |this, _, event, window, cx| {
                if matches!(
                    event,
                    project::Event::WorktreeAdded(_) | project::Event::WorktreeRemoved(_)
                ) {
                    this.refresh(window, cx);
                }
            }),
        ];
        if let Some(store) = ThreadMetadataStore::try_global(cx) {
            subscriptions.push(cx.observe(&store, |_, _, cx| cx.notify()));
        }
        Self {
            workspace: workspace.weak_handle(),
            project,
            focus_handle: cx.focus_handle(),
            search_editor,
            sessions: Vec::new(),
            load_state: LoadState::Loading,
            show_archived: false,
            renaming: None,
            position: DockPosition::Left,
            _refresh_task: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    fn project_roots(&self, cx: &App) -> Vec<PathBuf> {
        self.project
            .read(cx)
            .visible_worktrees(cx)
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
            .collect()
    }

    /// Lists the project's sessions from Claude's own store, one `session/list`
    /// per project root with `cwd` set to it.
    pub(crate) fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let roots = self.project_roots(cx);
        let agent = claude_agent();
        let thread_store = global_thread_store(cx);
        let server = agent.server(self.project.read(cx).fs().clone(), thread_store);
        let panel_store = panel_connection_store(workspace.read(cx), cx);
        let store = connection_store(panel_store, &self.project, cx);
        let entry = store.update(cx, |store, cx| store.request_connection(agent, server, cx));
        let connection = entry.read(cx).wait_for_connection();
        if self.sessions.is_empty() {
            self.load_state = LoadState::Loading;
        }
        cx.notify();
        self._refresh_task = cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let state = connection
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                let Some(list) = cx.update(|_, cx| state.connection.session_list(cx))? else {
                    return Ok(None);
                };
                let mut sessions = Vec::new();
                for root in roots {
                    sessions.extend(list_sessions_in(&list, &root, cx).await?);
                }
                anyhow::Ok(Some(sessions))
            }
            .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(Some(sessions)) => {
                        this.sessions = sessions;
                        this.load_state = LoadState::Loaded;
                    }
                    Ok(None) => this.load_state = LoadState::Unsupported,
                    Err(error) => {
                        log::warn!("Failed to list Claude sessions: {error:#}");
                        this.load_state = LoadState::Error(error.to_string().into());
                    }
                }
                cx.notify();
            })
            .log_err();
        });
    }

    fn rows(&self, cx: &App) -> Vec<SessionRow> {
        let store = ThreadMetadataStore::try_global(cx);
        let open_tabs = self
            .workspace
            .upgrade()
            .map(|workspace| {
                workspace
                    .read(cx)
                    .items_of_type::<AgentThreadItem>(cx)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let query = self.search_editor.read(cx).text(cx).to_lowercase();
        let mut seen = HashSet::default();
        let mut rows = self
            .sessions
            .iter()
            .filter(|session| seen.insert(session.session_id.clone()))
            .map(|session| {
                let metadata = store
                    .as_ref()
                    .and_then(|store| store.read(cx).entry_by_session(&session.session_id));
                let title = metadata
                    .and_then(|metadata| metadata.title_override.clone())
                    .or_else(|| session.title.clone())
                    .or_else(|| metadata.and_then(|metadata| metadata.title.clone()))
                    .unwrap_or_else(|| "Untitled session".into());
                let tab = open_tabs
                    .iter()
                    .find(|tab| tab.read(cx).session_id(cx).as_ref() == Some(&session.session_id));
                let status = match tab {
                    Some(tab) if tab.read(cx).is_waiting_for_permission(cx) => {
                        SessionStatus::WaitingForPermission
                    }
                    Some(tab) if tab.read(cx).is_generating(cx) => SessionStatus::Running,
                    _ => SessionStatus::Idle,
                };
                SessionRow {
                    session: session.clone(),
                    title,
                    archived: metadata.is_some_and(|metadata| metadata.archived),
                    status,
                }
            })
            .filter(|row| query.is_empty() || row.title.to_lowercase().contains(&query))
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| right.session.updated_at.cmp(&left.session.updated_at));
        rows
    }

    fn open_session(
        &mut self,
        session: &ListedSession,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let work_dirs = vec![session.cwd.clone()];
        if threads_open_in_editor(cx) {
            let thread_id = ThreadMetadataStore::try_global(cx).and_then(|store| {
                store
                    .read(cx)
                    .entry_by_session(&session.session_id)
                    .map(|metadata| metadata.thread_id)
            });
            let request = ThreadTabRequest {
                agent: claude_agent(),
                session_id: Some(session.session_id.clone()),
                thread_id,
                work_dirs: Some(work_dirs),
                title: Some(title),
            };
            workspace.update(cx, |workspace, cx| {
                open_thread_tab(workspace, request, true, window, cx);
            });
            return;
        }
        // The panel resumes sessions through Zed's thread metadata, so give
        // the session a row first if Zed hasn't seen it yet.
        let Some(thread_id) = ensure_metadata(session, Some(title.clone()), cx) else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            if let Some(panel) = workspace.focus_panel::<AgentPanel>(window, cx) {
                panel.update(cx, |panel, cx| {
                    panel.load_agent_thread(
                        claude_agent(),
                        thread_id,
                        Some(PathList::new(&[session.cwd.clone()])),
                        Some(title),
                        true,
                        AgentThreadSource::AgentPanel,
                        window,
                        cx,
                    );
                });
            }
        });
    }

    fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            if threads_open_in_editor(cx) {
                new_thread_tab(workspace, Some(claude_agent()), window, cx);
            } else if let Some(panel) = workspace.focus_panel::<AgentPanel>(window, cx) {
                panel.update(cx, |panel, cx| {
                    panel.new_external_agent_thread(
                        &NewExternalAgentThread {
                            agent: AgentId::new(agent_servers::CLAUDE_AGENT_ID),
                        },
                        window,
                        cx,
                    );
                });
            }
        });
    }

    fn set_archived(&mut self, session: &ListedSession, archived: bool, cx: &mut Context<Self>) {
        let Some(thread_id) = ensure_metadata(session, session.title.clone(), cx) else {
            return;
        };
        let Some(store) = ThreadMetadataStore::try_global(cx) else {
            return;
        };
        store.update(cx, |store, cx| {
            if archived {
                store.archive(thread_id, None, cx);
            } else {
                store.unarchive(thread_id, cx);
            }
        });
    }

    fn start_rename(
        &mut self,
        session: &ListedSession,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_text(title.to_string(), window, cx);
            editor.select_all(&editor::actions::SelectAll, window, cx);
            editor
        });
        let session_for_blur = session.clone();
        self._subscriptions.push(cx.subscribe_in(
            &editor,
            window,
            move |this, _, event: &EditorEvent, _, cx| {
                if matches!(event, EditorEvent::Blurred) {
                    this.finish_rename(&session_for_blur, true, cx);
                }
            },
        ));
        window.focus(&editor.focus_handle(cx), cx);
        self.renaming = Some((session.session_id.clone(), editor));
        cx.notify();
    }

    fn finish_rename(&mut self, session: &ListedSession, commit: bool, cx: &mut Context<Self>) {
        let Some((session_id, editor)) = self.renaming.take() else {
            return;
        };
        if session_id != session.session_id {
            self.renaming = Some((session_id, editor));
            return;
        }
        let title = editor.read(cx).text(cx).trim().to_string();
        if commit && !title.is_empty() {
            if let Some(thread_id) = ensure_metadata(session, session.title.clone(), cx)
                && let Some(store) = ThreadMetadataStore::try_global(cx)
            {
                store.update(cx, |store, cx| {
                    store.set_title_override(thread_id, title.into(), cx);
                });
            }
        }
        cx.notify();
    }

    fn render_status_dot(status: SessionStatus, cx: &App) -> impl IntoElement {
        let color = match status {
            SessionStatus::Idle => cx.theme().colors().border,
            SessionStatus::Running => cx.theme().status().success,
            SessionStatus::WaitingForPermission => cx.theme().status().info,
        };
        div().flex_none().size(px(7.)).rounded_full().bg(color)
    }

    fn render_row(
        &self,
        row: SessionRow,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let session = row.session.clone();
        let title = row.title.clone();
        let group = SharedString::from(format!("claude-code-session-{index}"));
        let renaming_editor = self
            .renaming
            .as_ref()
            .filter(|(session_id, _)| session_id == &session.session_id)
            .map(|(_, editor)| editor.clone());
        let time = row
            .session
            .updated_at
            .map(|updated_at| relative_time(Local::now(), updated_at.with_timezone(&Local)))
            .unwrap_or_default();

        h_flex()
            .id(("claude-code-session", index))
            .debug_selector(|| format!("claude-code-session-{}", session.session_id.0))
            .group(group.clone())
            .w_full()
            .min_w_0()
            .gap_2()
            .px_3()
            .py_1()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(cx.theme().colors().ghost_element_hover))
            .child(Self::render_status_dot(row.status, cx))
            .child(match renaming_editor {
                Some(editor) => div()
                    .flex_1()
                    .min_w_0()
                    .key_context("ClaudeCodeSessionRename")
                    .on_action(cx.listener({
                        let session = session.clone();
                        move |this, _: &menu::Confirm, _, cx| this.finish_rename(&session, true, cx)
                    }))
                    .on_action(cx.listener({
                        let session = session.clone();
                        move |this, _: &menu::Cancel, _, cx| this.finish_rename(&session, false, cx)
                    }))
                    .child(editor)
                    .into_any_element(),
                None => div()
                    .flex_1()
                    .min_w_0()
                    .child(Label::new(title.clone()).single_line().truncate())
                    .into_any_element(),
            })
            .child(
                h_flex()
                    .flex_none()
                    .visible_on_hover(group.clone())
                    .gap_0p5()
                    .when(!row.archived, |this| {
                        this.child(
                            IconButton::new(("rename-session", index), IconName::Pencil)
                                .icon_size(IconSize::XSmall)
                                .tooltip(Tooltip::text("Rename"))
                                .on_click(cx.listener({
                                    let session = session.clone();
                                    let title = title.clone();
                                    move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.start_rename(&session, title.clone(), window, cx);
                                    }
                                })),
                        )
                    })
                    .child(
                        IconButton::new(
                            ("archive-session", index),
                            if row.archived {
                                IconName::Undo
                            } else {
                                IconName::Archive
                            },
                        )
                        .icon_size(IconSize::XSmall)
                        .tooltip(Tooltip::text(if row.archived {
                            "Unarchive session"
                        } else {
                            "Archive"
                        }))
                        .on_click(cx.listener({
                            let session = session.clone();
                            let archived = row.archived;
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.set_archived(&session, !archived, cx);
                            }
                        })),
                    ),
            )
            .child(
                Label::new(time)
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .flex_none(),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_session(&session, title.clone(), window, cx);
            }))
    }
}

async fn list_sessions_in(
    list: &Rc<dyn AgentSessionList>,
    root: &Path,
    cx: &mut AsyncWindowContext,
) -> Result<Vec<ListedSession>> {
    let mut sessions = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let request = AgentSessionListRequest {
            cwd: Some(root.to_path_buf()),
            cursor: cursor.clone(),
            ..Default::default()
        };
        let response = cx.update(|_, cx| list.list_sessions(request, cx))?.await?;
        sessions.extend(sessions_for_roots(response.sessions, root));
        match response.next_cursor {
            Some(next) if Some(&next) != cursor.as_ref() => cursor = Some(next),
            _ => break,
        }
    }
    Ok(sessions)
}

/// Gives a Claude session a Zed thread metadata row, which Zed's archive,
/// rename and panel resume work through.
fn ensure_metadata(
    session: &ListedSession,
    title: Option<SharedString>,
    cx: &mut App,
) -> Option<ThreadId> {
    let store = ThreadMetadataStore::try_global(cx)?;
    if let Some(existing) = store.read(cx).entry_by_session(&session.session_id) {
        return Some(existing.thread_id);
    }
    let thread_id = ThreadId::new();
    let updated_at = session.updated_at.unwrap_or_else(Utc::now);
    store.update(cx, |store, cx| {
        store.save(
            ThreadMetadata {
                thread_id,
                session_id: Some(session.session_id.clone()),
                agent_id: AgentId::new(agent_servers::CLAUDE_AGENT_ID),
                title,
                title_override: None,
                updated_at,
                created_at: None,
                interacted_at: None,
                worktree_paths: project::WorktreePaths::from_folder_paths(&PathList::new(&[
                    session.cwd.clone(),
                ])),
                remote_connection: None,
                archived: false,
            },
            cx,
        );
    });
    Some(thread_id)
}

impl EventEmitter<PanelEvent> for ClaudeCodeSessionsPanel {}

impl Focusable for ClaudeCodeSessionsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for ClaudeCodeSessionsPanel {
    fn persistent_name() -> &'static str {
        "Claude Code"
    }

    fn panel_key() -> &'static str {
        PANEL_KEY
    }

    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(
        &mut self,
        position: DockPosition,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.position = position;
        cx.notify();
    }

    fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
        DEFAULT_WIDTH
    }

    fn icon(&self, _window: &Window, cx: &App) -> Option<IconName> {
        claude_code_style_enabled(cx).then_some(IconName::AiClaude)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Claude Code")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleClaudeCodeSessions)
    }

    fn activation_priority(&self) -> u32 {
        9
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            self.refresh(window, cx);
        }
    }
}

impl Render for ClaudeCodeSessionsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows(cx);
        let (archived, active): (Vec<_>, Vec<_>) = rows.into_iter().partition(|row| row.archived);
        let archived_count = archived.len();
        let has_query = !self.search_editor.read(cx).is_empty(cx);
        let mut index = 0;
        let mut next_index = || {
            index += 1;
            index
        };

        let empty_text: Option<SharedString> = match &self.load_state {
            LoadState::Loading if active.is_empty() => Some("Loading sessions…".into()),
            LoadState::Unsupported => Some("This Claude agent can't list its sessions.".into()),
            LoadState::Error(error) => Some(format!("Couldn't list sessions: {error}").into()),
            _ if active.is_empty() && has_query => Some("No matching sessions.".into()),
            _ if active.is_empty() => Some("No sessions in this project yet.".into()),
            _ => None,
        };

        v_flex()
            .id("claude-code-sessions-panel")
            .key_context("ClaudeCodeSessionsPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .child(
                h_flex()
                    .h(px(35.))
                    .flex_none()
                    .px_3()
                    .justify_between()
                    .child(
                        Label::new("CLAUDE CODE")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        IconButton::new("refresh-claude-sessions", IconName::RotateCw)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Refresh"))
                            .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                    ),
            )
            .child(
                div().px_2().pb_1().child(
                    Button::new("new-claude-session", "New session")
                        .start_icon(Icon::new(IconName::Plus).size(IconSize::Small))
                        .full_width()
                        .style(ButtonStyle::Outlined)
                        .on_click(cx.listener(|this, _, window, cx| this.new_session(window, cx))),
                ),
            )
            .child(
                div()
                    .mx_2()
                    .mb_1()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .child(self.search_editor.clone()),
            )
            .child(
                v_flex()
                    .id("claude-code-session-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_1()
                    .when_some(empty_text, |this, text| {
                        this.child(
                            div()
                                .px_3()
                                .py_2()
                                .child(Label::new(text).size(LabelSize::Small).color(Color::Muted)),
                        )
                    })
                    .children(
                        active
                            .into_iter()
                            .map(|row| self.render_row(row, next_index(), cx).into_any_element())
                            .collect::<Vec<_>>(),
                    )
                    .when(archived_count > 0, |this| {
                        let show_archived = self.show_archived;
                        this.child(
                            h_flex()
                                .id("claude-code-archived-sessions")
                                .px_3()
                                .pt_3()
                                .pb_1()
                                .gap_1()
                                .cursor_pointer()
                                .child(
                                    Icon::new(if show_archived {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .size(IconSize::XSmall)
                                    .color(Color::Muted),
                                )
                                .child(
                                    Label::new(format!("Archived sessions ({archived_count})"))
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_archived = !this.show_archived;
                                    cx.notify();
                                })),
                        )
                        .when(show_archived, |this| {
                            this.children(
                                archived
                                    .into_iter()
                                    .map(|row| {
                                        self.render_row(row, next_index(), cx).into_any_element()
                                    })
                                    .collect::<Vec<_>>(),
                            )
                        })
                    }),
            )
    }
}

/// Adds the panel to workspaces while the Claude Code style is on, and removes
/// it when the style is switched off, so stock Zed is unchanged.
pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        workspace.register_action(|workspace, _: &ToggleClaudeCodeSessions, window, cx| {
            if claude_code_style_enabled(cx) {
                workspace.toggle_panel_focus::<ClaudeCodeSessionsPanel>(window, cx);
            }
        });
        sync_panel(workspace, window, cx);
        cx.observe_global_in::<settings::SettingsStore>(window, |workspace, window, cx| {
            sync_panel(workspace, window, cx);
        })
        .detach();
    })
    .detach();
}

fn sync_panel(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let existing = workspace.panel::<ClaudeCodeSessionsPanel>(cx);
    match (claude_code_style_enabled(cx), existing) {
        (true, None) => {
            let panel = cx.new(|cx| ClaudeCodeSessionsPanel::new(workspace, window, cx));
            workspace.add_panel(panel, window, cx);
        }
        (false, Some(panel)) => workspace.remove_panel(&panel, window, cx),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation_view::tests::init_test;
    use acp_thread::AgentSessionListResponse;
    use chrono::TimeZone as _;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use std::cell::RefCell;
    use std::path::Path;
    use workspace::MultiWorkspace;

    fn session(id: &str, cwd: &str) -> AgentSessionInfo {
        let mut info = AgentSessionInfo::new(id);
        info.work_dirs = Some(PathList::new(&[PathBuf::from(cwd)]));
        info.title = Some(format!("Session {id}").into());
        info
    }

    /// A session list over sessions of two projects. `honors_cwd` decides
    /// whether it filters by the requested `cwd` like claude-agent-acp does.
    struct FakeSessionList {
        sessions: Vec<AgentSessionInfo>,
        honors_cwd: bool,
        requested_cwds: RefCell<Vec<Option<PathBuf>>>,
    }

    impl AgentSessionList for FakeSessionList {
        fn list_sessions(
            &self,
            request: AgentSessionListRequest,
            _cx: &mut App,
        ) -> Task<Result<AgentSessionListResponse>> {
            self.requested_cwds.borrow_mut().push(request.cwd.clone());
            let sessions = self
                .sessions
                .iter()
                .filter(|session| {
                    !self.honors_cwd
                        || request.cwd.as_ref().is_none_or(|cwd| {
                            session.work_dirs.as_ref().is_some_and(|work_dirs| {
                                work_dirs.paths().iter().any(|path| path == cwd)
                            })
                        })
                })
                .cloned()
                .collect();
            Task::ready(Ok(AgentSessionListResponse::new(sessions)))
        }

        fn into_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
            self
        }
    }

    #[gpui::test]
    async fn test_session_list_is_scoped_to_the_project(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (_, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));

        let all_sessions = vec![
            session("a", "/work/app"),
            session("b", "/work/other"),
            session("c", "/work/app-worktree"),
            session("d", "/work/app"),
        ];
        for honors_cwd in [true, false] {
            let fake = Rc::new(FakeSessionList {
                sessions: all_sessions.clone(),
                honors_cwd,
                requested_cwds: RefCell::new(Vec::new()),
            });
            let list: Rc<dyn AgentSessionList> = fake.clone();
            let root = Path::new("/work/app");
            let sessions = cx
                .update(|window, cx| {
                    window.spawn(cx, async move |cx| list_sessions_in(&list, root, cx).await)
                })
                .await
                .expect("listing succeeds");
            let ids = sessions
                .iter()
                .map(|session| session.session_id.0.to_string())
                .collect::<Vec<_>>();
            assert_eq!(ids, vec!["a", "d"], "honors_cwd: {honors_cwd}");
            assert_eq!(
                fake.requested_cwds.borrow().as_slice(),
                &[Some(PathBuf::from("/work/app"))]
            );
        }
    }

    #[gpui::test]
    async fn test_clicking_a_session_opens_or_focuses_its_tab(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| {
            crate::ui_styles::init(cx);
            gpui::UpdateGlobal::update_global(cx, |store: &mut settings::SettingsStore, cx| {
                store
                    .set_user_settings(
                        r#"{ "agent": { "ui_style": "claude_code", "thread_location": "editor" } }"#,
                        cx,
                    )
                    .expect("valid settings");
            });
        });
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
        let panel = workspace
            .read_with(cx, |workspace, cx| {
                workspace.panel::<ClaudeCodeSessionsPanel>(cx)
            })
            .expect("the Claude Code style adds the sessions panel");

        let listed = ListedSession {
            session_id: acp_v2::SessionId::new("claude-session"),
            cwd: PathBuf::from("/work/app"),
            title: Some("Fix the build".into()),
            updated_at: None,
        };
        for _ in 0..2 {
            panel.update_in(cx, |panel, window, cx| {
                panel.open_session(&listed, "Fix the build".into(), window, cx);
            });
            cx.run_until_parked();
        }
        let tabs = workspace.read_with(cx, |workspace, cx| {
            workspace
                .items_of_type::<AgentThreadItem>(cx)
                .map(|item| item.read(cx).session_id(cx))
                .collect::<Vec<_>>()
        });
        assert_eq!(tabs, vec![Some(listed.session_id.clone())]);
    }

    #[test]
    fn test_relative_time() {
        let now = Local
            .with_ymd_and_hms(2026, 11, 11, 15, 0, 0)
            .single()
            .expect("valid time");
        let ago = |minutes: i64| now - chrono::Duration::minutes(minutes);
        assert_eq!(relative_time(now, ago(0)), "now");
        assert_eq!(relative_time(now, ago(5)), "5m");
        assert_eq!(relative_time(now, ago(120)), "2h");
        assert_eq!(relative_time(now, ago(16 * 60)), "Yesterday");
        assert_eq!(relative_time(now, ago(3 * 24 * 60)), "3d");
        assert_eq!(relative_time(now, ago(30 * 24 * 60)), "Oct 12");
    }
}
