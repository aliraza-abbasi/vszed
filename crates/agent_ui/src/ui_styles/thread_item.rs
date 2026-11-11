use std::path::PathBuf;

use acp_thread::{AcpThread, AcpThreadEvent};
use agent::ThreadStore;
use agent_client_protocol::v2::schema as acp_v2;
use anyhow::{Context as _, Result};
use collections::HashMap;
use db::kvp::KeyValueStore;
use gpui::{
    Action, AnyElement, App, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable,
    Global, PromptLevel, Subscription, Task, WeakEntity, Window, actions,
};
use project::Project;
use serde::{Deserialize, Serialize};
use settings::Settings as _;
use ui::prelude::*;
use util::ResultExt as _;
use workspace::{
    CloseActiveItem, Item, ItemId, PathList, SaveIntent, SerializableItem, Workspace, WorkspaceId,
    item::{ItemEvent, TabContentParams},
};

use super::AgentUiStyleSettings;
use crate::agent_connection_store::AgentConnectionStore;
use crate::agent_panel::AgentPanel;
use crate::conversation_view::RootThreadUpdated;
use crate::thread_metadata_store::{ThreadId, ThreadMetadataStore};
use crate::{Agent, AgentThreadSource, ConversationView};

actions!(
    agent,
    [
        /// Moves focus between the editor and the agent thread tab, opening a
        /// thread tab if there is none.
        ToggleThreadTabFocus,
        /// Starts a new agent thread in a new editor tab.
        NewThreadTab,
    ]
);

const SERIALIZATION_NAMESPACE: &str = "agent_thread_items";
const STATUS_DOT_SIZE: Pixels = px(6.);

/// Whether `agent.thread_location` asks for threads in editor tabs.
pub(crate) fn threads_open_in_editor(cx: &App) -> bool {
    AgentUiStyleSettings::get_global(cx).thread_location == settings::AgentThreadLocation::Editor
}

/// What to show in a thread tab: an existing session to resume, a Zed thread
/// to restore, or a new thread when both are empty.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ThreadTabRequest {
    pub agent: Agent,
    pub session_id: Option<acp_v2::SessionId>,
    pub thread_id: Option<ThreadId>,
    pub work_dirs: Option<Vec<PathBuf>>,
    pub title: Option<SharedString>,
}

impl ThreadTabRequest {
    fn work_dir_list(&self) -> Option<PathList> {
        self.work_dirs
            .as_ref()
            .filter(|paths| !paths.is_empty())
            .map(|paths| PathList::new(paths))
    }
}

/// Connection stores for projects whose agent panel isn't loaded yet (tabs
/// restored at startup). Shared so a project starts each agent once.
#[derive(Default)]
struct FallbackConnectionStores(HashMap<EntityId, WeakEntity<AgentConnectionStore>>);

impl Global for FallbackConnectionStores {}

/// The global thread store, initialized on first use (tests and headless
/// setups don't initialize it up front).
pub(crate) fn global_thread_store(cx: &mut App) -> Entity<ThreadStore> {
    if let Some(store) = ThreadStore::try_global(cx) {
        return store;
    }
    ThreadStore::init_global(cx);
    ThreadStore::global(cx)
}

pub(crate) fn panel_connection_store(
    workspace: &Workspace,
    cx: &App,
) -> Option<Entity<AgentConnectionStore>> {
    workspace
        .panel::<AgentPanel>(cx)
        .map(|panel| panel.read(cx).connection_store().clone())
}

/// The agent panel's connection store, so tabs and the panel share agent
/// processes, or a per-project fallback while the panel isn't loaded.
pub(crate) fn connection_store(
    panel_store: Option<Entity<AgentConnectionStore>>,
    project: &Entity<Project>,
    cx: &mut App,
) -> Entity<AgentConnectionStore> {
    if let Some(store) = panel_store {
        return store;
    }
    let project_id = project.entity_id();
    if let Some(store) = cx
        .default_global::<FallbackConnectionStores>()
        .0
        .get(&project_id)
        .and_then(WeakEntity::upgrade)
    {
        return store;
    }
    let store = cx.new(|cx| AgentConnectionStore::new(project.clone(), cx));
    cx.default_global::<FallbackConnectionStores>()
        .0
        .insert(project_id, store.downgrade());
    store
}

