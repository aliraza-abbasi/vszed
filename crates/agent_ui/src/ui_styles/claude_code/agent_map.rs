use std::time::{Duration, Instant};

use acp_thread::AcpThread;
use agent_servers::AgentExtNotifications;
use collections::HashMap;
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, EntityId, EventEmitter, FocusHandle, Focusable,
    ScrollHandle, Stateful, Subscription, Task,
};
use ui::{Tooltip, prelude::*};
use workspace::{ModalView, Workspace};

use super::{
    AgentStatus, AgentSummary, BackgroundTaskSummary, DotState, StatusDot, agent_count_label,
    agent_summaries, background_task_summaries, format_token_count, thread_activity,
};

const NODE_WIDTH: Pixels = px(300.);
const CONNECTOR_WIDTH: Pixels = px(16.);
const MAP_MAX_WIDTH: Pixels = px(1200.);

#[derive(Clone, Debug, PartialEq)]
enum Selection {
    Agent(SharedString),
    Task(SharedString),
}

/// Claude Code's agent map: the session on the left, its agents in a column to its
/// right and each agent's own agents in further columns, joined by elbow connectors,
/// then the session's background tasks. Clicking an agent or a task shows its details.
pub(crate) struct AgentMap {
    thread: Entity<AcpThread>,
    thread_view_id: EntityId,
    session_title: SharedString,
    model_name: Option<SharedString>,
    selected: Option<Selection>,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    _ticker: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for AgentMap {}

impl ModalView for AgentMap {
    fn fade_out_background(&self) -> bool {
        true
    }
}

impl Focusable for AgentMap {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl AgentMap {
    fn new(
        thread: Entity<AcpThread>,
        thread_view_id: EntityId,
        model_name: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        let session_title = thread
            .read(cx)
            .title()
            .unwrap_or_else(|| "This session".into());
        let ext_notifications = AgentExtNotifications::global(cx);
        let subscriptions = vec![
            cx.observe(&thread, |_, _, cx| cx.notify()),
            cx.subscribe(&ext_notifications, |_, _, _, cx| cx.notify()),
        ];
        // Running agents and tasks count their time up.
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            thread,
            thread_view_id,
            session_title,
            model_name,
            selected: None,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            _ticker: ticker,
            _subscriptions: subscriptions,
        }
    }

    fn dismiss(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.take().is_some() {
            cx.notify();
        } else {
            cx.emit(DismissEvent);
        }
    }

    fn select(&mut self, selection: Selection, cx: &mut Context<Self>) {
        self.selected = Some(selection);
        cx.notify();
    }

    fn render_session_node(&self, cx: &Context<Self>) -> Stateful<Div> {
        let thread = self.thread.read(cx);
        let is_busy = matches!(thread.status(), acp_thread::ThreadStatus::Generating);
        let tokens = thread.token_usage().map(|usage| {
            format!(
                "{} tokens in context",
                format_token_count(usage.used_tokens)
            )
        });
        let meta = [
            self.model_name.as_ref().map(|name| name.to_string()),
            tokens,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        render_node(
            "agent-map-session",
            if is_busy {
                DotState::Running
            } else {
                DotState::Idle
            },
            self.session_title.clone(),
            meta,
            true,
            cx,
        )
    }

    /// A node with its children in a column to its right, the node centered against
    /// them.
    fn render_subtree(
        &self,
        node: Stateful<Div>,
        children: &[&AgentSummary],
        children_by_parent: &HashMap<SharedString, Vec<&AgentSummary>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let border = cx.theme().colors().border;
        let child_rows = children
            .iter()
            .map(|agent| {
                let key = agent.key.clone();
                let agent_node = render_node(
                    SharedString::from(format!("agent-map-agent-{}", agent.key)),
                    agent.dot_state(),
                    agent.description.clone(),
                    agent.meta(),
                    false,
                    cx,
                )
                .tooltip(Tooltip::text(agent.description.clone()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select(Selection::Agent(key.clone()), cx);
                }));
                let grandchildren = children_by_parent
                    .get(&agent.key)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                // An elbow from the column's trunk to the child.
                h_flex()
                    .items_center()
                    .border_l_1()
                    .border_color(border)
                    .child(div().w(CONNECTOR_WIDTH).h_px().bg(border))
                    .child(self.render_subtree(agent_node, grandchildren, children_by_parent, cx))
            })
            .collect::<Vec<_>>();
        h_flex()
            .items_center()
            .child(node)
            .when(!child_rows.is_empty(), |this| {
                this.child(div().w(CONNECTOR_WIDTH).h_px().bg(border))
                    .child(v_flex().gap_2().children(child_rows))
            })
            .into_any_element()
    }

