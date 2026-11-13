use std::path::{Path, PathBuf};
use std::time::Duration;

use acp_thread::{AgentSessionInfo, AgentSessionList, AgentSessionListRequest};
use agent_client_protocol::v2::schema as acp_v2;
use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use collections::HashSet;
use db::kvp::KeyValueStore;
use editor::{Editor, EditorEvent};
use gpui::{
    Action, Anchor, AnyElement, App, AsyncWindowContext, Context, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, Stateful, Subscription, Task, WeakEntity, Window, actions,
    px,
};
use project::{AgentId, Project};
use serde::{Deserialize, Serialize};
use std::rc::Rc;
use ui::{ButtonLike, ContextMenu, PopoverMenu, Tooltip, prelude::*, right_click_menu};
use util::ResultExt as _;
use workspace::{
    PathList, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

use crate::agent_panel::AgentPanel;
use crate::thread_metadata_store::{ThreadId, ThreadMetadata, ThreadMetadataStore};
use crate::ui_styles::claude_code::side_bar_label_size;
use crate::ui_styles::claude_code::usage::{self, UsageWindow};
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
const FILTERS_KEY: &str = "claude_code_session_filters";
/// Groups are saved per set of project folders, like the extension saves
/// them per workspace folder.
const GROUPS_NAMESPACE: &str = "claude_code_session_groups";
const NEW_GROUP_NAME: &str = "New group";
const WEB_SESSIONS_UNAVAILABLE: &str =
    "Cloud sessions from claude.ai aren't available through the Claude adapter";

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

/// What the filters look at for a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SessionState {
    status: SessionStatus,
    /// It has a tab in this window.
    is_open: bool,
    /// A turn ended while its tab was hidden.
    unread: bool,
    /// It is in the Claude tab focused last.
    is_selected: bool,
}

impl SessionState {
    /// The extension's Active filter: sessions that need input, are working
    /// or are unread, plus the session in the Claude tab focused last.
    fn is_active(&self) -> bool {
        self.status != SessionStatus::Idle || self.unread || self.is_selected
    }
}

/// The sessions list filters: the Active toggle and the funnel's checks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct SessionFilters {
    active: bool,
    needs_input: bool,
    working: bool,
    completed: bool,
    open: bool,
    closed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilterCheck {
    NeedsInput,
    Working,
    Completed,
    Open,
    Closed,
}

impl FilterCheck {
    const ALL: [Self; 5] = [
        Self::NeedsInput,
        Self::Working,
        Self::Completed,
        Self::Open,
        Self::Closed,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::NeedsInput => "Needs input",
            Self::Working => "Working",
            Self::Completed => "Completed",
            Self::Open => "Open",
            Self::Closed => "Closed",
        }
    }
}

impl SessionFilters {
    fn check(&self, check: FilterCheck) -> bool {
        match check {
            FilterCheck::NeedsInput => self.needs_input,
            FilterCheck::Working => self.working,
            FilterCheck::Completed => self.completed,
            FilterCheck::Open => self.open,
            FilterCheck::Closed => self.closed,
        }
    }

    fn toggle_check(&mut self, check: FilterCheck) {
        let value = match check {
            FilterCheck::NeedsInput => &mut self.needs_input,
            FilterCheck::Working => &mut self.working,
            FilterCheck::Completed => &mut self.completed,
            FilterCheck::Open => &mut self.open,
            FilterCheck::Closed => &mut self.closed,
        };
        *value = !*value;
    }

    fn has_checks(&self) -> bool {
        FilterCheck::ALL.into_iter().any(|check| self.check(check))
    }

    fn is_filtering(&self) -> bool {
        self.active || self.has_checks()
    }

    /// The status checks pick sessions in any checked state, and Open or
    /// Closed narrow them by whether they have a tab.
    fn matches_checks(&self, state: &SessionState) -> bool {
        let status_checked = self.needs_input || self.working || self.completed;
        let status_matches = !status_checked
            || match state.status {
                SessionStatus::WaitingForPermission => self.needs_input,
                SessionStatus::Running => self.working,
                SessionStatus::Idle => self.completed,
            };
        let open_checked = self.open || self.closed;
        let open_matches =
            !open_checked || (self.open && state.is_open) || (self.closed && !state.is_open);
        status_matches && open_matches
    }

    /// With Active on, the checks add the sessions they match to the active ones.
    fn matches(&self, state: &SessionState) -> bool {
        if self.active {
            state.is_active() || (self.has_checks() && self.matches_checks(state))
        } else {
            self.matches_checks(state)
        }
    }
}

/// A named, collapsible group of sessions in the list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SessionGroup {
    id: u64,
    name: String,
    #[serde(default)]
    session_ids: Vec<String>,
    #[serde(default)]
    collapsed: bool,
}

struct SessionRow {
    session: ListedSession,
    title: SharedString,
    archived: bool,
    state: SessionState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    AccountAndUsage,
    SessionManager,
}