/// An agent thread hosted as an editor tab (`agent.thread_location: "editor"`).
pub(crate) struct AgentThreadItem {
    conversation_view: Entity<ConversationView>,
    request: ThreadTabRequest,
    workspace: WeakEntity<Workspace>,
    last_title: Option<SharedString>,
    finished_while_hidden: bool,
    _root_thread_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl AgentThreadItem {
    pub(crate) fn new(
        request: ThreadTabRequest,
        workspace: &Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let project = workspace.project().clone();
        let fs = project.read(cx).fs().clone();
        let thread_store = global_thread_store(cx);
        let server = request.agent.server(fs, thread_store.clone());
        let native_thread_store =
            matches!(request.agent, Agent::NativeAgent).then_some(thread_store);
        let connection_store =
            connection_store(panel_connection_store(workspace, cx), &project, cx);
        let thread_id = request.thread_id.or_else(|| {
            let session_id = request.session_id.as_ref()?;
            ThreadMetadataStore::try_global(cx)?
                .read(cx)
                .entry_by_session(session_id)
                .map(|metadata| metadata.thread_id)
        });
        let workspace_handle = workspace.weak_handle();
        let conversation_view = cx.new(|cx| {
            ConversationView::new(
                server,
                connection_store,
                request.agent.clone(),
                request.session_id.clone(),
                thread_id,
                request.work_dir_list(),
                request.title.clone(),
                None,
                workspace_handle.clone(),
                project,
                native_thread_store,
                AgentThreadSource::AgentPanel,
                window,
                cx,
            )
        });

        let subscriptions = vec![
            // The conversation view notifies on every streamed update; only a
            // title change needs the tab (and its saved state) updated.
            cx.observe(&conversation_view, |this, conversation_view, cx| {
                let title = conversation_view.read(cx).title(cx);
                if this.last_title.as_ref() != Some(&title) {
                    this.last_title = Some(title);
                    cx.emit(ItemEvent::UpdateTab);
                }
            }),
            cx.subscribe_in(
                &conversation_view,
                window,
                |this, _, _: &RootThreadUpdated, window, cx| {
                    this.observe_root_thread(window, cx);
                },
            ),
        ];

        let mut this = Self {
            conversation_view,
            request: ThreadTabRequest {
                thread_id,
                ..request
            },
            workspace: workspace_handle,
            last_title: None,
            finished_while_hidden: false,
            _root_thread_subscription: None,
            _subscriptions: subscriptions,
        };
        this.observe_root_thread(window, cx);
        this
    }

    fn root_thread(&self, cx: &App) -> Option<Entity<AcpThread>> {
        self.conversation_view.read(cx).root_thread(cx)
    }

    pub(crate) fn session_id(&self, cx: &App) -> Option<acp_v2::SessionId> {
        self.root_thread(cx)
            .map(|thread| thread.read(cx).session_id().clone())
            .or_else(|| self.request.session_id.clone())
    }

    pub(crate) fn thread_id(&self, cx: &App) -> ThreadId {
        self.conversation_view.read(cx).parent_id()
    }

    pub(crate) fn is_generating(&self, cx: &App) -> bool {
        self.root_thread(cx).is_some_and(|thread| {
            matches!(
                thread.read(cx).status(),
                acp_thread::ThreadStatus::Generating | acp_thread::ThreadStatus::Unconfirmed
            )
        })
    }

    pub(crate) fn is_waiting_for_permission(&self, cx: &App) -> bool {
        self.conversation_view
            .read(cx)
            .root_thread_is_waiting_for_input(cx)
    }

    fn observe_root_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.root_thread(cx) else {
            return;
        };
        if let Some(session_id) = self.session_id(cx) {
            self.request.session_id = Some(session_id);
        }
        self.request.thread_id = Some(self.thread_id(cx));
        self._root_thread_subscription = Some(cx.subscribe_in(
            &thread,
            window,
            |this, _, event: &AcpThreadEvent, window, cx| match event {
                AcpThreadEvent::Stopped { .. } => {
                    if !this.is_visible(window, cx) {
                        this.finished_while_hidden = true;
                    }
                    cx.emit(ItemEvent::UpdateTab);
                    cx.notify();
                }
                AcpThreadEvent::TitleUpdated
                | AcpThreadEvent::StatusChanged
                | AcpThreadEvent::ToolAuthorizationRequested(_)
                | AcpThreadEvent::ToolAuthorizationReceived(_) => {
                    cx.emit(ItemEvent::UpdateTab);
                    cx.notify();
                }
                _ => {}
            },
        ));
        cx.emit(ItemEvent::UpdateTab);
    }