    fn render_tree(&self, agents: &[AgentSummary], cx: &mut Context<Self>) -> AnyElement {
        let keys = agents
            .iter()
            .map(|agent| agent.key.clone())
            .collect::<Vec<_>>();
        let mut top_level = Vec::new();
        let mut children_by_parent: HashMap<SharedString, Vec<&AgentSummary>> = HashMap::default();
        for agent in agents {
            // A parent that is not in the map (no longer tracked) counts as none.
            match agent
                .parent_key
                .as_ref()
                .filter(|parent| *parent != &agent.key && keys.contains(parent))
            {
                Some(parent) => children_by_parent
                    .entry(parent.clone())
                    .or_default()
                    .push(agent),
                None => top_level.push(agent),
            }
        }
        let session_node = self.render_session_node(cx);
        self.render_subtree(session_node, &top_level, &children_by_parent, cx)
    }

    fn render_background_tasks(
        &self,
        tasks: &[BackgroundTaskSummary],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let task_word = if tasks.len() == 1 { "task" } else { "tasks" };
        v_flex()
            .pt_4()
            .gap_2()
            .child(
                Label::new(format!(
                    "{} background {task_word} · click a task for details",
                    tasks.len()
                ))
                .color(Color::Muted),
            )
            .children(tasks.iter().map(|task| {
                let key = task.key.clone();
                render_node(
                    SharedString::from(format!("agent-map-task-{}", task.key)),
                    DotState::Running,
                    task.title.clone(),
                    task.meta(),
                    false,
                    cx,
                )
                .tooltip(Tooltip::text(task.title.clone()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select(Selection::Task(key.clone()), cx);
                }))
            }))
    }

    fn render_details(
        &self,
        dot: DotState,
        title: SharedString,
        lines: Vec<(SharedString, Color)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        IconButton::new("agent-map-back", IconName::ArrowLeft)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Back to the agent map"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.selected = None;
                                cx.notify();
                            })),
                    )
                    .child(StatusDot::new(dot))
                    .child(Label::new(title).truncate()),
            )
            .children(
                lines
                    .into_iter()
                    .map(|(line, color)| Label::new(line).size(LabelSize::Small).color(color)),
            )
            .into_any_element()
    }

    fn render_agent_details(&self, agent: &AgentSummary, cx: &mut Context<Self>) -> AnyElement {
        let status = match (agent.waiting, agent.status) {
            (true, _) => "Waiting for your permission",
            (false, AgentStatus::Working) => "Working",
            (false, AgentStatus::Finished) => "Finished",
            (false, AgentStatus::Failed) => "Failed",
            (false, AgentStatus::Stopped) => "Stopped",
        };
        let meta = agent.meta();
        let status_line = if meta.is_empty() {
            status.to_string()
        } else {
            format!("{status} · {meta}")
        };
        let spawned_by = match &agent.parent_key {
            Some(_) => "Spawned by another agent",
            None => "Spawned by the main agent",
        };
        let mut lines = vec![
            (SharedString::from(status_line), Color::Muted),
            (SharedString::from(spawned_by), Color::Muted),
        ];
        if let Some(activity) = agent.summary.clone().or_else(|| agent.activity.clone()) {
            lines.push((activity, Color::Default));
        }
        if let Some(error) = agent.error.clone() {
            lines.push((error, Color::Error));
        }
        self.render_details(agent.dot_state(), agent.description.clone(), lines, cx)
    }
}