impl Section {
    fn title(self) -> &'static str {
        match self {
            Self::AccountAndUsage => "ACCOUNT & USAGE",
            Self::SessionManager => "SESSION MANAGER",
        }
    }
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
    renaming_group: Option<(u64, Entity<Editor>)>,
    filters: SessionFilters,
    search_visible: bool,
    last_focused_session: Option<acp_v2::SessionId>,
    groups: Vec<SessionGroup>,
    /// The key `groups` were loaded from, so they are saved for the same folders.
    groups_key: Option<String>,
    position: DockPosition,
    has_requested_sessions: bool,
    account_and_usage_expanded: bool,
    session_manager_expanded: bool,
    /// The reset time `_usage_reset_timer` re-renders the panel at, so a
    /// window that has reset stops showing its old reading.
    scheduled_usage_reset: Option<i64>,
    _usage_reset_timer: Task<()>,
    _account_watch: usage::AccountFileWatch,
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
                    this.load_groups(cx);
                    this.refresh(window, cx);
                }
            }),
            usage::observe_usage(cx),
        ];
        if let Some(workspace_entity) = workspace.weak_handle().upgrade() {
            subscriptions.push(cx.subscribe(
                &workspace_entity,
                |this, workspace, event: &workspace::Event, cx| {
                    if matches!(event, workspace::Event::ActiveItemChanged) {
                        this.track_focused_tab(&workspace, cx);
                    }
                },
            ));
        }
        let filters = KeyValueStore::global(cx)
            .read_kvp(FILTERS_KEY)
            .log_err()
            .flatten()
            .and_then(|value| serde_json::from_str::<SessionFilters>(&value).log_err())
            .unwrap_or_default();
        if let Some(store) = ThreadMetadataStore::try_global(cx) {
            subscriptions.push(cx.observe(&store, |_, _, cx| cx.notify()));
        }
        let account_watch = usage::watch_account_file(project.read(cx).fs().clone(), cx);
        let mut this = Self {
            workspace: workspace.weak_handle(),
            project,
            focus_handle: cx.focus_handle(),
            search_editor,
            sessions: Vec::new(),
            load_state: LoadState::Loading,
            show_archived: false,
            renaming: None,
            renaming_group: None,
            filters,
            search_visible: false,
            last_focused_session: None,
            groups: Vec::new(),
            groups_key: None,
            position: DockPosition::Left,
            has_requested_sessions: false,
            account_and_usage_expanded: true,
            session_manager_expanded: true,
            scheduled_usage_reset: None,
            _usage_reset_timer: Task::ready(()),
            _account_watch: account_watch,
            _refresh_task: Task::ready(()),
            _subscriptions: subscriptions,
        };
        this.load_groups(cx);
        this
    }

    /// The usage limits from Claude Code's account cache and the most recent
    /// rate limit reports of the project's Claude threads.
    pub(crate) fn usage_windows(&self, cx: &App) -> Vec<UsageWindow> {
        usage::project_usage_windows(self.project.entity_id(), Utc::now().timestamp(), cx)
    }

    fn schedule_usage_refresh(&mut self, windows: &[UsageWindow], cx: &mut Context<Self>) {
        let next_reset = windows.iter().filter_map(|window| window.resets_at).min();
        if next_reset == self.scheduled_usage_reset {
            return;
        }
        self.scheduled_usage_reset = next_reset;
        self._usage_reset_timer = match next_reset {
            Some(resets_at) => {
                let seconds_left = resets_at.saturating_sub(Utc::now().timestamp());
                let delay =
                    Duration::from_secs(u64::try_from(seconds_left).unwrap_or(0).saturating_add(1));
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    this.update(cx, |this, cx| {
                        this.scheduled_usage_reset = None;
                        cx.notify();
                    })
                    .log_err();
                })
            }
            None => Task::ready(()),
        };
    }

    fn is_expanded(&self, section: Section) -> bool {
        match section {
            Section::AccountAndUsage => self.account_and_usage_expanded,
            Section::SessionManager => self.session_manager_expanded,
        }
    }

    fn toggle_section(&mut self, section: Section, cx: &mut Context<Self>) {
        match section {
            Section::AccountAndUsage => {
                self.account_and_usage_expanded = !self.account_and_usage_expanded
            }
            Section::SessionManager => {
                self.session_manager_expanded = !self.session_manager_expanded
            }
        }
        cx.notify();
    }

    fn render_section_header(&self, section: Section, cx: &mut Context<Self>) -> Stateful<Div> {
        let expanded = self.is_expanded(section);
        h_flex()
            .id(match section {
                Section::AccountAndUsage => "claude-code-account-and-usage-header",
                Section::SessionManager => "claude-code-session-manager-header",
            })
            .w_full()
            .px_2()
            .py_1()
            .gap_1()
            .cursor_pointer()
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(IconSize::XSmall)
                .color(Color::Muted),
            )
            .child(
                Label::new(section.title())
                    .size(side_bar_label_size(0.85, cx))
                    .weight(FontWeight::SEMIBOLD)
                    .color(Color::Muted),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_section(section, cx)))
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
        let agent = claude_agent();
        let thread_store = global_thread_store(cx);
        let server = agent.server(self.project.read(cx).fs().clone(), thread_store);
        self.refresh_with_server(agent, server, window, cx);
    }

    fn refresh_with_server(
        &mut self,
        agent: Agent,
        server: Rc<dyn agent_servers::AgentServer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        self.has_requested_sessions = true;
        let roots = self.project_roots(cx);
        let panel_store = panel_connection_store(workspace.read(cx), cx);
        let store = connection_store(panel_store, &self.project, cx);
        let entry = store.update(cx, |store, cx| store.request_connection(agent, server, cx));
        let connection = entry.read(cx).wait_for_connection();
        if self.sessions.is_empty() {
            self.load_state = LoadState::Loading;
        }
        cx.notify();
        self._refresh_task = cx.spawn_in(window, async move |this, cx| {
            // At startup the agent panel may not be loaded yet, so `store` can be a
            // fallback that nothing else holds. Releasing it shuts its connection
            // down, which would fail the listing with "owner was released".
            let _connection_store = store;
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
        let query = self.search_query(cx);
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
                    state: SessionState {
                        status,
                        is_open: tab.is_some(),
                        unread: tab.is_some_and(|tab| tab.read(cx).finished_while_hidden()),
                        is_selected: tab.is_some()
                            && self.last_focused_session.as_ref() == Some(&session.session_id),
                    },
                }
            })
            .filter(|row| query.is_empty() || row.title.to_lowercase().contains(&query))
            .collect::<Vec<_>>();
        rows.sort_by_key(|row| std::cmp::Reverse(row.session.updated_at));
        rows
    }

    fn search_query(&self, cx: &App) -> String {
        if self.search_visible {
            self.search_editor.read(cx).text(cx).trim().to_lowercase()
        } else {
            String::new()
        }
    }

    fn track_focused_tab(&mut self, workspace: &Entity<Workspace>, cx: &mut Context<Self>) {
        let Some(session_id) = workspace
            .read(cx)
            .active_item_as::<AgentThreadItem>(cx)
            .and_then(|item| item.read(cx).session_id(cx))
        else {
            return;
        };
        if self.last_focused_session.as_ref() != Some(&session_id) {
            self.last_focused_session = Some(session_id);
            cx.notify();
        }
    }

    fn set_filters(&mut self, filters: SessionFilters, cx: &mut Context<Self>) {
        if self.filters == filters {
            return;
        }
        self.filters = filters;
        if let Some(value) = serde_json::to_string(&filters).log_err() {
            let store = KeyValueStore::global(cx);
            db::write_and_log(cx, move || async move {
                store.write_kvp(FILTERS_KEY.to_string(), value).await
            });
        }
        cx.notify();
    }

    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_visible = !self.search_visible;
        if self.search_visible {
            window.focus(&self.search_editor.focus_handle(cx), cx);
        } else {
            self.search_editor
                .update(cx, |editor, cx| editor.set_text("", window, cx));
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    fn load_groups(&mut self, cx: &mut Context<Self>) {
        let key = groups_key(&self.project_roots(cx));
        if key == self.groups_key {
            return;
        }
        self.groups = key
            .as_deref()
            .and_then(|key| {
                KeyValueStore::global(cx)
                    .scoped(GROUPS_NAMESPACE)
                    .read(key)
                    .log_err()
                    .flatten()
            })
            .and_then(|value| serde_json::from_str::<Vec<SessionGroup>>(&value).log_err())
            .unwrap_or_default();
        self.groups_key = key;
        self.renaming_group = None;
        cx.notify();
    }

    fn save_groups(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        let Some(key) = self.groups_key.clone() else {
            return;
        };
        let Some(value) = serde_json::to_string(&self.groups).log_err() else {
            return;
        };
        let store = KeyValueStore::global(cx);
        db::write_and_log(cx, move || async move {
            store.scoped(GROUPS_NAMESPACE).write(key, value).await
        });
    }

    fn group_of(&self, session_id: &acp_v2::SessionId) -> Option<u64> {
        self.groups
            .iter()
            .find(|group| {
                group
                    .session_ids
                    .iter()
                    .any(|id| id.as_str() == session_id.0.as_ref())
            })
            .map(|group| group.id)
    }

    fn remove_from_groups(&mut self, session_id: &acp_v2::SessionId) {
        for group in &mut self.groups {
            group
                .session_ids
                .retain(|id| id.as_str() != session_id.0.as_ref());
        }
    }

    /// Creates a group, from a session when one is given, and starts naming it.
    fn create_group(
        &mut self,
        session_id: Option<acp_v2::SessionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.groups_key.is_none() {
            return;
        }
        let id = self
            .groups
            .iter()
            .map(|group| group.id)
            .max()
            .map_or(1, |id| id + 1);
        let mut session_ids = Vec::new();
        if let Some(session_id) = session_id {
            self.remove_from_groups(&session_id);
            session_ids.push(session_id.0.to_string());
        }
        self.groups.push(SessionGroup {
            id,
            name: NEW_GROUP_NAME.to_string(),
            session_ids,
            collapsed: false,
        });
        self.save_groups(cx);
        // Wait for a context menu that triggered this to close and restore
        // focus, which would end the rename right away.
        cx.defer_in(window, move |this, window, cx| {
            this.start_group_rename(id, window, cx);
        });
    }

    fn move_to_group(
        &mut self,
        session_id: &acp_v2::SessionId,
        group_id: u64,
        cx: &mut Context<Self>,
    ) {
        self.remove_from_groups(session_id);
        if let Some(group) = self.groups.iter_mut().find(|group| group.id == group_id) {
            group.session_ids.push(session_id.0.to_string());
        }
        self.save_groups(cx);
    }

    fn remove_from_group(&mut self, session_id: &acp_v2::SessionId, cx: &mut Context<Self>) {
        self.remove_from_groups(session_id);
        self.save_groups(cx);
    }

    fn delete_group(&mut self, group_id: u64, cx: &mut Context<Self>) {
        self.groups.retain(|group| group.id != group_id);
        if self
            .renaming_group
            .as_ref()
            .is_some_and(|(renaming_id, _)| *renaming_id == group_id)
        {
            self.renaming_group = None;
        }
        self.save_groups(cx);
    }

    fn toggle_group(&mut self, group_id: u64, cx: &mut Context<Self>) {
        if let Some(group) = self.groups.iter_mut().find(|group| group.id == group_id) {
            group.collapsed = !group.collapsed;
            self.save_groups(cx);
        }
    }

    fn start_group_rename(&mut self, group_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(group) = self.groups.iter().find(|group| group.id == group_id) else {
            return;
        };
        let name = group.name.clone();
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_text(name, window, cx);
            editor.select_all(&editor::actions::SelectAll, window, cx);
            editor
        });
        self._subscriptions.push(cx.subscribe_in(
            &editor,
            window,
            move |this, _, event: &EditorEvent, _, cx| {
                if matches!(event, EditorEvent::Blurred) {
                    this.finish_group_rename(group_id, true, cx);
                }
            },
        ));
        window.focus(&editor.focus_handle(cx), cx);
        self.renaming_group = Some((group_id, editor));
        cx.notify();
    }

    fn finish_group_rename(&mut self, group_id: u64, commit: bool, cx: &mut Context<Self>) {
        let Some((renaming_id, editor)) = self.renaming_group.take() else {
            return;
        };
        if renaming_id != group_id {
            self.renaming_group = Some((renaming_id, editor));
            return;
        }
        let name = editor.read(cx).text(cx).trim().to_string();
        if commit
            && !name.is_empty()
            && let Some(group) = self.groups.iter_mut().find(|group| group.id == group_id)
        {
            group.name = name;
            self.save_groups(cx);
        }
        cx.notify();
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
                        Some(PathList::new(std::slice::from_ref(&session.cwd))),
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

    fn render_status_dot(state: SessionState, cx: &App) -> impl IntoElement {
        let status_colors = cx.theme().status();
        let color = if state.status == SessionStatus::WaitingForPermission {
            status_colors.info
        } else if state.unread {
            status_colors.warning
        } else if state.is_active() {
            status_colors.success
        } else {
            cx.theme().colors().border
        };
        div().flex_none().size(px(7.)).rounded_full().bg(color)
    }

    fn render_row(&self, row: SessionRow, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let session = row.session.clone();
        let title = row.title.clone();
        let group = SharedString::from(format!("claude-code-session-{index}"));
        let renaming_editor = self
            .renaming
            .as_ref()
            .filter(|(session_id, _)| session_id == &session.session_id)
            .map(|(_, editor)| editor.clone());
        let is_renaming = renaming_editor.is_some();
        let time = row
            .session
            .updated_at
            .map(|updated_at| relative_time(Local::now(), updated_at.with_timezone(&Local)))
            .unwrap_or_default();
        let colors = cx.theme().colors();

        let row_element = h_flex()
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
            .when(row.state.is_selected, |this| {
                this.bg(colors.ghost_element_selected)
            })
            .hover(|style| style.bg(colors.ghost_element_hover))
            .child(Self::render_status_dot(row.state, cx))
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
                    .child(
                        Label::new(title.clone())
                            .size(side_bar_label_size(1., cx))
                            .weight(FontWeight::SEMIBOLD)
                            .single_line()
                            .truncate(),
                    )
                    .into_any_element(),
            })
            .child(
                h_flex()
                    .flex_none()
                    .visible_on_hover(group)
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
                    .size(side_bar_label_size(0.9, cx))
                    .color(Color::Muted)
                    .flex_none(),
            )
            .when(!is_renaming, |this| {
                this.on_click(cx.listener({
                    let session = session.clone();
                    move |this, _, window, cx| {
                        this.open_session(&session, title.clone(), window, cx);
                    }
                }))
            });

        if row.archived || self.groups_key.is_none() {
            return row_element.into_any_element();
        }
        let panel = cx.weak_entity();
        let session_id = session.session_id;
        let current_group = self.group_of(&session_id);
        let other_groups = self
            .groups
            .iter()
            .filter(|group| Some(group.id) != current_group)
            .map(|group| (group.id, SharedString::from(group.name.clone())))
            .collect::<Vec<_>>();
        right_click_menu(("claude-code-session-menu", index))
            .trigger(move |_, _, _| row_element)
            .menu(move |window, cx| {
                let panel = panel.clone();
                let session_id = session_id.clone();
                let other_groups = other_groups.clone();
                ContextMenu::build(window, cx, move |mut menu, _, _| {
                    menu = menu.entry("New group from session", None, {
                        let panel = panel.clone();
                        let session_id = session_id.clone();
                        move |window, cx| {
                            panel
                                .update(cx, |panel, cx| {
                                    panel.create_group(Some(session_id.clone()), window, cx)
                                })
                                .log_err();
                        }
                    });
                    for (group_id, name) in other_groups {
                        menu = menu.entry(format!("Move to {name}"), None, {
                            let panel = panel.clone();
                            let session_id = session_id.clone();
                            move |_, cx| {
                                panel
                                    .update(cx, |panel, cx| {
                                        panel.move_to_group(&session_id, group_id, cx)
                                    })
                                    .log_err();
                            }
                        });
                    }
                    if current_group.is_some() {
                        menu = menu.entry("Remove from group", None, {
                            let panel = panel.clone();
                            move |_, cx| {
                                panel
                                    .update(cx, |panel, cx| {
                                        panel.remove_from_group(&session_id, cx)
                                    })
                                    .log_err();
                            }
                        });
                    }
                    menu
                })
            })
            .into_any_element()
    }

    fn render_group_header(
        &self,
        group: &SessionGroup,
        session_count: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let group_id = group.id;
        let renaming_editor = self
            .renaming_group
            .as_ref()
            .filter(|(renaming_id, _)| *renaming_id == group_id)
            .map(|(_, editor)| editor.clone());
        let is_renaming = renaming_editor.is_some();
        let header = h_flex()
            .id(("claude-code-session-group", group_id))
            .debug_selector(move || format!("claude-code-session-group-{group_id}"))
            .w_full()
            .min_w_0()
            .px_3()
            .pt_2()
            .pb_1()
            .gap_1()
            .cursor_pointer()
            .child(
                Icon::new(if group.collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .size(IconSize::XSmall)
                .color(Color::Muted),
            )
            .child(match renaming_editor {
                Some(editor) => div()
                    .flex_1()
                    .min_w_0()
                    .key_context("ClaudeCodeSessionRename")
                    .on_action(cx.listener(move |this, _: &menu::Confirm, _, cx| {
                        this.finish_group_rename(group_id, true, cx)
                    }))
                    .on_action(cx.listener(move |this, _: &menu::Cancel, _, cx| {
                        this.finish_group_rename(group_id, false, cx)
                    }))
                    .child(editor)
                    .into_any_element(),
                None => h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        Label::new(group.name.clone())
                            .size(side_bar_label_size(0.9, cx))
                            .weight(FontWeight::SEMIBOLD)
                            .single_line()
                            .truncate(),
                    )
                    .child(
                        Label::new(session_count.to_string())
                            .size(side_bar_label_size(0.8, cx))
                            .color(Color::Muted),
                    )
                    .into_any_element(),
            })
            .when(!is_renaming, |this| {
                this.on_click(cx.listener(move |this, _, _, cx| this.toggle_group(group_id, cx)))
            });
        let panel = cx.weak_entity();
        right_click_menu(("claude-code-session-group-menu", group_id))
            .trigger(move |_, _, _| header)
            .menu(move |window, cx| {
                let panel = panel.clone();
                ContextMenu::build(window, cx, move |menu, _, _| {
                    menu.entry("Rename group", None, {
                        let panel = panel.clone();
                        move |window, cx| {
                            panel
                                .update(cx, |_, cx| {
                                    cx.defer_in(window, move |panel, window, cx| {
                                        panel.start_group_rename(group_id, window, cx)
                                    });
                                })
                                .log_err();
                        }
                    })
                    .entry("Delete group", None, {
                        let panel = panel.clone();
                        move |_, cx| {
                            panel
                                .update(cx, |panel, cx| panel.delete_group(group_id, cx))
                                .log_err();
                        }
                    })
                })
            })
            .into_any_element()
    }

    fn render_new_session_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("new-claude-session")
            .mx_1()
            .px_2()
            .py_1()
            .gap_2()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(cx.theme().colors().ghost_element_hover))
            .child(
                Icon::new(IconName::Plus)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .child(Label::new("New session").size(side_bar_label_size(1., cx)))
            .on_click(cx.listener(|this, _, window, cx| this.new_session(window, cx)))
    }

    /// Local and Web, like the extension's sessions list. Web lists claude.ai
    /// cloud sessions, which ACP has no way to reach.
    fn render_location_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .mx_2()
            .my_1()
            .p_0p5()
            .gap_0p5()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().colors().border_variant)
            .child(
                div().flex_1().child(
                    Button::new("claude-code-local-sessions", "Local")
                        .start_icon(Icon::new(IconName::Screen).size(IconSize::Small))
                        .label_size(side_bar_label_size(0.9, cx))
                        .full_width()
                        .toggle_state(true),
                ),
            )
            .child(
                div()
                    .id("claude-code-web-sessions")
                    .flex_1()
                    .tooltip(Tooltip::text(WEB_SESSIONS_UNAVAILABLE))
                    .child(
                        Button::new("claude-code-web-sessions-button", "Web")
                            .start_icon(Icon::new(IconName::Public).size(IconSize::Small))
                            .label_size(side_bar_label_size(0.9, cx))
                            .full_width()
                            .disabled(true),
                    ),
            )
    }

    fn render_filter_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = cx.weak_entity();
        let has_checks = self.filters.has_checks();
        PopoverMenu::new("claude-code-session-filter")
            .anchor(Anchor::TopLeft)
            .trigger_with_tooltip(
                ButtonLike::new("claude-code-session-filter-button")
                    .toggle_state(has_checks)
                    .child(
                        h_flex()
                            .gap_0p5()
                            .child(
                                Icon::new(IconName::FilterFunnel)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(
                                Icon::new(IconName::ChevronDown)
                                    .size(IconSize::XSmall)
                                    .color(Color::Muted),
                            ),
                    ),
                Tooltip::text("Filter by status"),
            )
            .menu(move |window, cx| {
                let panel = panel.clone();
                Some(ContextMenu::build_persistent(
                    window,
                    cx,
                    move |mut menu, _, cx| {
                        let Some(filters) = panel.upgrade().map(|panel| panel.read(cx).filters)
                        else {
                            return menu;
                        };
                        let menu_entity = cx.weak_entity();
                        for (index, check) in FilterCheck::ALL.into_iter().enumerate() {
                            if index == 3 {
                                menu = menu.separator();
                            }
                            let panel = panel.clone();
                            let menu_entity = menu_entity.clone();
                            menu = menu.toggleable_entry(
                                check.label(),
                                filters.check(check),
                                IconPosition::Start,
                                None,
                                move |window, cx| {
                                    panel
                                        .update(cx, |panel, cx| {
                                            let mut filters = panel.filters;
                                            filters.toggle_check(check);
                                            panel.set_filters(filters, cx);
                                        })
                                        .log_err();
                                    let menu_entity = menu_entity.clone();
                                    window.defer(cx, move |window, cx| {
                                        menu_entity
                                            .update(cx, |menu, cx| menu.rebuild(window, cx))
                                            .log_err();
                                    });
                                },
                            );
                        }
                        menu
                    },
                ))
            })
    }

    fn render_toolbar(&self, active_count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .px_2()
            .py_1()
            .gap_1()
            .child(self.render_filter_menu(cx))
            .child(
                Button::new(
                    "claude-code-active-filter",
                    format!("Active · {active_count}"),
                )
                .start_icon(
                    Icon::new(IconName::BoltOutlined)
                        .size(IconSize::Small)
                        .color(Color::Muted),
                )
                .label_size(side_bar_label_size(0.9, cx))
                .toggle_state(self.filters.active)
                .tooltip(Tooltip::text(
                    "Show sessions that need input, are working or are unread",
                ))
                .on_click(cx.listener(|this, _, _, cx| {
                    let mut filters = this.filters;
                    filters.active = !filters.active;
                    this.set_filters(filters, cx);
                })),
            )
            .child(
                IconButton::new("claude-code-search-sessions", IconName::MagnifyingGlass)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .toggle_state(self.search_visible)
                    .tooltip(Tooltip::text("Search sessions"))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_search(window, cx))),
            )
            .when(self.groups_key.is_some(), |this| {
                this.child(
                    Button::new("claude-code-new-group", "New group")
                        .start_icon(
                            Icon::new(IconName::Plus)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                        .label_size(side_bar_label_size(0.9, cx))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.create_group(None, window, cx)),
                        ),
                )
            })
            .child(div().flex_1())
            .child(
                IconButton::new("refresh-claude-sessions", IconName::RotateCw)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Refresh"))
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
            )
    }
}