    /// Whether this tab is the active item of one of the workspace's panes in
    /// the active window.
    fn is_visible(&self, window: &Window, cx: &Context<Self>) -> bool {
        if !window.is_window_active() {
            return false;
        }
        let item_id = cx.entity_id();
        self.workspace.upgrade().is_some_and(|workspace| {
            workspace.read(cx).panes().iter().any(|pane| {
                pane.read(cx)
                    .active_item()
                    .is_some_and(|item| item.item_id() == item_id)
            })
        })
    }

    fn agent_icon(&self, cx: &App) -> Icon {
        let thread_view = self.conversation_view.read(cx).root_thread_view();
        match thread_view {
            Some(thread_view) => {
                let thread_view = thread_view.read(cx);
                match thread_view.agent_icon_from_external_svg.clone() {
                    Some(svg) => Icon::from_external_svg(svg),
                    None => Icon::new(thread_view.agent_icon),
                }
            }
            None if self.request.agent.id().as_ref() == agent_servers::CLAUDE_AGENT_ID => {
                Icon::new(IconName::AiClaude)
            }
            None => Icon::new(IconName::ZedAgent),
        }
    }

    /// Closing a tab while the agent is working stops the session, so ask first.
    fn close_with_confirmation(
        &mut self,
        action: &CloseActiveItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_generating(cx) {
            cx.propagate();
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            "The agent is still working in this session.",
            Some("Closing the tab stops the current turn."),
            &["Close Anyway", "Cancel"],
            cx,
        );
        let item_id = cx.entity_id();
        let workspace = self.workspace.clone();
        let save_intent = action.save_intent.unwrap_or(SaveIntent::Skip);
        cx.spawn_in(window, async move |_, cx| {
            if answer.await != Ok(0) {
                return anyhow::Ok(());
            }
            workspace.update_in(cx, |workspace, window, cx| {
                let pane = workspace
                    .panes()
                    .iter()
                    .find(|pane| pane.read(cx).items().any(|item| item.item_id() == item_id))
                    .cloned();
                if let Some(pane) = pane {
                    pane.update(cx, |pane, cx| {
                        pane.close_item_by_id(item_id, save_intent, window, cx)
                    })
                    .detach_and_log_err(cx);
                }
            })
        })
        .detach_and_log_err(cx);
    }
}

impl EventEmitter<ItemEvent> for AgentThreadItem {}

impl Focusable for AgentThreadItem {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.conversation_view.read(cx).focus_handle(cx)
    }
}

impl Render for AgentThreadItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Rendering means the tab is the visible one in its pane.
        if self.finished_while_hidden {
            self.finished_while_hidden = false;
            cx.emit(ItemEvent::UpdateTab);
        }
        v_flex()
            .key_context("AgentThreadItem")
            .capture_action(cx.listener(Self::close_with_confirmation))
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .child(self.conversation_view.clone())
    }
}