fn render_node(
    id: impl Into<ElementId>,
    dot: DotState,
    title: SharedString,
    meta: String,
    is_session: bool,
    cx: &App,
) -> Stateful<Div> {
    let colors = cx.theme().colors();
    let title = Label::new(title).truncate();
    div()
        .id(id)
        .flex_none()
        .w(NODE_WIDTH)
        .px_3()
        .py_1p5()
        .rounded_md()
        .border_1()
        .border_color(colors.border_variant)
        .bg(colors.element_background)
        .cursor_pointer()
        .hover(|style| style.bg(colors.element_hover))
        .child(
            h_flex()
                .gap_2()
                .child(StatusDot::new(dot))
                .child(if is_session {
                    title.weight(gpui::FontWeight::SEMIBOLD)
                } else {
                    title
                }),
        )
        .when(!meta.is_empty(), |this| {
            this.child(
                div()
                    .pl_4()
                    .child(Label::new(meta).size(LabelSize::Small).color(Color::Muted)),
            )
        })
}

impl Render for AgentMap {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let thread = self.thread.read(cx);
        let activity = thread_activity(self.thread_view_id, cx);
        let agents = agent_summaries(thread, activity, now, cx);
        let tasks = background_task_summaries(thread, activity, now, cx);

        let body =
            match self.selected.clone() {
                Some(Selection::Agent(key)) => agents
                    .iter()
                    .find(|agent| agent.key == key)
                    .cloned()
                    .map(|agent| self.render_agent_details(&agent, cx)),
                Some(Selection::Task(key)) => tasks
                    .iter()
                    .find(|task| task.key == key)
                    .cloned()
                    .map(|task| {
                        self.render_details(
                            DotState::Running,
                            task.title.clone(),
                            vec![(SharedString::from(task.meta()), Color::Muted)],
                            cx,
                        )
                    }),
                None => None,
            };
        let body = match body {
            Some(details) => details,
            None => v_flex()
                .gap_3()
                .child(
                    Label::new(format!(
                        "{} · click an agent for details",
                        agent_count_label(agents.len())
                    ))
                    .color(Color::Muted),
                )
                .child(self.render_tree(&agents, cx))
                .when(!tasks.is_empty(), |this| {
                    this.child(self.render_background_tasks(&tasks, cx))
                })
                .into_any_element(),
        };

        let viewport = window.viewport_size();
        v_flex()
            .key_context("AgentMap")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::dismiss))
            .elevation_3(cx)
            .w(MAP_MAX_WIDTH.min(viewport.width * 0.9))
            .h(viewport.height * 0.85)
            .child(
                h_flex()
                    .flex_none()
                    .justify_between()
                    .px_4()
                    .pt_4()
                    .pb_2()
                    .child(Headline::new("Agent map").size(HeadlineSize::Small))
                    .child(
                        IconButton::new("agent-map-close", IconName::Close)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Close"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    ),
            )
            .child(
                div()
                    .id("agent-map-body")
                    .flex_1()
                    .min_h_0()
                    .px_4()
                    .pb_4()
                    .overflow_scroll()
                    .track_scroll(&self.scroll_handle)
                    .child(body),
            )
    }
}

/// Opens the agent map of `thread` over the workspace.
pub(crate) fn open_agent_map(
    workspace: &Entity<Workspace>,
    thread: Entity<AcpThread>,
    thread_view_id: EntityId,
    model_name: Option<SharedString>,
    window: &mut Window,
    cx: &mut App,
) {
    workspace.update(cx, |workspace, cx| {
        workspace.toggle_modal(window, cx, |_window, cx| {
            AgentMap::new(thread, thread_view_id, model_name, cx)
        });
    });
}