fn groups_key(roots: &[PathBuf]) -> Option<String> {
    if roots.is_empty() {
        return None;
    }
    let mut paths = roots
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    paths.sort();
    Some(paths.join("\n"))
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
                worktree_paths: project::WorktreePaths::from_folder_paths(&PathList::new(
                    std::slice::from_ref(&session.cwd),
                )),
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
            // Docks activate panels while the workspace is being updated, and
            // `refresh` reads the workspace.
            self.has_requested_sessions = true;
            cx.defer_in(window, |this, window, cx| this.refresh(window, cx));
        }
    }
}

impl Render for ClaudeCodeSessionsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Listing starts the Claude agent, so wait until the panel is first shown.
        if !self.has_requested_sessions {
            self.has_requested_sessions = true;
            cx.defer_in(window, |this, window, cx| this.refresh(window, cx));
        }
        if self.last_focused_session.is_none()
            && let Some(workspace) = self.workspace.upgrade()
        {
            self.track_focused_tab(&workspace, cx);
        }
        let rows = self.rows(cx);
        let active_count = rows
            .iter()
            .filter(|row| !row.archived && row.state.is_active())
            .count();
        let filters = self.filters;
        let is_filtering = filters.is_filtering();
        let is_searching = !self.search_query(cx).is_empty();
        let (archived, unarchived): (Vec<_>, Vec<_>) =
            rows.into_iter().partition(|row| row.archived);
        let visible = unarchived
            .into_iter()
            .filter(|row| filters.matches(&row.state))
            .collect::<Vec<_>>();
        // Archived sessions are hidden while a filter is on.
        let archived = if is_filtering { Vec::new() } else { archived };
        let archived_count = archived.len();
        let has_visible_rows = !visible.is_empty();
        let mut index = 0;
        let mut next_index = || {
            index += 1;
            index
        };

        // A search shows its matches in one flat list across all groups.
        let mut grouped: Vec<(SessionGroup, Vec<SessionRow>)> = Vec::new();
        let mut ungrouped = Vec::new();
        if is_searching {
            ungrouped = visible;
        } else {
            grouped = self
                .groups
                .iter()
                .map(|group| (group.clone(), Vec::new()))
                .collect();
            for row in visible {
                let group_index = grouped.iter().position(|(group, _)| {
                    group
                        .session_ids
                        .iter()
                        .any(|id| id.as_str() == row.session.session_id.0.as_ref())
                });
                match group_index.and_then(|group_index| grouped.get_mut(group_index)) {
                    Some((_, group_rows)) => group_rows.push(row),
                    None => ungrouped.push(row),
                }
            }
            if is_filtering {
                grouped.retain(|(_, group_rows)| !group_rows.is_empty());
            }
        }

        let empty_text: Option<SharedString> = match &self.load_state {
            LoadState::Loading if !has_visible_rows => Some("Loading sessions…".into()),
            LoadState::Unsupported => Some("This Claude agent can't list its sessions.".into()),
            LoadState::Error(error) => Some(format!("Couldn't list sessions: {error}").into()),
            _ if !has_visible_rows && (is_searching || is_filtering) => {
                Some("No matching sessions.".into())
            }
            _ if !has_visible_rows && grouped.is_empty() => {
                Some("No sessions in this project yet.".into())
            }
            _ => None,
        };

        let usage_windows = self.usage_windows(cx);
        self.schedule_usage_refresh(&usage_windows, cx);

        let mut list_children: Vec<AnyElement> = Vec::new();
        for (group, group_rows) in grouped {
            list_children.push(self.render_group_header(&group, group_rows.len(), cx));
            if !group.collapsed {
                for row in group_rows {
                    let row_index = next_index();
                    list_children.push(self.render_row(row, row_index, cx));
                }
            }
        }
        for row in ungrouped {
            let row_index = next_index();
            list_children.push(self.render_row(row, row_index, cx));
        }
        if archived_count > 0 {
            let show_archived = self.show_archived;
            list_children.push(
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
                            .size(side_bar_label_size(0.9, cx))
                            .color(Color::Muted),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_archived = !this.show_archived;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            if show_archived {
                for row in archived {
                    let row_index = next_index();
                    list_children.push(self.render_row(row, row_index, cx));
                }
            }
        }

        v_flex()
            .id("claude-code-sessions-panel")
            .key_context("ClaudeCodeSessionsPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().panel_background)
            // No title row of its own: a dock header, when there is one, already names the panel.
            .child(self.render_section_header(Section::AccountAndUsage, cx))
            .when(self.account_and_usage_expanded, |this| {
                this.child(usage::render_account_and_usage(
                    usage::account_email(cx),
                    &usage_windows,
                    cx,
                ))
            })
            .child(
                self.render_section_header(Section::SessionManager, cx)
                    .border_t_1()
                    .border_color(cx.theme().colors().border_variant),
            )
            .when(self.session_manager_expanded, |this| {
                this.child(
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .child(self.render_new_session_row(cx))
                        .child(self.render_location_toggle(cx))
                        .child(self.render_toolbar(active_count, cx))
                        .when(self.search_visible, |this| {
                            this.child(
                                div()
                                    .mx_2()
                                    .mb_1()
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(cx.theme().colors().border)
                                    .key_context("ClaudeCodeSessionSearch")
                                    .on_action(cx.listener(|this, _: &menu::Cancel, window, cx| {
                                        this.toggle_search(window, cx)
                                    }))
                                    .child(self.search_editor.clone()),
                            )
                        })
                        .child(
                            v_flex()
                                .id("claude-code-session-list")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .px_1()
                                .when_some(empty_text, |this, text| {
                                    this.child(div().px_3().py_2().child(
                                        Label::new(text)
                                            .size(side_bar_label_size(0.9, cx))
                                            .color(Color::Muted),
                                    ))
                                })
                                .children(list_children),
                        ),
                )
            })
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
    use crate::test_support::{active_session_id, open_thread_with_connection};
    use acp_thread::{AgentSessionListResponse, StubAgentConnection};
    use agent_client_protocol::v1::schema as acp_v1;
    use chrono::TimeZone as _;
    use fs::FakeFs;
    use gpui::{TestAppContext, VisualTestContext};
    use serde_json::json;
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
    async fn test_refresh_before_the_agent_panel_loads(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| {
            agent::ThreadStore::init_global(cx);
            crate::ui_styles::init(cx);
            gpui::UpdateGlobal::update_global(cx, |store: &mut settings::SettingsStore, cx| {
                store
                    .set_user_settings(r#"{ "agent": { "ui_style": "claude_code" } }"#, cx)
                    .expect("valid settings");
            });
        });
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let multi_workspace =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace
            .read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone())
            .expect("window is open");
        let cx = &mut VisualTestContext::from_window(multi_workspace.into(), cx);
        let sessions_panel = workspace
            .read_with(cx, |workspace, cx| {
                assert!(workspace.panel::<AgentPanel>(cx).is_none());
                workspace.panel::<ClaudeCodeSessionsPanel>(cx)
            })
            .expect("the Claude Code style adds the sessions panel");

        sessions_panel.update_in(cx, |panel, window, cx| {
            let server = Rc::new(crate::conversation_view::tests::StubAgentServer::new(
                StubAgentConnection::new(),
            ));
            panel.refresh_with_server(Agent::Stub, server, window, cx);
        });
        cx.run_until_parked();

        // The stub connection has no session list, so a connection that survived
        // the refresh ends up unsupported rather than in an error.
        sessions_panel.read_with(cx, |panel, _| {
            assert_eq!(panel.load_state, LoadState::Unsupported);
        });
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

    #[gpui::test]
    async fn test_toggling_the_panel_focuses_it(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| {
            crate::ui_styles::init(cx);
            gpui::UpdateGlobal::update_global(cx, |store: &mut settings::SettingsStore, cx| {
                store
                    .set_user_settings(r#"{ "agent": { "ui_style": "claude_code" } }"#, cx)
                    .expect("valid settings");
            });
        });
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());

        // The activity bar and the keybinding toggle the panel from inside a
        // workspace update, which activates the panel.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_panel_focus::<ClaudeCodeSessionsPanel>(window, cx);
        });
        cx.run_until_parked();

        let panel_is_active = workspace.read_with(cx, |workspace, cx| {
            workspace
                .left_dock()
                .read(cx)
                .visible_panel()
                .is_some_and(|panel| panel.panel_key() == ClaudeCodeSessionsPanel::panel_key())
        });
        assert!(panel_is_active);
    }

    #[gpui::test]
    async fn test_rate_limit_usage_update_reaches_the_sessions_panel(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| {
            agent::ThreadStore::init_global(cx);
            language_model::LanguageModelRegistry::test(cx);
            crate::ui_styles::init(cx);
            gpui::UpdateGlobal::update_global(cx, |store: &mut settings::SettingsStore, cx| {
                store
                    .set_user_settings(r#"{ "agent": { "ui_style": "claude_code" } }"#, cx)
                    .expect("valid settings");
            });
        });
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let multi_workspace =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace
            .read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone())
            .expect("window is open");
        let cx = &mut VisualTestContext::from_window(multi_workspace.into(), cx);
        cx.simulate_resize(gpui::size(px(1200.), px(800.)));

        let sessions_panel = workspace
            .read_with(cx, |workspace, cx| {
                workspace.panel::<ClaudeCodeSessionsPanel>(cx)
            })
            .expect("the Claude Code style adds the sessions panel");
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_panel_focus::<ClaudeCodeSessionsPanel>(window, cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("claude-code-usage-empty").is_some());

        let connection = StubAgentConnection::new();
        let agent_panel = workspace.update_in(cx, |workspace, window, cx| {
            let panel = cx.new(|cx| AgentPanel::new(workspace, window, cx));
            workspace.add_panel(panel.clone(), window, cx);
            panel
        });
        open_thread_with_connection(&agent_panel, connection.clone(), cx);
        let session_id = active_session_id(&agent_panel, cx);

        let resets_at = Utc::now().timestamp() + 3 * 24 * 60 * 60;
        let usage_updates = [
            json!({
                "sessionUpdate": "usage_update", "used": 1_000, "size": 200_000,
                "_meta": {
                    "_claude/model": "claude-opus-5-5",
                    "_claude/rateLimit": {
                        "status": "allowed_warning",
                        "rateLimitType": "seven_day",
                        "utilization": 0.81,
                        "resetsAt": resets_at,
                        "unifiedWindows": {
                            "five_hour": { "utilization": 0.14, "resetsAt": resets_at - 3_600 },
                            "seven_day": { "utilization": 0.81, "resetsAt": resets_at },
                        },
                    },
                },
            }),
            // Later usage updates carry only the model.
            json!({
                "sessionUpdate": "usage_update", "used": 2_000, "size": 200_000,
                "_meta": { "_claude/model": "claude-opus-5-5" },
            }),
        ];
        for update in usage_updates {
            let update: acp_v1::SessionUpdate =
                serde_json::from_value(update).expect("valid usage update");
            cx.update(|_, cx| connection.send_update(session_id.clone(), update, cx));
            cx.run_until_parked();
        }

        let windows = sessions_panel.read_with(cx, |panel, cx| panel.usage_windows(cx));
        let rows = windows
            .iter()
            .map(|window| {
                (
                    window.limit_type.label(),
                    window.utilization.map(usage::format_percent),
                    window.status,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            vec![
                (
                    SharedString::from("Session (5 hour)"),
                    Some("14%".to_string()),
                    None
                ),
                (
                    SharedString::from("Weekly (7 day)"),
                    Some("81%".to_string()),
                    Some(usage::RateLimitStatus::Warning)
                ),
            ]
        );
        assert!(cx.debug_bounds("claude-code-usage-seven_day").is_some());
        assert!(cx.debug_bounds("claude-code-usage-five_hour").is_some());
        assert!(cx.debug_bounds("claude-code-usage-empty").is_none());
    }

    #[test]
    fn test_session_filters() {
        let state = |status, is_open, unread, is_selected| SessionState {
            status,
            is_open,
            unread,
            is_selected,
        };
        let working = state(SessionStatus::Running, true, false, false);
        let needs_input = state(SessionStatus::WaitingForPermission, true, false, false);
        let unread = state(SessionStatus::Idle, true, true, false);
        let selected = state(SessionStatus::Idle, true, false, true);
        let open_idle = state(SessionStatus::Idle, true, false, false);
        let closed = state(SessionStatus::Idle, false, false, false);
        let all = [working, needs_input, unread, selected, open_idle, closed];
        let matching = |filters: SessionFilters| {
            all.iter()
                .map(|state| filters.matches(state))
                .collect::<Vec<_>>()
        };

        assert_eq!(matching(SessionFilters::default()), [true; 6]);
        assert!(!SessionFilters::default().is_filtering());

        let active = SessionFilters {
            active: true,
            ..Default::default()
        };
        assert_eq!(matching(active), [true, true, true, true, false, false]);

        let working_or_completed = SessionFilters {
            working: true,
            completed: true,
            ..Default::default()
        };
        assert_eq!(
            matching(working_or_completed),
            [true, false, true, true, true, true]
        );

        // Open and Closed narrow the status checks.
        let completed_and_closed = SessionFilters {
            completed: true,
            closed: true,
            ..Default::default()
        };
        assert_eq!(
            matching(completed_and_closed),
            [false, false, false, false, false, true]
        );

        // With Active on, the checks add their matches.
        let active_or_closed = SessionFilters {
            active: true,
            closed: true,
            ..Default::default()
        };
        assert_eq!(
            matching(active_or_closed),
            [true, true, true, true, false, true]
        );

        let mut toggled = SessionFilters::default();
        toggled.toggle_check(FilterCheck::NeedsInput);
        assert!(toggled.check(FilterCheck::NeedsInput) && toggled.is_filtering());
        let saved = serde_json::to_string(&toggled).expect("filters serialize");
        assert_eq!(
            serde_json::from_str::<SessionFilters>(&saved).expect("filters deserialize"),
            toggled
        );
        assert_eq!(
            serde_json::from_str::<SessionFilters>(r#"{ "active": true, "future": 1 }"#)
                .expect("lenient filters"),
            active
        );
    }

    #[gpui::test]
    async fn test_session_groups_search_and_filters(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| {
            // Filters and groups are saved; keep them out of other tests.
            cx.set_global(db::AppDatabase::test_new());
            crate::ui_styles::init(cx);
            gpui::UpdateGlobal::update_global(cx, |store: &mut settings::SettingsStore, cx| {
                store
                    .set_user_settings(r#"{ "agent": { "ui_style": "claude_code" } }"#, cx)
                    .expect("valid settings");
            });
        });
        let root = "/work/claude-session-groups";
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(root, json!({ "README.md": "" })).await;
        let project = Project::test(fs, [Path::new(root)], cx).await;
        let multi_workspace =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace
            .read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone())
            .expect("window is open");
        let cx = &mut VisualTestContext::from_window(multi_workspace.into(), cx);
        cx.simulate_resize(gpui::size(px(1200.), px(800.)));
        let panel = workspace
            .read_with(cx, |workspace, cx| {
                workspace.panel::<ClaudeCodeSessionsPanel>(cx)
            })
            .expect("the Claude Code style adds the sessions panel");
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_panel_focus::<ClaudeCodeSessionsPanel>(window, cx);
        });
        cx.run_until_parked();

        let listed = |id: &str, minutes_ago: i64| ListedSession {
            session_id: acp_v2::SessionId::new(id.to_string()),
            cwd: PathBuf::from(root),
            title: Some(format!("Session {id}").into()),
            updated_at: Some(Utc::now() - chrono::Duration::minutes(minutes_ago)),
        };
        panel.update(cx, |panel, cx| {
            panel.sessions = vec![listed("a", 1), listed("b", 2), listed("c", 3)];
            panel.load_state = LoadState::Loaded;
            cx.notify();
        });
        cx.run_until_parked();
        for selector in [
            "claude-code-session-a",
            "claude-code-session-b",
            "claude-code-session-c",
        ] {
            assert!(cx.debug_bounds(selector).is_some());
        }

        let session_a = acp_v2::SessionId::new("a");
        panel.update_in(cx, |panel, window, cx| {
            panel.create_group(Some(session_a.clone()), window, cx)
        });
        cx.run_until_parked();
        let group_id = panel.read_with(cx, |panel, _| {
            let group = panel.groups.first().expect("a group");
            assert_eq!(group.name, NEW_GROUP_NAME);
            assert_eq!(group.session_ids, ["a"]);
            assert!(
                panel.renaming_group.is_some(),
                "a new group starts renaming"
            );
            group.id
        });
        assert_eq!(group_id, 1);
        let editor = panel.read_with(cx, |panel, _| {
            panel
                .renaming_group
                .as_ref()
                .map(|(_, editor)| editor.clone())
                .expect("rename editor")
        });
        editor.update_in(cx, |editor, window, cx| {
            editor.set_text("Reviews", window, cx)
        });
        panel.update(cx, |panel, cx| {
            panel.finish_group_rename(group_id, true, cx)
        });
        cx.run_until_parked();
        let header = cx
            .debug_bounds("claude-code-session-group-1")
            .expect("group header");
        let grouped_row = cx
            .debug_bounds("claude-code-session-a")
            .expect("grouped session");
        let ungrouped_row = cx
            .debug_bounds("claude-code-session-b")
            .expect("ungrouped session");
        assert!(header.top() < grouped_row.top() && grouped_row.top() < ungrouped_row.top());

        panel.update(cx, |panel, cx| {
            panel.move_to_group(&acp_v2::SessionId::new("b"), group_id, cx);
            panel.toggle_group(group_id, cx);
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("claude-code-session-a").is_none(),
            "collapsed"
        );
        assert!(
            cx.debug_bounds("claude-code-session-b").is_none(),
            "collapsed"
        );
        assert!(cx.debug_bounds("claude-code-session-c").is_some());

        // A search lists its matches flat, across groups.
        panel.update_in(cx, |panel, window, cx| {
            panel.toggle_search(window, cx);
            panel
                .search_editor
                .update(cx, |editor, cx| editor.set_text("session a", window, cx));
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("claude-code-session-group-1").is_none());
        assert!(cx.debug_bounds("claude-code-session-a").is_some());
        assert!(cx.debug_bounds("claude-code-session-c").is_none());
        panel.update_in(cx, |panel, window, cx| panel.toggle_search(window, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("claude-code-session-c").is_some());

        // No session has a tab, so none is active.
        panel.update(cx, |panel, cx| {
            panel.set_filters(
                SessionFilters {
                    active: true,
                    ..Default::default()
                },
                cx,
            )
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("claude-code-session-c").is_none());
        panel.update(cx, |panel, cx| {
            panel.set_filters(SessionFilters::default(), cx)
        });

        // Groups are saved for the project's folders.
        panel.update(cx, |panel, cx| {
            panel.remove_from_group(&session_a, cx);
        });
        cx.run_until_parked();
        let reloaded = workspace.update_in(cx, |workspace, window, cx| {
            cx.new(|cx| ClaudeCodeSessionsPanel::new(workspace, window, cx))
        });
        let groups = reloaded.read_with(cx, |panel, _| panel.groups.clone());
        assert_eq!(
            groups,
            vec![SessionGroup {
                id: group_id,
                name: "Reviews".to_string(),
                session_ids: vec!["b".to_string()],
                collapsed: true,
            }]
        );

        panel.update(cx, |panel, cx| panel.delete_group(group_id, cx));
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| panel.groups.is_empty()));
        assert!(cx.debug_bounds("claude-code-session-b").is_some());
    }

    #[test]
    fn test_groups_key() {
        assert_eq!(groups_key(&[]), None);
        assert_eq!(
            groups_key(&[PathBuf::from("/b"), PathBuf::from("/a")]).as_deref(),
            Some("/a\n/b")
        );
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