impl Item for AgentThreadItem {
    type Event = ItemEvent;

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        self.conversation_view.read(cx).title(cx)
    }

    fn tab_tooltip_text(&self, cx: &App) -> Option<SharedString> {
        Some(self.conversation_view.read(cx).title(cx))
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        let status_color = if self.is_waiting_for_permission(cx) {
            Some(cx.theme().status().info)
        } else if self.finished_while_hidden {
            Some(cx.theme().status().warning)
        } else {
            None
        };
        let icon = self
            .agent_icon(cx)
            .size(IconSize::Small)
            .color(params.text_color());
        h_flex()
            .gap_1p5()
            .child(
                div()
                    .relative()
                    .child(icon)
                    .when_some(status_color, |this, color| {
                        this.child(
                            div()
                                .absolute()
                                .top_neg_0p5()
                                .right_neg_0p5()
                                .size(STATUS_DOT_SIZE)
                                .rounded_full()
                                .border_1()
                                .border_color(cx.theme().colors().tab_bar_background)
                                .bg(color),
                        )
                    }),
            )
            .child(
                Label::new(self.tab_content_text(params.detail.unwrap_or_default(), cx))
                    .color(params.text_color())
                    .single_line()
                    .truncate(),
            )
            .into_any_element()
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        None
    }

    fn include_in_nav_history() -> bool {
        false
    }

    fn tab_extra_context_menu_actions(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Vec<(SharedString, Box<dyn Action>)> {
        vec![("New Thread Tab".into(), NewThreadTab.boxed_clone())]
    }
}

impl SerializableItem for AgentThreadItem {
    fn serialized_item_kind() -> &'static str {
        "AgentThreadItem"
    }

    fn cleanup(
        _workspace_id: WorkspaceId,
        _alive_items: Vec<ItemId>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<Result<()>> {
        // The workspace only deserializes the items it lists, so entries of
        // closed tabs are never read again.
        Task::ready(Ok(()))
    }

    fn deserialize(
        _project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        workspace_id: WorkspaceId,
        item_id: ItemId,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Entity<Self>>> {
        let key = serialization_key(workspace_id, item_id);
        let request = KeyValueStore::global(cx)
            .scoped(SERIALIZATION_NAMESPACE)
            .read(&key)
            .and_then(|value| value.context("no saved agent thread tab"))
            .and_then(|value| {
                serde_json::from_str::<ThreadTabRequest>(&value)
                    .context("invalid saved agent thread tab")
            });
        let request = match request {
            Ok(request) => request,
            Err(error) => return Task::ready(Err(error)),
        };
        window.spawn(cx, async move |cx| {
            workspace.update_in(cx, |workspace, window, cx| {
                cx.new(|cx| AgentThreadItem::new(request, workspace, window, cx))
            })
        })
    }

    fn serialize(
        &mut self,
        workspace: &mut Workspace,
        item_id: ItemId,
        _closing: bool,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        let workspace_id = workspace.database_id()?;
        let request = ThreadTabRequest {
            session_id: self.session_id(cx),
            thread_id: Some(self.thread_id(cx)),
            title: Some(self.conversation_view.read(cx).title(cx)),
            ..self.request.clone()
        };
        let value = serde_json::to_string(&request).log_err()?;
        let key = serialization_key(workspace_id, item_id);
        let store = KeyValueStore::global(cx);
        Some(cx.background_spawn(async move {
            store
                .scoped(SERIALIZATION_NAMESPACE)
                .write(key, value)
                .await
        }))
    }

    fn should_serialize(&self, event: &Self::Event) -> bool {
        matches!(event, ItemEvent::UpdateTab)
    }
}

fn serialization_key(workspace_id: WorkspaceId, item_id: ItemId) -> String {
    format!("{}:{item_id}", i64::from(workspace_id))
}

fn find_thread_tab(
    workspace: &Workspace,
    cx: &App,
    predicate: impl Fn(&AgentThreadItem, &App) -> bool,
) -> Option<Entity<AgentThreadItem>> {
    workspace
        .items_of_type::<AgentThreadItem>(cx)
        .find(|item| predicate(item.read(cx), cx))
}

/// Opens `request` as an editor tab, or focuses the tab that already shows
/// that session or thread.
pub(crate) fn open_thread_tab(
    workspace: &mut Workspace,
    request: ThreadTabRequest,
    focus: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Entity<AgentThreadItem> {
    let existing = find_thread_tab(workspace, cx, |item, cx| {
        let same_session = request.session_id.is_some()
            && item.session_id(cx).as_ref() == request.session_id.as_ref();
        let same_thread = request
            .thread_id
            .is_some_and(|thread_id| item.thread_id(cx) == thread_id);
        same_session || same_thread
    });
    if let Some(existing) = existing {
        workspace.activate_item(&existing, true, focus, window, cx);
        return existing;
    }
    let item = cx.new(|cx| AgentThreadItem::new(request, workspace, window, cx));
    workspace.add_item_to_active_pane(Box::new(item.clone()), None, focus, window, cx);
    item
}

fn default_agent(workspace: &Workspace, cx: &App) -> Agent {
    workspace
        .panel::<AgentPanel>(cx)
        .map(|panel| panel.read(cx).selected_agent(cx))
        .unwrap_or_default()
}

pub(crate) fn new_thread_tab(
    workspace: &mut Workspace,
    agent: Option<Agent>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Entity<AgentThreadItem> {
    let agent = agent.unwrap_or_else(|| default_agent(workspace, cx));
    let item = cx.new(|cx| {
        AgentThreadItem::new(
            ThreadTabRequest {
                agent,
                ..Default::default()
            },
            workspace,
            window,
            cx,
        )
    });
    workspace.add_item_to_active_pane(Box::new(item.clone()), None, true, window, cx);
    item
}

/// Runs `update` on the workspace after the current update finishes: the
/// routing hooks run inside `AgentPanel` methods, which are often called while
/// the workspace itself is being updated.
fn defer_with_workspace(
    workspace: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
    update: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) {
    let workspace = workspace.clone();
    window.defer(cx, move |window, cx| {
        workspace
            .update(cx, |workspace, cx| update(workspace, window, cx))
            .log_err();
    });
}

impl ThreadTabRequest {
    pub(crate) fn new_thread(agent: Agent) -> Self {
        Self {
            agent,
            ..Default::default()
        }
    }

    pub(crate) fn for_thread(
        agent: &Agent,
        thread_id: ThreadId,
        work_dirs: &Option<PathList>,
        title: &Option<SharedString>,
        cx: &App,
    ) -> Self {
        let session_id = ThreadMetadataStore::try_global(cx).and_then(|store| {
            store
                .read(cx)
                .entry(thread_id)
                .and_then(|metadata| metadata.session_id.clone())
        });
        Self {
            agent: agent.clone(),
            session_id,
            thread_id: Some(thread_id),
            work_dirs: work_dirs
                .as_ref()
                .map(|work_dirs| work_dirs.paths().to_vec()),
            title: title.clone(),
        }
    }

    pub(crate) fn for_session(
        session_id: &acp_v2::SessionId,
        work_dirs: &Option<PathList>,
        title: &Option<SharedString>,
        cx: &App,
    ) -> Self {
        let metadata = ThreadMetadataStore::try_global(cx).and_then(|store| {
            store
                .read(cx)
                .entry_by_session(session_id)
                .map(|metadata| (metadata.thread_id, metadata.agent_id.clone()))
        });
        Self {
            agent: metadata
                .as_ref()
                .map(|(_, agent_id)| Agent::from(agent_id.clone()))
                .unwrap_or_default(),
            session_id: Some(session_id.clone()),
            thread_id: metadata.map(|(thread_id, _)| thread_id),
            work_dirs: work_dirs
                .as_ref()
                .map(|work_dirs| work_dirs.paths().to_vec()),
            title: title.clone(),
        }
    }
}

/// Hook for the `AgentPanel` methods that open threads (`new_thread`,
/// `load_agent_thread`, `open_thread`): with `agent.thread_location: "editor"`
/// the thread opens in an editor tab, or focuses the tab already showing it.
/// Callers pass `None` for opens that should stay in the panel, such as the
/// panel restoring its last thread at startup.
pub(crate) fn route_to_thread_tab(
    workspace: &WeakEntity<Workspace>,
    request: Option<ThreadTabRequest>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let Some(request) = request.filter(|_| threads_open_in_editor(cx)) else {
        return false;
    };
    defer_with_workspace(workspace, window, cx, move |workspace, window, cx| {
        open_thread_tab(workspace, request, true, window, cx);
    });
    true
}

/// The thread tab shortcuts are bound by default, so they only act once the
/// user opted into thread tabs or the Claude Code style. Otherwise the
/// keystroke falls through to its stock binding (`cmd-escape` cancels menus).
fn thread_tab_shortcuts_enabled(cx: &App) -> bool {
    threads_open_in_editor(cx) || super::claude_code_style_enabled(cx)
}

fn toggle_thread_tab_focus(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if !thread_tab_shortcuts_enabled(cx) {
        cx.propagate();
        return;
    }
    if !threads_open_in_editor(cx) {
        workspace.toggle_panel_focus::<AgentPanel>(window, cx);
        return;
    }
    let focused_tab = workspace
        .items_of_type::<AgentThreadItem>(cx)
        .find(|item| item.focus_handle(cx).contains_focused(window, cx));
    if focused_tab.is_some() {
        let editor_pane = workspace
            .panes()
            .iter()
            .find(|pane| {
                pane.read(cx)
                    .active_item()
                    .is_some_and(|item| item.downcast::<AgentThreadItem>().is_none())
            })
            .cloned();
        if let Some(pane) = editor_pane {
            window.focus(&pane.focus_handle(cx), cx);
        }
        return;
    }
    let visible_tab = workspace.panes().iter().find_map(|pane| {
        pane.read(cx)
            .active_item()
            .and_then(|item| item.downcast::<AgentThreadItem>())
    });
    let tab = visible_tab.or_else(|| workspace.items_of_type::<AgentThreadItem>(cx).next());
    match tab {
        Some(tab) => {
            workspace.activate_item(&tab, true, true, window, cx);
        }
        None => {
            new_thread_tab(workspace, None, window, cx);
        }
    }
}

pub(crate) fn init(cx: &mut App) {
    workspace::register_serializable_item::<AgentThreadItem>(cx);
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace
            .register_action(|workspace, _: &ToggleThreadTabFocus, window, cx| {
                toggle_thread_tab_focus(workspace, window, cx);
            })
            .register_action(|workspace, _: &NewThreadTab, window, cx| {
                if !thread_tab_shortcuts_enabled(cx) {
                    cx.propagate();
                    return;
                }
                new_thread_tab(workspace, None, window, cx);
            });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation_view::tests::init_test;
    use fs::FakeFs;
    use gpui::{TestAppContext, UpdateGlobal as _, VisualTestContext};
    use settings::SettingsStore;
    use workspace::MultiWorkspace;

    async fn setup<'a>(
        thread_location: &str,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Workspace>, &'a mut VisualTestContext) {
        init_test(cx);
        cx.update(|cx| {
            crate::ui_styles::init(cx);
            SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(
                        &format!(r#"{{ "agent": {{ "thread_location": "{thread_location}" }} }}"#),
                        cx,
                    )
                    .expect("valid thread_location settings");
            });
        });
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
        workspace.update(cx, |workspace, _| workspace.set_random_database_id());
        (workspace, cx)
    }

    fn thread_tabs(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> usize {
        workspace.read_with(cx, |workspace, cx| {
            workspace.items_of_type::<AgentThreadItem>(cx).count()
        })
    }

    #[gpui::test]
    async fn test_thread_tab_serialization_round_trip(cx: &mut TestAppContext) {
        let (workspace, cx) = setup("editor", cx).await;
        let item = workspace.update_in(cx, |workspace, window, cx| {
            new_thread_tab(workspace, Some(Agent::Stub), window, cx)
        });
        cx.run_until_parked();

        let item_id = item.entity_id().as_u64();
        let workspace_id = workspace
            .read_with(cx, |workspace, _| workspace.database_id())
            .expect("test workspace has a database id");
        let saved = workspace.update_in(cx, |workspace, _, cx| {
            item.update(cx, |item, cx| item.serialize(workspace, item_id, false, cx))
        });
        saved
            .expect("the tab serializes")
            .await
            .expect("serialization succeeds");

        let (project, expected_thread_id) = workspace.read_with(cx, |workspace, cx| {
            (workspace.project().clone(), item.read(cx).thread_id(cx))
        });
        let restored = cx
            .update(|window, cx| {
                AgentThreadItem::deserialize(
                    project,
                    workspace.downgrade(),
                    workspace_id,
                    item_id,
                    window,
                    cx,
                )
            })
            .await
            .expect("the tab deserializes");
        cx.run_until_parked();
        restored.read_with(cx, |restored, cx| {
            assert_eq!(restored.request.agent, Agent::Stub);
            assert_eq!(restored.thread_id(cx), expected_thread_id);
        });
    }

    #[gpui::test]
    async fn test_new_thread_routes_to_tab_in_editor_location(cx: &mut TestAppContext) {
        let (workspace, cx) = setup("editor", cx).await;
        cx.update(|window, cx| {
            assert!(route_to_thread_tab(
                &workspace.downgrade(),
                Some(ThreadTabRequest::new_thread(Agent::Stub)),
                window,
                cx
            ));
        });
        cx.run_until_parked();
        assert_eq!(thread_tabs(&workspace, cx), 1);
        let active_is_thread_tab = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .is_some_and(|item| item.downcast::<AgentThreadItem>().is_some())
        });
        assert!(active_is_thread_tab);
    }

    #[gpui::test]
    async fn test_new_thread_stays_in_panel_by_default(cx: &mut TestAppContext) {
        let (workspace, cx) = setup("panel", cx).await;
        cx.update(|window, cx| {
            assert!(!route_to_thread_tab(
                &workspace.downgrade(),
                Some(ThreadTabRequest::new_thread(Agent::Stub)),
                window,
                cx
            ));
        });
        cx.run_until_parked();
        assert_eq!(thread_tabs(&workspace, cx), 0);
    }

    #[gpui::test]
    async fn test_opening_an_open_thread_focuses_its_tab(cx: &mut TestAppContext) {
        let (workspace, cx) = setup("editor", cx).await;
        let first = workspace.update_in(cx, |workspace, window, cx| {
            new_thread_tab(workspace, Some(Agent::Stub), window, cx)
        });
        cx.run_until_parked();
        let thread_id = first.read_with(cx, |item, cx| item.thread_id(cx));
        let second = workspace.update_in(cx, |workspace, window, cx| {
            new_thread_tab(workspace, Some(Agent::Stub), window, cx)
        });
        cx.run_until_parked();
        assert_ne!(first.entity_id(), second.entity_id());

        let reopened = workspace.update_in(cx, |workspace, window, cx| {
            open_thread_tab(
                workspace,
                ThreadTabRequest {
                    agent: Agent::Stub,
                    thread_id: Some(thread_id),
                    ..Default::default()
                },
                true,
                window,
                cx,
            )
        });
        cx.run_until_parked();
        assert_eq!(reopened.entity_id(), first.entity_id());
        assert_eq!(thread_tabs(&workspace, cx), 2);
        let active_item_id = workspace.read_with(cx, |workspace, cx| {
            workspace.active_item(cx).map(|item| item.item_id())
        });
        assert_eq!(active_item_id, Some(first.entity_id()));
    }

    #[test]
    fn test_thread_tab_request_serde() {
        let request = ThreadTabRequest {
            agent: Agent::Custom {
                id: agent_servers::CLAUDE_AGENT_ID.into(),
            },
            session_id: Some(acp_v2::SessionId::new("session-1")),
            thread_id: Some(ThreadId::new()),
            work_dirs: Some(vec![PathBuf::from("/project")]),
            title: Some("Fix the build".into()),
        };
        let json = serde_json::to_string(&request).expect("serializes");
        let restored: ThreadTabRequest = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(restored, request);
    }
}
