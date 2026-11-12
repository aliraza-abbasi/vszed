mod agent_map;
mod model_picker;
pub(crate) mod sessions_panel;
mod tool_row;
pub(crate) mod usage;

pub(crate) use agent_map::open_agent_map;
pub(crate) use model_picker::{ClaudeCodeModelPicker, ConfigOptionsHandle};
pub(crate) use tool_row::ToolRow;

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use acp_thread::{
    AcpThread, AcpThreadEvent, AgentThreadEntry, AssistantMessageChunk, MentionUri,
    PermissionRequestId, ThreadStatus, ToolCall, ToolCallContent, ToolCallStatus,
};
use agent_client_protocol::v2::schema as acp_v2;
use agent_servers::{AgentExtNotification, AgentExtNotifications};
use chrono::{DateTime, Datelike as _, Local};
use collections::{HashMap, HashSet};
use editor::Editor;
use gpui::{
    Animation, AnimationExt as _, AnyView, App, Context, ElementId, Entity, EntityId, FocusHandle,
    Focusable as _, Global, Hsla, Pixels, Subscription, TextStyle, WeakEntity, pulsating_between,
    px, rgba,
};
use language::Buffer;
use markdown::{MarkdownFont, MarkdownStyle};
use settings::{AgentUiStyle, Settings as _, SettingsStore};
use theme_settings::ThemeSettings;
use ui::prelude::*;
use util::{ResultExt as _, paths::PathStyle};
use workspace::{StatusItemView, Workspace, item::ItemHandle};

use super::AgentUiStyleSettings;
use crate::conversation_view::ThreadView;

/// Width of the left gutter that holds each timeline step's status dot.
pub(crate) const TIMELINE_GUTTER: Pixels = px(16.);
const TIMELINE_DOT_SIZE: Pixels = px(7.);

/// The `_meta` namespace claude-agent-acp uses for Claude Code specific data.
const CLAUDE_CODE_META_KEY: &str = "claudeCode";
const CLAUDE_CODE_TOOL_NAME_KEY: &str = "toolName";
/// claude-agent-acp's plan approval option ids all start with this
/// (`src/permissions/options/shared.ts`).
const EXIT_PLAN_OPTION_PREFIX: &str = "exit-plan-";
/// claude-agent-acp's config option id for Ultracode (`src/session-config-ids.ts`).
pub(crate) const ULTRACODE_CONFIG_ID: &str = "ultracode";
/// The built-in tools claude-agent-acp reports by name (`src/tool-calls/reporters`).
const CLAUDE_CODE_TOOL_NAMES: &[&str] = &[
    "Agent",
    "AskUserQuestion",
    "Bash",
    "BashOutput",
    "Edit",
    "EnterPlanMode",
    "ExitPlanMode",
    "Glob",
    "Grep",
    "KillShell",
    "ListAgents",
    "Monitor",
    "MultiEdit",
    "NotebookEdit",
    "PowerShell",
    "Read",
    "ReportFindings",
    "SendMessage",
    "Skill",
    "Task",
    "TaskCreate",
    "TaskGet",
    "TaskList",
    "TaskStop",
    "TaskUpdate",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
    "Write",
];
/// Tab stops used when laying out tool input and output as plain text.
const TAB_WIDTH: usize = 4;

/// Skip computing line stats for edits whose text is too large to diff on every render.
const MAX_DIFF_STATS_INPUT_BYTES: usize = 256 * 1024;

const SPINNER_GLYPHS: [&str; 6] = ["·", "✢", "✳", "✶", "✻", "✽"];
const SPINNER_VERBS: [&str; 12] = [
    "Spinning",
    "Pondering",
    "Brewing",
    "Cogitating",
    "Noodling",
    "Percolating",
    "Ruminating",
    "Simmering",
    "Musing",
    "Conjuring",
    "Crafting",
    "Mulling",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepStatus {
    /// Assistant text.
    Neutral,
    /// Thinking and other secondary steps.
    Muted,
    Running,
    AwaitingPermission,
    Succeeded,
    Failed,
    Canceled,
}

impl StepStatus {
    pub(crate) fn from_tool_call_status(status: ToolCallStatus) -> Self {
        match status {
            ToolCallStatus::Pending | ToolCallStatus::InProgress | ToolCallStatus::Paused => {
                Self::Running
            }
            ToolCallStatus::WaitingForConfirmation => Self::AwaitingPermission,
            ToolCallStatus::Completed => Self::Succeeded,
            ToolCallStatus::Failed | ToolCallStatus::Rejected => Self::Failed,
            ToolCallStatus::Canceled => Self::Canceled,
        }
    }

    fn color(self, cx: &App) -> Hsla {
        let colors = cx.theme().colors();
        let status = cx.theme().status();
        match self {
            Self::Neutral => colors.text,
            Self::Muted | Self::Canceled => colors.text_muted,
            Self::Running => colors.text_accent,
            Self::AwaitingPermission => status.warning,
            Self::Succeeded => status.success,
            Self::Failed => status.error,
        }
    }
}

#[derive(IntoElement)]
pub(crate) struct TimelineDot {
    id: ElementId,
    status: StepStatus,
}

impl TimelineDot {
    pub(crate) fn new(id: impl Into<ElementId>, status: StepStatus) -> Self {
        Self {
            id: id.into(),
            status,
        }
    }
}

impl RenderOnce for TimelineDot {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let dot = div()
            .flex_none()
            .size(TIMELINE_DOT_SIZE)
            .rounded_full()
            .bg(self.status.color(cx));
        if self.status == StepStatus::Running {
            dot.with_animation(
                self.id,
                Animation::new(Duration::from_millis(1600))
                    .repeat()
                    .with_easing(pulsating_between(0.35, 1.)),
                |dot, delta| dot.opacity(delta),
            )
            .into_any_element()
        } else {
            dot.into_any_element()
        }
    }
}

/// A timeline step: the status dot in the left gutter, vertically centered on
/// the first line of `content`.
pub(crate) fn timeline_row(
    dot: impl IntoElement,
    content: impl IntoElement,
    window: &Window,
) -> Div {
    h_flex()
        .w_full()
        .items_start()
        .child(
            h_flex()
                .flex_none()
                .w(TIMELINE_GUTTER)
                .h(window.line_height())
                .items_center()
                .child(dot),
        )
        .child(div().flex_1().min_w_0().child(content))
}

/// What the timeline knows about a thread beyond the `AcpThread` data model:
/// when each user message was sent and how long each thought streamed for.
#[derive(Default)]
pub(crate) struct ThreadActivity {
    /// The placeholder Zed's own style set on the message editor, restored when
    /// switching back from the Claude Code style.
    zed_placeholder: Option<String>,
    last_activity_at: Option<Instant>,
    thoughts: HashMap<(usize, usize), ThoughtTiming>,
    /// The thought that is still the last thing in the thread. It ends when
    /// anything else arrives after it.
    open_thought: Option<(usize, usize)>,
    /// Claude Code opens or closes every thought summary of a conversation at
    /// once, starting closed.
    thoughts_expanded: bool,
    /// When each user message and each run of assistant text first appeared,
    /// by entry and chunk (user messages use chunk 0). Messages replayed from
    /// a loaded session have no time: the adapter sends none.
    message_times: BTreeMap<(usize, usize), DateTime<Local>>,
    /// Long user messages the user expanded with "Show more".
    expanded_user_messages: HashSet<usize>,
    /// User messages whose text is taller than the collapsed box, as measured
    /// in the last frame.
    overflowing_user_messages: HashSet<usize>,
    /// Editor tabs showing the full output of a tool call, by tool call id.
    output_tabs: HashMap<acp_v2::ToolCallId, ToolOutputTab>,
    /// Focused while the permission card replaces the prompt box, so the
    /// card's number keys and Esc reach it.
    permission_focus: Option<FocusHandle>,
    /// The highlighted option of the permission card, for the request it was
    /// chosen on.
    permission_selection: Option<(PermissionRequestId, usize)>,
    /// claude-agent-acp's tasks from raw SDK messages (ACP v1), in start order.
    sdk_tasks: Vec<SdkTask>,
    /// When each ACP v2 subagent session was announced and last showed activity.
    subagent_times: HashMap<acp_v2::SessionId, SubagentTimes>,
    subagent_subscriptions: Vec<Subscription>,
    prompt_cache: PromptCache,
    /// The height of the floating prompt box, which the transcript keeps clear below its
    /// last entry.
    floating_prompt_height: Rc<Cell<Pixels>>,
    /// The model picker, while it is open above the prompt box.
    model_picker: Option<AnyView>,
    model_picker_dismissed_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug)]
struct ThoughtTiming {
    started_at: Instant,
    last_updated_at: Instant,
}

/// A read-only editor tab with a tool call's full output, kept in sync while
/// the tool call streams.
pub(crate) struct ToolOutputTab {
    pub editor: WeakEntity<Editor>,
    pub buffer: WeakEntity<Buffer>,
    pub _subscriptions: Vec<Subscription>,
}

impl ToolOutputTab {
    fn live_parts(&self) -> Option<(Entity<Editor>, Entity<Buffer>)> {
        Some((self.editor.upgrade()?, self.buffer.upgrade()?))
    }
}

impl ThreadActivity {
    pub(crate) fn thought_duration(&self, entry_ix: usize, chunk_ix: usize) -> Option<Duration> {
        self.thoughts.get(&(entry_ix, chunk_ix)).map(|timing| {
            timing
                .last_updated_at
                .saturating_duration_since(timing.started_at)
        })
    }

    pub(crate) fn message_time(&self, key: (usize, usize)) -> Option<DateTime<Local>> {
        self.message_times.get(&key).copied()
    }

    /// Whether the message at `key` is the first with a time on its day, which
    /// Claude Code marks with a date line (also above the first message).
    pub(crate) fn starts_new_day(&self, key: (usize, usize)) -> bool {
        let Some(time) = self.message_times.get(&key) else {
            return false;
        };
        self.message_times
            .range(..key)
            .next_back()
            .is_none_or(|(_, previous)| previous.date_naive() != time.date_naive())
    }

    pub(crate) fn is_user_message_expanded(&self, entry_ix: usize) -> bool {
        self.expanded_user_messages.contains(&entry_ix)
    }

    pub(crate) fn user_message_overflows(&self, entry_ix: usize) -> bool {
        self.overflowing_user_messages.contains(&entry_ix)
    }

    pub(crate) fn thoughts_expanded(&self) -> bool {
        self.thoughts_expanded
    }

    pub(crate) fn output_tab(
        &self,
        tool_call_id: &acp_v2::ToolCallId,
    ) -> Option<(Entity<Editor>, Entity<Buffer>)> {
        self.output_tabs.get(tool_call_id)?.live_parts()
    }

    pub(crate) fn permission_focus(&self) -> Option<&FocusHandle> {
        self.permission_focus.as_ref()
    }

    pub(crate) fn permission_selection(&self, request_id: PermissionRequestId) -> usize {
        match self.permission_selection {
            Some((selected_request_id, index)) if selected_request_id == request_id => index,
            _ => 0,
        }
    }

    pub(crate) fn floating_prompt_height(&self) -> Rc<Cell<Pixels>> {
        self.floating_prompt_height.clone()
    }

    pub(crate) fn model_picker(&self) -> Option<&AnyView> {
        self.model_picker.as_ref()
    }

    pub(crate) fn model_picker_dismissed_at(&self) -> Option<Instant> {
        self.model_picker_dismissed_at
    }

    pub(crate) fn cache_clock(&self, now: Instant) -> CacheClock {
        self.prompt_cache.clock(now)
    }

    fn record_event(
        &mut self,
        thread: &AcpThread,
        event: &AcpThreadEvent,
        now: Instant,
        wall_clock: DateTime<Local>,
    ) {
        match event {
            AcpThreadEvent::NewEntry => {
                let Some(entry_ix) = thread.entries().len().checked_sub(1) else {
                    return;
                };
                match thread.entries().get(entry_ix) {
                    Some(AgentThreadEntry::UserMessage(_)) => {
                        self.message_times.insert((entry_ix, 0), wall_clock);
                    }
                    // Each model request starts a new response entry. Entries replayed
                    // when a session loads are not requests made now.
                    Some(AgentThreadEntry::AssistantMessage(_) | AgentThreadEntry::ToolCall(_))
                        if thread.status() == ThreadStatus::Generating =>
                    {
                        self.prompt_cache.record_request(now);
                    }
                    _ => {}
                }
                self.record_assistant_text(thread, entry_ix, wall_clock);
                self.record_streaming_thought(thread, entry_ix, now);
                self.finish_open_thought(thread, now);
                self.last_activity_at = Some(now);
            }
            AcpThreadEvent::EntryUpdated(entry_ix) => {
                self.record_assistant_text(thread, *entry_ix, wall_clock);
                self.record_streaming_thought(thread, *entry_ix, now);
                self.finish_open_thought(thread, now);
                if let Some(AgentThreadEntry::ContextCompaction(compaction)) =
                    thread.entries().get(*entry_ix)
                    && matches!(
                        compaction.status,
                        acp_thread::ContextCompactionStatus::Completed
                    )
                {
                    self.prompt_cache.record_compaction(now);
                }
                self.last_activity_at = Some(now);
            }
            AcpThreadEvent::EntriesRemoved(range) => self.remove_entries(range.clone()),
            AcpThreadEvent::Stopped { .. } | AcpThreadEvent::Error => {
                self.open_thought = None;
            }
            AcpThreadEvent::SubagentSpawned(session_id) => {
                self.subagent_times
                    .entry(session_id.clone())
                    .or_insert(SubagentTimes {
                        started_at: now,
                        last_activity_at: now,
                    });
            }
            _ => {}
        }
    }

    fn record_assistant_text(
        &mut self,
        thread: &AcpThread,
        entry_ix: usize,
        wall_clock: DateTime<Local>,
    ) {
        let Some(AgentThreadEntry::AssistantMessage(message)) = thread.entries().get(entry_ix)
        else {
            return;
        };
        let Some(chunk_ix) = message.chunks.len().checked_sub(1) else {
            return;
        };
        if matches!(
            message.chunks.get(chunk_ix),
            Some(AssistantMessageChunk::Message { .. })
        ) {
            self.message_times
                .entry((entry_ix, chunk_ix))
                .or_insert(wall_clock);
        }
    }

    /// A thought ends when the next step starts, which is also when Claude
    /// Code's thinking block stops. Zed reveals streamed text gradually, so the
    /// thought's own updates can trail behind it.
    fn finish_open_thought(&mut self, thread: &AcpThread, now: Instant) {
        let Some(open_thought) = self.open_thought else {
            return;
        };
        if trailing_thought(thread) == Some(open_thought) {
            return;
        }
        if let Some(timing) = self.thoughts.get_mut(&open_thought) {
            timing.last_updated_at = now;
        }
        self.open_thought = None;
    }

    fn record_subagent_activity(&mut self, session_id: &acp_v2::SessionId, now: Instant) {
        if let Some(times) = self.subagent_times.get_mut(session_id) {
            times.last_activity_at = now;
        }
    }

    /// A thought is timed from the previous activity in the thread (the model
    /// starts thinking as soon as it has the previous step's result) until its
    /// last streamed chunk. Thoughts replayed from a loaded session arrive all at
    /// once, so they get no time and read "Thinking", as in Claude Code.
    fn record_streaming_thought(&mut self, thread: &AcpThread, entry_ix: usize, now: Instant) {
        if thread.status() != ThreadStatus::Generating {
            return;
        }
        let Some(AgentThreadEntry::AssistantMessage(message)) = thread.entries().get(entry_ix)
        else {
            return;
        };
        let Some(chunk_ix) = message.chunks.len().checked_sub(1) else {
            return;
        };
        if !matches!(
            message.chunks.get(chunk_ix),
            Some(AssistantMessageChunk::Thought { .. })
        ) {
            return;
        }
        let started_at = self.last_activity_at.unwrap_or(now);
        self.thoughts
            .entry((entry_ix, chunk_ix))
            .and_modify(|timing| timing.last_updated_at = now)
            .or_insert(ThoughtTiming {
                started_at,
                last_updated_at: now,
            });
        if trailing_thought(thread) == Some((entry_ix, chunk_ix)) {
            self.open_thought = Some((entry_ix, chunk_ix));
        }
    }

    fn remove_entries(&mut self, removed: Range<usize>) {
        let shift = |entry_ix: usize| {
            if entry_ix < removed.start {
                Some(entry_ix)
            } else if entry_ix < removed.end {
                None
            } else {
                Some(entry_ix - removed.len())
            }
        };
        self.thoughts = self
            .thoughts
            .drain()
            .filter_map(|((entry_ix, chunk_ix), timing)| {
                Some(((shift(entry_ix)?, chunk_ix), timing))
            })
            .collect();
        self.open_thought = self
            .open_thought
            .and_then(|(entry_ix, chunk_ix)| Some((shift(entry_ix)?, chunk_ix)));
        self.message_times = std::mem::take(&mut self.message_times)
            .into_iter()
            .filter_map(|((entry_ix, chunk_ix), time)| Some(((shift(entry_ix)?, chunk_ix), time)))
            .collect();
        self.expanded_user_messages = self
            .expanded_user_messages
            .drain()
            .filter_map(shift)
            .collect();
        self.overflowing_user_messages = self
            .overflowing_user_messages
            .drain()
            .filter_map(shift)
            .collect();
    }
}

/// The thought at the very end of the thread, if the thread ends with one.
fn trailing_thought(thread: &AcpThread) -> Option<(usize, usize)> {
    let entry_ix = thread.entries().len().checked_sub(1)?;
    let Some(AgentThreadEntry::AssistantMessage(message)) = thread.entries().get(entry_ix) else {
        return None;
    };
    let chunk_ix = message.chunks.len().checked_sub(1)?;
    matches!(
        message.chunks.get(chunk_ix),
        Some(AssistantMessageChunk::Thought { .. })
    )
    .then_some((entry_ix, chunk_ix))
}

#[derive(Default)]
struct ClaudeCodeActivity {
    threads: HashMap<EntityId, ThreadActivity>,
}

impl Global for ClaudeCodeActivity {}

pub(crate) fn thread_activity(thread_view_id: EntityId, cx: &App) -> Option<&ThreadActivity> {
    cx.try_global::<ClaudeCodeActivity>()?
        .threads
        .get(&thread_view_id)
}

pub(crate) fn thread_activity_mut(
    thread_view_id: EntityId,
    cx: &mut App,
) -> Option<&mut ThreadActivity> {
    cx.default_global::<ClaudeCodeActivity>()
        .threads
        .get_mut(&thread_view_id)
}

pub(crate) fn toggle_thoughts_expanded(thread_view_id: EntityId, cx: &mut App) {
    if let Some(activity) = thread_activity_mut(thread_view_id, cx) {
        activity.thoughts_expanded = !activity.thoughts_expanded;
    }
}

pub(crate) fn toggle_user_message_expanded(
    thread_view_id: EntityId,
    entry_ix: usize,
    cx: &mut App,
) {
    if let Some(activity) = thread_activity_mut(thread_view_id, cx)
        && !activity.expanded_user_messages.remove(&entry_ix)
    {
        activity.expanded_user_messages.insert(entry_ix);
    }
}

/// Records whether a user message's text is taller than its collapsed box,
/// and re-renders the thread view when that changes, because it is only
/// known once the text has been laid out.
pub(crate) fn set_user_message_overflows(
    thread_view_id: EntityId,
    entry_ix: usize,
    overflows: bool,
    cx: &mut App,
) {
    let Some(activity) = thread_activity_mut(thread_view_id, cx) else {
        return;
    };
    let changed = if overflows {
        activity.overflowing_user_messages.insert(entry_ix)
    } else {
        activity.overflowing_user_messages.remove(&entry_ix)
    };
    if changed {
        cx.notify(thread_view_id);
    }
}

pub(crate) fn set_output_tab(
    thread_view_id: EntityId,
    tool_call_id: acp_v2::ToolCallId,
    tab: ToolOutputTab,
    cx: &mut App,
) {
    if let Some(activity) = thread_activity_mut(thread_view_id, cx) {
        activity.output_tabs.insert(tool_call_id, tab);
    }
}

pub(crate) fn remove_output_tab(
    thread_view_id: EntityId,
    tool_call_id: &acp_v2::ToolCallId,
    cx: &mut App,
) {
    if let Some(activity) = thread_activity_mut(thread_view_id, cx) {
        activity.output_tabs.remove(tool_call_id);
    }
}

pub(crate) fn set_permission_selection(
    thread_view_id: EntityId,
    request_id: PermissionRequestId,
    index: usize,
    cx: &mut App,
) {
    if let Some(activity) = cx
        .default_global::<ClaudeCodeActivity>()
        .threads
        .get_mut(&thread_view_id)
    {
        activity.permission_selection = Some((request_id, index));
    }
}

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|thread_view: &mut ThreadView, window, cx| {
        track_thread_view(thread_view, window, cx);
    })
    .detach();
    sessions_panel::init(cx);
    usage::init(cx);
    init_status_item(cx);
}

/// Records timeline activity for every thread view, whichever style is active,
/// so that switching to the Claude Code style mid-thread still knows recent
/// timings. Also re-measures the list when the style changes, because every
/// row changes height.
fn track_thread_view(
    thread_view: &mut ThreadView,
    window: Option<&mut Window>,
    cx: &mut Context<ThreadView>,
) {
    let thread_view_id = cx.entity_id();
    let activity = ThreadActivity {
        permission_focus: Some(cx.focus_handle()),
        ..ThreadActivity::default()
    };
    cx.default_global::<ClaudeCodeActivity>()
        .threads
        .insert(thread_view_id, activity);

    let thread_subscription = cx.subscribe(&thread_view.thread, move |_, thread, event, cx| {
        let now = Instant::now();
        let wall_clock = Local::now();
        if !cx.has_global::<ClaudeCodeActivity>() {
            return;
        }
        cx.update_global::<ClaudeCodeActivity, _>(|activity, cx| {
            if let Some(thread_activity) = activity.threads.get_mut(&thread_view_id) {
                thread_activity.record_event(thread.read(cx), event, now, wall_clock);
            }
        });
        if let AcpThreadEvent::SubagentSpawned(session_id) = event
            && let Some(subagent) = thread.read(cx).subagent(session_id).cloned()
        {
            track_subagent(thread_view_id, session_id.clone(), &subagent, cx);
        }
    });

    let ext_notifications = AgentExtNotifications::global(cx);
    let thread = thread_view.thread.downgrade();
    let ext_subscription = cx.subscribe(&ext_notifications, move |_, _, notification, cx| {
        record_ext_notification(thread_view_id, &thread, notification, cx);
    });

    let mut ui_style = AgentUiStyleSettings::get_global(cx).ui_style;
    let on_settings_changed = move |thread_view: &mut ThreadView,
                                    window: Option<&mut Window>,
                                    cx: &mut Context<ThreadView>| {
        let new_ui_style = AgentUiStyleSettings::get_global(cx).ui_style;
        if new_ui_style == ui_style {
            return;
        }
        ui_style = new_ui_style;
        thread_view.list_state.remeasure();
        if new_ui_style != AgentUiStyle::ClaudeCode {
            thread_view.sync_editor_mode(cx);
        }
        if new_ui_style != AgentUiStyle::ClaudeCode
            && let Some(window) = window
        {
            restore_zed_placeholder(thread_view, window, cx);
            // Zed's style has no permission card, so focus must not stay on it.
            let permission_card_focused = thread_activity(thread_view_id, cx)
                .and_then(|activity| activity.permission_focus())
                .is_some_and(|focus| focus.is_focused(window));
            if permission_card_focused {
                let editor_focus = thread_view.message_editor.focus_handle(cx);
                window.focus(&editor_focus, cx);
            }
        }
        cx.notify();
    };
    let settings_subscription = match window {
        Some(window) => cx.observe_global_in::<SettingsStore>(window, {
            let mut on_settings_changed = on_settings_changed;
            move |thread_view, window, cx| on_settings_changed(thread_view, Some(window), cx)
        }),
        None => cx.observe_global::<SettingsStore>({
            let mut on_settings_changed = on_settings_changed;
            move |thread_view, cx| on_settings_changed(thread_view, None, cx)
        }),
    };

    let release_subscription = cx.on_release(move |_, cx| {
        cx.default_global::<ClaudeCodeActivity>()
            .threads
            .remove(&thread_view_id);
    });

    thread_view._subscriptions.extend([
        thread_subscription,
        ext_subscription,
        settings_subscription,
        release_subscription,
    ]);
}

/// Times an ACP v2 subagent from its announcement to its last activity, for the
/// agent map.
fn track_subagent(
    thread_view_id: EntityId,
    session_id: acp_v2::SessionId,
    subagent: &Entity<AcpThread>,
    cx: &mut App,
) {
    let subscription = cx.subscribe(subagent, move |subagent, event, cx| {
        let now = Instant::now();
        match event {
            AcpThreadEvent::NewEntry
            | AcpThreadEvent::EntryUpdated(_)
            | AcpThreadEvent::Stopped { .. } => {
                if let Some(activity) = thread_activity_mut(thread_view_id, cx) {
                    activity.record_subagent_activity(&session_id, now);
                }
            }
            AcpThreadEvent::SubagentSpawned(nested_session_id) => {
                if let Some(activity) = thread_activity_mut(thread_view_id, cx) {
                    activity
                        .subagent_times
                        .entry(nested_session_id.clone())
                        .or_insert(SubagentTimes {
                            started_at: now,
                            last_activity_at: now,
                        });
                }
                if let Some(nested) = subagent.read(cx).subagent(nested_session_id).cloned() {
                    track_subagent(thread_view_id, nested_session_id.clone(), &nested, cx);
                }
            }
            _ => {}
        }
    });
    if let Some(activity) = thread_activity_mut(thread_view_id, cx) {
        activity.subagent_subscriptions.push(subscription);
    }
}

fn record_ext_notification(
    thread_view_id: EntityId,
    thread: &WeakEntity<AcpThread>,
    notification: &AgentExtNotification,
    cx: &mut App,
) {
    if notification.method.as_ref() != CLAUDE_SDK_MESSAGE_METHOD {
        return;
    }
    let Ok(session_id) = thread.read_with(cx, |thread, _| thread.session_id().clone()) else {
        return;
    };
    if notification.session_id.as_ref() != Some(&session_id) {
        return;
    }
    let Some(message) = notification.params.get("message") else {
        return;
    };
    let Some(activity) = thread_activity_mut(thread_view_id, cx) else {
        return;
    };
    apply_sdk_message(activity, message, Instant::now());
    cx.notify(thread_view_id);
}

pub(crate) fn set_model_picker(thread_view_id: EntityId, picker: Option<AnyView>, cx: &mut App) {
    if let Some(activity) = thread_activity_mut(thread_view_id, cx) {
        if picker.is_none() && activity.model_picker.is_some() {
            activity.model_picker_dismissed_at = Some(Instant::now());
        }
        activity.model_picker = picker;
    }
}

fn restore_zed_placeholder(
    thread_view: &mut ThreadView,
    window: &mut Window,
    cx: &mut Context<ThreadView>,
) {
    let thread_view_id = cx.entity_id();
    let Some(placeholder) = cx
        .default_global::<ClaudeCodeActivity>()
        .threads
        .get_mut(&thread_view_id)
        .and_then(|activity| activity.zed_placeholder.take())
    else {
        return;
    };
    thread_view.message_editor.update(cx, |message_editor, cx| {
        message_editor.set_placeholder_text(&placeholder, window, cx);
    });
}

/// Claude Code writes prompts and sent messages in the UI font, not the buffer font.
pub(crate) fn ui_font_text_style(cx: &App) -> TextStyle {
    // Scaling the agent font size (rather than using a fixed label size) keeps
    // the agent zoom actions working.
    let settings = ThemeSettings::get_global(cx);
    TextStyle {
        color: cx.theme().colors().text,
        font_family: settings.ui_font.family.clone(),
        font_fallbacks: settings.ui_font.fallbacks.clone(),
        font_features: settings.ui_font.features.clone(),
        font_size: (settings.agent_ui_font_size(cx) * TRANSCRIPT_FONT_RATIO).into(),
        font_weight: settings.ui_font.weight,
        line_height: relative(1.5),
        ..Default::default()
    }
}

/// The option a number key picks in a permission card: `1` is the first.
pub(crate) fn permission_option_for_key(key: &str, option_count: usize) -> Option<usize> {
    let [digit @ b'1'..=b'9'] = key.as_bytes() else {
        return None;
    };
    let index = usize::from(digit - b'1');
    (index < option_count).then_some(index)
}

/// The option Esc picks in a permission card: the adapter's "No" choice.
pub(crate) fn reject_option_index(options: &[acp_v2::PermissionOption]) -> Option<usize> {
    options
        .iter()
        .position(|option| option.kind == acp_v2::PermissionOptionKind::RejectOnce)
        .or_else(|| {
            options
                .iter()
                .position(|option| option.kind == acp_v2::PermissionOptionKind::RejectAlways)
        })
}

pub(crate) fn is_allow_option(option: &acp_v2::PermissionOption) -> bool {
    matches!(
        option.kind,
        acp_v2::PermissionOptionKind::AllowOnce | acp_v2::PermissionOptionKind::AllowAlways
    )
}

/// Whether a permission request is claude-agent-acp's ExitPlanMode approval.
pub(crate) fn is_plan_approval(options: &[acp_v2::PermissionOption]) -> bool {
    options
        .iter()
        .any(|option| option.option_id.0.starts_with(EXIT_PLAN_OPTION_PREFIX))
}

/// The labels of the Claude Code extension's thinking block: it rounds to
/// whole seconds (so "Thought for 0s" happens), and says "Thinking" for a
/// finished thought it has no duration for.
pub(crate) fn thought_label(duration: Option<Duration>, is_streaming: bool) -> SharedString {
    if is_streaming {
        return "Thinking...".into();
    }
    match duration {
        Some(duration) => {
            let seconds = (duration.as_millis() + 500) / 1000;
            format!("Thought for {seconds}s").into()
        }
        None => "Thinking".into(),
    }
}

/// Whether a thought carries summary text. claude-agent-acp only sends a
/// thought when Claude streams thinking text, which needs the thinking display
/// that vszed asks for when `showThinkingSummaries` is on; other agents may
/// send empty ones.
pub(crate) fn thought_has_summary(block: &acp_thread::MessageContent) -> bool {
    block.source_blocks().iter().any(|source| match source {
        acp_v2::ContentBlock::Text(text) => !text.text.trim().is_empty(),
        _ => false,
    })
}

pub(crate) fn format_timestamp(time: DateTime<Local>) -> SharedString {
    time.format("%-I:%M %p").to_string().into()
}

/// The date line's label, such as "Sun, Nov 8"; the year is added for
/// another year, as Claude Code does.
pub(crate) fn format_day(time: DateTime<Local>, now: DateTime<Local>) -> SharedString {
    if time.year() == now.year() {
        time.format("%a, %b %-d").to_string().into()
    } else {
        time.format("%a, %b %-d, %Y").to_string().into()
    }
}

/// The name of an image attachment chip. Pasted images have no file name,
/// so they get the one VS Code gives a pasted image, such as "image.png".
pub(crate) fn attachment_name(
    uri: Option<&str>,
    mime_type: &str,
    path_style: PathStyle,
) -> SharedString {
    let mention = uri.and_then(|uri| MentionUri::parse(uri, path_style).log_err());
    match mention {
        Some(MentionUri::File { abs_path }) => {
            if let Some(file_name) = abs_path.file_name() {
                return file_name.to_string_lossy().into_owned().into();
            }
        }
        Some(MentionUri::PastedImage { name }) if name != DEFAULT_PASTED_IMAGE_NAME => {
            return name.into();
        }
        _ => {}
    }
    let extension = match mime_type {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        _ => "png",
    };
    format!("image.{extension}").into()
}

/// Zed's name for every pasted image.
const DEFAULT_PASTED_IMAGE_NAME: &str = "Image";

/// The text of a sent message as Claude Code shows it: what the user typed,
/// with mentions as "@name" and images left to the attachment chips.
pub(crate) fn sent_message_text(blocks: &[acp_v2::ContentBlock], path_style: PathStyle) -> String {
    let mention_name = |uri: &str, fallback: &str| {
        MentionUri::parse(uri, path_style)
            .map(|mention| mention.name())
            .unwrap_or_else(|_| fallback.to_string())
    };
    let mut text = String::new();
    for block in blocks {
        match block {
            acp_v2::ContentBlock::Text(content) => text.push_str(&content.text),
            acp_v2::ContentBlock::ResourceLink(link) => {
                text.push('@');
                text.push_str(&mention_name(&link.uri, &link.name));
            }
            acp_v2::ContentBlock::Resource(acp_v2::EmbeddedResource {
                resource: acp_v2::EmbeddedResourceResource::TextResourceContents(resource),
                ..
            }) => {
                text.push('@');
                text.push_str(&mention_name(&resource.uri, &resource.uri));
            }
            _ => {}
        }
    }
    text.trim().to_string()
}

/// VS Code draws Claude Code's transcript and prompt at its 13px workbench
/// size, which Zed's labels reach at 14/16 of the UI font size.
const TRANSCRIPT_FONT_RATIO: f32 = 14. / 16.;

/// The agent markdown style, scaled to the size of the transcript's labels.
pub(crate) fn transcript_markdown_style(window: &Window, cx: &App) -> MarkdownStyle {
    let mut style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
    let settings = ThemeSettings::get_global(cx);
    let text_size = settings.agent_ui_font_size(cx) * TRANSCRIPT_FONT_RATIO;
    let code_size = settings.agent_buffer_font_size(cx) * TRANSCRIPT_FONT_RATIO;
    style.base_text_style.font_size = text_size.into();
    // `MarkdownStyle::themed` derives the line height from the code size.
    style.base_text_style.line_height = (code_size * 1.75).into();
    style.code_block.text.font_size = Some(code_size.into());
    style.inline_code.font_size = Some(code_size.into());
    style
}

pub(crate) fn claude_code_tool_name(meta: Option<&acp_v2::Meta>) -> Option<SharedString> {
    let name = meta?
        .get(CLAUDE_CODE_META_KEY)?
        .get(CLAUDE_CODE_TOOL_NAME_KEY)?
        .as_str()?
        .trim();
    (!name.is_empty()).then(|| SharedString::from(name.to_string()))
}

fn raw_input_str<'a>(tool_call: &'a ToolCall, key: &str) -> Option<&'a str> {
    tool_call
        .raw_input
        .as_ref()?
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn strip_backticks(text: &str) -> &str {
    let trimmed = text.trim();
    trimmed
        .strip_prefix('`')
        .and_then(|inner| inner.strip_suffix('`'))
        .map(str::trim)
        .unwrap_or(trimmed)
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default().trim()
}

/// Header text for a tool row, mirroring how the Claude Code extension labels steps.
pub(crate) struct ToolCallSummary {
    pub name: SharedString,
    pub description: Option<SharedString>,
    pub input: Option<SharedString>,
}

/// Whether `name` is a tool name Claude Code itself shows. claude-agent-acp
/// reports it as the ACP v2 tool call `name` (and as `_meta.claudeCode.toolName`
/// over ACP v1), while other agents report their own internal names there.
fn is_claude_code_tool_name(name: &str) -> bool {
    CLAUDE_CODE_TOOL_NAMES.contains(&name) || name.starts_with("mcp__")
}

pub(crate) fn summarize_tool_call(tool_call: &ToolCall, label: &str) -> ToolCallSummary {
    let label = label.trim();
    let is_execute = matches!(tool_call.kind(), acp_v2::ToolKind::Execute);

    let reported_name = tool_call
        .name
        .as_ref()
        .or(tool_call.tool_name.as_ref())
        .filter(|name| is_claude_code_tool_name(name.trim()))
        .map(|name| SharedString::from(name.trim().to_string()));
    let name = claude_code_tool_name(tool_call.meta.as_ref())
        .or(reported_name)
        .unwrap_or_else(|| match tool_call.kind() {
            acp_v2::ToolKind::Execute => "Bash".into(),
            acp_v2::ToolKind::Read => "Read".into(),
            acp_v2::ToolKind::Edit | acp_v2::ToolKind::Delete | acp_v2::ToolKind::Move => {
                "Edit".into()
            }
            acp_v2::ToolKind::Search => "Grep".into(),
            acp_v2::ToolKind::Fetch => "WebFetch".into(),
            acp_v2::ToolKind::Think => "Think".into(),
            _ => tool_call
                .tool_name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| SharedString::from(first_line(label).to_string())),
        });

    let input = if is_execute {
        raw_input_str(tool_call, "command")
            .or_else(|| (!label.is_empty()).then(|| strip_backticks(label)))
            .map(|command| SharedString::from(command.to_string()))
    } else {
        ["pattern", "query", "url"]
            .into_iter()
            .find_map(|key| raw_input_str(tool_call, key))
            .map(|value| SharedString::from(value.to_string()))
    };

    let description = raw_input_str(tool_call, "description")
        .map(str::to_string)
        .or_else(|| {
            if is_execute {
                return None;
            }
            let title = first_line(label);
            let title = title
                .strip_prefix(name.as_ref())
                .filter(|rest| rest.starts_with(' '))
                .unwrap_or(title);
            let title = strip_backticks(title);
            (!title.is_empty() && title != name.as_ref()).then(|| title.to_string())
        })
        .map(SharedString::from);

    ToolCallSummary {
        name,
        description,
        input,
    }
}

/// Removes a single fenced code block wrapper, which agents commonly use to
/// send tool output as markdown. claude-agent-acp lengthens the fence when the
/// text itself contains backtick runs, so the closing fence must match the
/// opening one's length.
fn strip_code_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let fence_len = trimmed
        .chars()
        .take_while(|character| *character == '`')
        .count();
    if fence_len < 3 {
        return trimmed;
    }
    let (fence, after_fence) = trimmed.split_at(fence_len);
    let Some((_info, body)) = after_fence.split_once('\n') else {
        return trimmed;
    };
    let body = body.trim_end();
    let Some(body) = body.strip_suffix(fence) else {
        return trimmed;
    };
    if body.ends_with('`') {
        return trimmed;
    }
    body.trim_end_matches(['\n', '\r'])
}

/// Splits tool input or output into the lines to display. CRLF and lone CR
/// line endings both end a line, and tabs are expanded to spaces, because the
/// text is laid out one row per line rather than by a terminal.
pub(crate) fn display_lines(text: &str) -> Vec<String> {
    let text = text.trim_end_matches(['\n', '\r']);
    if text.is_empty() {
        return Vec::new();
    }
    text.split('\n')
        .flat_map(|line| line.strip_suffix('\r').unwrap_or(line).split('\r'))
        .map(expand_tabs)
        .collect()
}

fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return line.to_string();
    }
    let mut expanded = String::with_capacity(line.len() + TAB_WIDTH);
    let mut column = 0;
    for character in line.chars() {
        if character == '\t' {
            let spaces = TAB_WIDTH - column % TAB_WIDTH;
            expanded.extend(std::iter::repeat_n(' ', spaces));
            column += spaces;
        } else {
            expanded.push(character);
            column += 1;
        }
    }
    expanded
}

/// Plain-text output of a tool call for the OUT row. Diffs, images and other
/// rich content are rendered separately.
pub(crate) fn tool_output_text(
    tool_call: &ToolCall,
    skip_text: Option<&str>,
    cx: &App,
) -> Option<SharedString> {
    let mut sections = Vec::new();
    for content in tool_call.content() {
        match content {
            ToolCallContent::ContentBlock { block, .. } => {
                if block.image().is_some() {
                    continue;
                }
                let text = block.to_markdown(cx).trim();
                if text.is_empty() || skip_text.is_some_and(|skip| skip.trim() == text) {
                    continue;
                }
                sections.push(strip_code_fence(text).to_string());
            }
            ToolCallContent::Terminal { terminal, .. } => {
                let terminal = terminal.read(cx);
                let output = match terminal.output() {
                    Some(output) => output.content.clone(),
                    None => terminal.current_output(cx).output,
                };
                let output = output.trim_end();
                if !output.is_empty() {
                    sections.push(output.to_string());
                }
            }
            ToolCallContent::Other { markdown, .. } => {
                let source = markdown.read(cx).source().trim();
                if !source.is_empty() {
                    sections.push(strip_code_fence(source).to_string());
                }
            }
            ToolCallContent::Diff(_)
            | ToolCallContent::LegacyDiff { .. }
            | ToolCallContent::DiffPatch { .. }
            | ToolCallContent::SessionReference(_) => {}
        }
    }
    (!sections.is_empty()).then(|| sections.join("\n").into())
}

/// The title Claude Code gives a tool output tab, such as "Bash tool output".
pub(crate) fn tool_output_title(tool_call: &ToolCall, cx: &App) -> SharedString {
    let label = tool_call.label.read(cx).source().to_string();
    let summary = summarize_tool_call(tool_call, &label);
    format!("{} tool output", summary.name).into()
}

/// Claude Code only offers to open output or input in a tab when it is long:
/// more than 250 characters or more than 3 lines.
pub(crate) fn is_long_tool_text(text: &str) -> bool {
    text.chars().nth(250).is_some() || text.split('\n').nth(3).is_some()
}

fn line_stats(old_text: &str, new_text: &str) -> (usize, usize) {
    language::line_diff(old_text, new_text)
        .into_iter()
        .fold((0, 0), |(added, removed), (old_rows, new_rows)| {
            (added + new_rows.len(), removed + old_rows.len())
        })
}

/// Lines added and removed by an edit, from the diff the agent sent or from
/// Claude Code's `Edit`/`MultiEdit`/`Write` tool input.
pub(crate) fn edit_line_stats(tool_call: &ToolCall) -> Option<(usize, usize)> {
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    for content in tool_call.content() {
        if let ToolCallContent::LegacyDiff { source, .. } = content {
            pairs.push((
                source.old_text.as_deref().unwrap_or_default(),
                &source.new_text,
            ));
        }
    }
    if pairs.is_empty()
        && let Some(raw_input) = tool_call.raw_input.as_ref()
    {
        fn string_field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
            value.get(key)?.as_str()
        }
        if let Some(edits) = raw_input.get("edits").and_then(|edits| edits.as_array()) {
            for edit in edits {
                if let (Some(old_text), Some(new_text)) = (
                    string_field(edit, "old_string"),
                    string_field(edit, "new_string"),
                ) {
                    pairs.push((old_text, new_text));
                }
            }
        } else if let (Some(old_text), Some(new_text)) = (
            string_field(raw_input, "old_string"),
            string_field(raw_input, "new_string"),
        ) {
            pairs.push((old_text, new_text));
        } else if let Some(content) = string_field(raw_input, "content") {
            pairs.push(("", content));
        }
    }

    let total_bytes: usize = pairs
        .iter()
        .map(|(old_text, new_text)| old_text.len() + new_text.len())
        .sum();
    if pairs.is_empty() || total_bytes > MAX_DIFF_STATS_INPUT_BYTES {
        return None;
    }
    Some(
        pairs
            .into_iter()
            .fold((0, 0), |(added, removed), (old_text, new_text)| {
                let (pair_added, pair_removed) = line_stats(old_text, new_text);
                (added + pair_added, removed + pair_removed)
            }),
    )
}

/// The value of one entry of a select config option.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConfigOptionEntry {
    pub value: acp_v2::SessionConfigValueId,
    pub name: String,
    pub description: Option<String>,
}

pub(crate) fn select_entries(select: &acp_v2::SessionConfigSelect) -> Vec<ConfigOptionEntry> {
    let to_entry = |option: &acp_v2::SessionConfigSelectOption| ConfigOptionEntry {
        value: option.value.clone(),
        name: option.name.clone(),
        description: option.description.clone(),
    };
    match &select.options {
        acp_v2::SessionConfigSelectOptions::Ungrouped(options) => {
            options.iter().map(to_entry).collect()
        }
        acp_v2::SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter().map(to_entry))
            .collect(),
        _ => Vec::new(),
    }
}

fn current_select_entry(option: &acp_v2::SessionConfigOption) -> Option<ConfigOptionEntry> {
    let acp_v2::SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };
    select_entries(select)
        .into_iter()
        .find(|entry| entry.value == select.current_value)
}

pub(crate) fn first_select_option<'a>(
    options: &'a [acp_v2::SessionConfigOption],
    category: &acp_v2::SessionConfigOptionCategory,
) -> Option<&'a acp_v2::SessionConfigOption> {
    options.iter().find(|option| {
        option.category.as_ref() == Some(category)
            && matches!(option.kind, acp_v2::SessionConfigKind::Select(_))
    })
}

/// Whether a toggle-like option is on. claude-agent-acp sends Fast mode and
/// Ultracode as booleans to clients that support them, and as an "on"/"off"
/// select otherwise.
pub(crate) fn toggle_option_state(option: &acp_v2::SessionConfigOption) -> Option<bool> {
    match &option.kind {
        acp_v2::SessionConfigKind::Boolean(boolean) => Some(boolean.current_value),
        acp_v2::SessionConfigKind::Select(select) => {
            let entries = select_entries(select);
            let is_on_off = entries.len() == 2
                && entries
                    .iter()
                    .all(|entry| matches!(entry.value.0.as_ref(), "on" | "off"));
            is_on_off.then(|| select.current_value.0.as_ref() == "on")
        }
        _ => None,
    }
}

/// The value that flips a toggle-like option.
pub(crate) fn toggled_option_value(
    option: &acp_v2::SessionConfigOption,
    enabled: bool,
) -> Option<acp_v2::SessionConfigOptionValue> {
    match &option.kind {
        acp_v2::SessionConfigKind::Boolean(_) => {
            Some(acp_v2::SessionConfigOptionValue::boolean(enabled))
        }
        acp_v2::SessionConfigKind::Select(_) => {
            Some(acp_v2::SessionConfigOptionValue::id(if enabled {
                "on"
            } else {
                "off"
            }))
        }
        _ => None,
    }
}

/// Claude Code's name for an effort level. claude-agent-acp title-cases the
/// SDK's level ids, which turns `xhigh` into "Xhigh".
pub(crate) fn effort_display_name(entry: &ConfigOptionEntry) -> SharedString {
    match entry.value.0.as_ref() {
        "xhigh" => "Extra high".into(),
        _ => entry.name.clone().into(),
    }
}

/// The model name the chip shows. claude-agent-acp lists the account default
/// as "Default (recommended)" and puts the name of the model it resolves to in
/// the description ("Opus"); Claude Code shows that model with its version, as
/// the matching family row names it ("Opus 5.5").
fn model_display_name(entry: &ConfigOptionEntry, entries: &[ConfigOptionEntry]) -> SharedString {
    if entry.value.0.as_ref() == "default"
        && let Some(description) = entry
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
    {
        return default_model_row(description, entries)
            .map(|row| row.name.clone())
            .unwrap_or_else(|| description.to_string())
            .into();
    }
    entry
        .name
        .strip_suffix(" (recommended)")
        .unwrap_or(&entry.name)
        .to_string()
        .into()
}

/// The row the `default` model resolves to, from the default row's description: the
/// model's family name ("Opus" names "Opus 5.5") or its full name.
fn default_model_row<'a>(
    description: &str,
    entries: &'a [ConfigOptionEntry],
) -> Option<&'a ConfigOptionEntry> {
    let rows = || {
        entries
            .iter()
            .filter(|entry| entry.value.0.as_ref() != "default")
    };
    rows()
        .find(|entry| entry.name == description)
        .or_else(|| rows().find(|entry| strip_model_version(&entry.name) == description))
}

/// "Opus 5.5" without its version: "Opus".
fn strip_model_version(name: &str) -> &str {
    match name.rsplit_once(' ') {
        Some((family, version))
            if !version.is_empty()
                && version
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == '.') =>
        {
            family
        }
        _ => name,
    }
}

/// The parts of the model pill: "Opus 5.5", "Extra high" (muted) and whether
/// Ultracode is on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModelChipParts {
    pub model: Option<SharedString>,
    pub effort: Option<SharedString>,
    pub ultracode: bool,
}

pub(crate) fn model_chip_parts(options: &[acp_v2::SessionConfigOption]) -> Option<ModelChipParts> {
    let model_option = first_select_option(options, &acp_v2::SessionConfigOptionCategory::Model);
    let model_entries = match model_option.map(|option| &option.kind) {
        Some(acp_v2::SessionConfigKind::Select(select)) => select_entries(select),
        _ => Vec::new(),
    };
    let model = model_option
        .and_then(current_select_entry)
        .map(|entry| model_display_name(&entry, &model_entries));
    let effort = first_select_option(options, &acp_v2::SessionConfigOptionCategory::ThoughtLevel)
        .and_then(current_select_entry)
        .filter(|entry| entry.value.0.as_ref() != "default")
        .map(|entry| effort_display_name(&entry));
    let ultracode = options
        .iter()
        .find(|option| option.config_id.0.as_ref() == ULTRACODE_CONFIG_ID)
        .and_then(toggle_option_state)
        .unwrap_or(false);
    if model.is_none() && effort.is_none() {
        return None;
    }
    Some(ModelChipParts {
        model,
        effort,
        ultracode,
    })
}

/// The label of the model chip: "Opus 5.5 Extra high · Ultracode".
#[cfg(test)]
pub(crate) fn model_chip_label(options: &[acp_v2::SessionConfigOption]) -> Option<SharedString> {
    let model_option = first_select_option(options, &acp_v2::SessionConfigOptionCategory::Model);
    let model_entries = match model_option.map(|option| &option.kind) {
        Some(acp_v2::SessionConfigKind::Select(select)) => select_entries(select),
        _ => Vec::new(),
    };
    let model = model_option
        .and_then(current_select_entry)
        .map(|entry| model_display_name(&entry, &model_entries));
    let effort = first_select_option(options, &acp_v2::SessionConfigOptionCategory::ThoughtLevel)
        .and_then(current_select_entry)
        .filter(|entry| entry.value.0.as_ref() != "default")
        .map(|entry| effort_display_name(&entry));
    let ultracode = options
        .iter()
        .find(|option| option.config_id.0.as_ref() == ULTRACODE_CONFIG_ID)
        .and_then(toggle_option_state)
        .unwrap_or(false);
    if model.is_none() && effort.is_none() {
        return None;
    }
    let mut label = model.map(|model| model.to_string()).unwrap_or_default();
    if let Some(effort) = effort {
        if !label.is_empty() {
            label.push(' ');
        }
        label.push_str(&effort);
    }
    if ultracode {
        label.push_str(" · Ultracode");
    }
    Some(label.into())
}

/// The Claude Code VS Code extension's names for the permission modes, keyed by
/// the claude-agent-acp mode ids. The adapter's own names differ for
/// `acceptEdits` ("Accept edits").
pub(crate) fn mode_display_name(mode_id: &str, adapter_name: &str) -> SharedString {
    match mode_id {
        "default" => "Manual".into(),
        "acceptEdits" => "Edit automatically".into(),
        "plan" => "Plan".into(),
        "auto" => "Auto".into(),
        "bypassPermissions" => "Bypass permissions".into(),
        _ => adapter_name.to_string().into(),
    }
}

pub(crate) fn mode_icon(mode_id: &str) -> IconName {
    match mode_id {
        "default" => IconName::UserCheck,
        "acceptEdits" => IconName::Pencil,
        "plan" => IconName::ListTodo,
        "auto" => IconName::BoltOutlined,
        "bypassPermissions" => IconName::Warning,
        _ => IconName::Circle,
    }
}

pub(crate) const QUEUE_PLACEHOLDER: &str = "Queue another message…";

/// Claude Code's idle placeholder names the shortcut that moves focus between the
/// editor and Claude: "⌘ Esc" on macOS and "ctrl esc" elsewhere.
pub(crate) fn idle_placeholder() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘ Esc to focus or unfocus Claude"
    } else {
        "ctrl esc to focus or unfocus Claude"
    }
}

pub(crate) fn is_claude_code_placeholder(text: &str) -> bool {
    text == QUEUE_PLACEHOLDER || text.ends_with(" to focus or unfocus Claude")
}

pub(crate) fn remember_zed_placeholder(
    thread_view_id: EntityId,
    placeholder: String,
    cx: &mut App,
) {
    if let Some(activity) = cx
        .default_global::<ClaudeCodeActivity>()
        .threads
        .get_mut(&thread_view_id)
    {
        activity.zed_placeholder = Some(placeholder);
    }
}

// --- Model picker ---------------------------------------------------------------------

/// claude-agent-acp's model family aliases (`MODEL_FAMILY_ALIASES`,
/// `src/session-model.ts`). An alias always runs its family's newest model.
const MODEL_FAMILY_ALIASES: [&str; 4] = ["opus", "sonnet", "haiku", "fable"];

/// One row of the model picker.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModelMenuRow {
    pub value: acp_v2::SessionConfigValueId,
    pub title: SharedString,
    pub subtitle: Option<SharedString>,
    pub is_current: bool,
}

/// The family of a model option value, and the version of a pinned one: `opus` and
/// `opus[1m]` are the Opus alias, `claude-fable-5-1[1m]` is Fable 5.1 and
/// `claude-haiku-4-5-20251001` is Haiku 4.5. `None` for other values, such as
/// `default` or a custom model.
fn model_family_version(value: &str) -> Option<(&'static str, Option<(u32, u32)>)> {
    let value = value.split('[').next().unwrap_or(value);
    if let Some(alias) = MODEL_FAMILY_ALIASES.iter().find(|alias| **alias == value) {
        return Some((alias, None));
    }
    let mut parts = value.strip_prefix("claude-")?.split('-');
    let family = parts.next()?;
    let family = MODEL_FAMILY_ALIASES
        .iter()
        .find(|alias| **alias == family)?;
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts
        .next()
        .filter(|part| part.len() <= 2)
        .and_then(|part| part.parse::<u32>().ok())
        .unwrap_or(0);
    Some((family, Some((major, minor))))
}

/// The rows of the model picker. Claude Code lists Default and one row per model family
/// ("Opus", "Fable", "Sonnet", "Haiku"), titled with the family and described as
/// "<model> · <what it is for>". claude-agent-acp can also list pinned older versions
/// (`claude-opus-4-8`) next to the family aliases; those are left out, and a family
/// without an alias is shown by its newest version. The current model always stays, so
/// the check has a row.
pub(crate) fn model_menu_rows(select: &acp_v2::SessionConfigSelect) -> Vec<ModelMenuRow> {
    let entries = select_entries(select);
    let mut families_with_alias = Vec::new();
    let mut newest_versions: HashMap<&'static str, (u32, u32)> = HashMap::default();
    for entry in &entries {
        match model_family_version(&entry.value.0) {
            Some((family, None)) => families_with_alias.push(family),
            Some((family, Some(version))) => {
                let newest = newest_versions.entry(family).or_insert(version);
                *newest = (*newest).max(version);
            }
            None => {}
        }
    }

    let mut shown_families = Vec::new();
    let mut rows = Vec::new();
    for entry in &entries {
        let is_current = entry.value == select.current_value;
        let is_family_row = match model_family_version(&entry.value.0) {
            Some((family, version)) => {
                let represents_family = match version {
                    None => true,
                    Some(version) => {
                        !families_with_alias.contains(&family)
                            && newest_versions.get(family) == Some(&version)
                    }
                };
                let shown = represents_family && !shown_families.contains(&family);
                if shown {
                    shown_families.push(family);
                } else if !is_current {
                    continue;
                }
                shown
            }
            None => false,
        };
        let title = if is_family_row {
            strip_model_version(&entry.name).to_string()
        } else {
            entry.name.clone()
        };
        let subtitle = if entry.value.0.as_ref() == "default" {
            entry.description.as_deref().map(|description| {
                default_model_row(description, &entries)
                    .and_then(|row| row.description.clone())
                    .unwrap_or_else(|| description.to_string())
            })
        } else {
            entry.description.clone()
        };
        rows.push(ModelMenuRow {
            value: entry.value.clone(),
            title: title.into(),
            subtitle: subtitle
                .filter(|subtitle| !subtitle.trim().is_empty())
                .map(SharedString::from),
            is_current,
        });
    }
    rows
}

/// The Effort row: the model's levels without `default`, the current one, and its name
/// for the "(Extra high)" suffix.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EffortSlider {
    pub levels: Vec<ConfigOptionEntry>,
    pub current: Option<usize>,
    pub label: SharedString,
}

pub(crate) fn effort_slider(option: &acp_v2::SessionConfigOption) -> Option<EffortSlider> {
    let acp_v2::SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };
    let levels = select_entries(select)
        .into_iter()
        .filter(|entry| entry.value.0.as_ref() != "default")
        .collect::<Vec<_>>();
    if levels.is_empty() {
        return None;
    }
    let current = levels
        .iter()
        .position(|level| level.value == select.current_value);
    // Claude Code calls an effort that was never chosen "Auto".
    let label = current
        .and_then(|current| levels.get(current))
        .map(effort_display_name)
        .unwrap_or_else(|| "Auto".into());
    Some(EffortSlider {
        levels,
        current,
        label,
    })
}

// --- Colors ---------------------------------------------------------------------------

/// The extension's `--app-claude-orange`: the prompt box focus ring in manual mode.
const CLAUDE_ORANGE: u32 = 0xd97757ff;
/// `--app-claude-clay-button-orange`: the send button in manual mode.
const CLAUDE_CLAY_BUTTON_ORANGE: u32 = 0xc6613fff;
/// `--app-claude-ivory`: the arrow on the clay send button.
const CLAUDE_IVORY: u32 = 0xfaf9f5ff;
/// `--app-status-busy` and `--app-status-pending`, the agent status dots.
const STATUS_BUSY: u32 = 0x89d185ff;
const STATUS_PENDING: u32 = 0x3b82f6ff;

pub(crate) fn claude_orange() -> Hsla {
    rgba(CLAUDE_ORANGE).into()
}

/// The prompt box's focus ring and send button colors, which follow the permission
/// mode in the extension: Claude orange in manual mode, the text color for "Edit
/// automatically", the focus color for Plan, and the error color for Auto and Bypass.
pub(crate) struct PromptAccent {
    pub ring: Hsla,
    pub button_background: Hsla,
    pub button_foreground: Hsla,
}

pub(crate) fn prompt_accent(mode_id: Option<&str>, cx: &App) -> PromptAccent {
    let colors = cx.theme().colors();
    // The extension draws the Plan, Auto and Bypass send icons in VS Code's
    // `button.foreground`, which is white in the default themes. Zed themes have no
    // equivalent token.
    let button_foreground = gpui::white();
    match mode_id {
        Some("acceptEdits") => PromptAccent {
            ring: colors.text,
            button_background: colors.text,
            button_foreground: colors.editor_background,
        },
        Some("plan") => PromptAccent {
            ring: colors.border_focused,
            button_background: colors.border_focused,
            button_foreground,
        },
        Some("auto" | "bypassPermissions") => {
            let error = cx.theme().status().error;
            PromptAccent {
                ring: error,
                button_background: error,
                button_foreground,
            }
        }
        _ => PromptAccent {
            ring: claude_orange(),
            button_background: rgba(CLAUDE_CLAY_BUTTON_ORANGE).into(),
            button_foreground: rgba(CLAUDE_IVORY).into(),
        },
    }
}

// --- Prompt footer ----------------------------------------------------------------------

/// claude-agent-acp's command that compacts the conversation.
pub(crate) const COMPACT_COMMAND: &str = "compact";

/// What "Add context" types after the text before the cursor: an `@`, after a space
/// when the text doesn't end in whitespace, because Zed's mention menu (like the
/// extension's) only opens for an `@` that starts a word.
pub(crate) fn mention_trigger_text(previous_character: Option<char>) -> &'static str {
    match previous_character {
        Some(character) if !character.is_whitespace() => " @",
        _ => "@",
    }
}

// --- Context ring -----------------------------------------------------------------------

/// Claude Code counts the context as full this many tokens before the model's window
/// ends: the auto-compact buffer, plus the output reserve below.
const AUTO_COMPACT_BUFFER_TOKENS: u64 = 13_000;
/// The output reserve is the model's output limit, capped at this. ACP doesn't send the
/// limit, and every current Claude model's is above the cap.
const MAX_OUTPUT_RESERVE_TOKENS: u64 = 20_000;

/// How much of the context Claude Code's footer ring shows as used: the share of the
/// window left after the output reserve and the auto-compact buffer, in percent and at
/// most 100. The ring only appears once less than half of it remains.
pub(crate) fn context_ring_percentage(
    used_tokens: u64,
    context_window: u64,
    max_output_tokens: Option<u64>,
) -> Option<f32> {
    let reserve = max_output_tokens
        .unwrap_or(MAX_OUTPUT_RESERVE_TOKENS)
        .min(MAX_OUTPUT_RESERVE_TOKENS)
        + AUTO_COMPACT_BUFFER_TOKENS;
    let usable = context_window
        .checked_sub(reserve)
        .filter(|usable| *usable > 0)?;
    let percentage = (used_tokens as f32 / usable as f32 * 100.).min(100.);
    (100. - percentage < 50.).then_some(percentage)
}

/// The ring's tooltip, as the extension words it.
pub(crate) fn context_ring_tooltip(percentage_used: f32) -> String {
    format!(
        "{}% of context remaining until auto-compact.",
        (100. - percentage_used).round()
    )
}

/// The extension draws the ring in three steps rather than continuously: an orange arc
/// from the top, clockwise, and a faint track over most of the rest. Angles are in
/// degrees clockwise from the top, read off the extension's arcs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ContextRingArcs {
    pub used_end: f32,
    pub track: Option<(f32, f32)>,
}

pub(crate) fn context_ring_arcs(percentage_used: f32) -> ContextRingArcs {
    if percentage_used < 62.5 {
        ContextRingArcs {
            used_end: 180.,
            track: Some((210.7, 326.8)),
        }
    } else if percentage_used < 87. {
        ContextRingArcs {
            used_end: 252.5,
            track: Some((284.8, 326.8)),
        }
    } else {
        ContextRingArcs {
            used_end: 315.,
            track: None,
        }
    }
}

/// The extension's 20px ring icon, drawn at its 26px button size.
pub(crate) const CONTEXT_RING_BOX: Pixels = px(26.);
const CONTEXT_RING_RADIUS: f32 = 5. * 26. / 20.;
const CONTEXT_RING_STROKE: f32 = 1.5 * 26. / 20.;

#[derive(IntoElement)]
pub(crate) struct ContextRing {
    percentage_used: f32,
}

impl ContextRing {
    pub(crate) fn new(percentage_used: f32) -> Self {
        Self { percentage_used }
    }
}

fn paint_ring_arc(
    center: gpui::Point<Pixels>,
    start_degrees: f32,
    end_degrees: f32,
    color: Hsla,
    window: &mut Window,
) {
    let point_at = |degrees: f32| {
        let radians = degrees.to_radians();
        gpui::point(
            center.x + px(CONTEXT_RING_RADIUS * radians.sin()),
            center.y - px(CONTEXT_RING_RADIUS * radians.cos()),
        )
    };
    let mut builder = gpui::PathBuilder::stroke(px(CONTEXT_RING_STROKE)).with_style(
        gpui::PathStyle::Stroke(gpui::StrokeOptions {
            line_width: CONTEXT_RING_STROKE,
            line_cap: gpui::LineCap::Round,
            ..Default::default()
        }),
    );
    builder.move_to(point_at(start_degrees));
    builder.arc_to(
        gpui::point(px(CONTEXT_RING_RADIUS), px(CONTEXT_RING_RADIUS)),
        px(0.),
        end_degrees - start_degrees > 180.,
        true,
        point_at(end_degrees),
    );
    match builder.build() {
        Ok(path) => window.paint_path(path, color),
        Err(error) => log::debug!("failed to build the context ring: {error}"),
    }
}

impl RenderOnce for ContextRing {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let arcs = context_ring_arcs(self.percentage_used);
        let used_color: Hsla = rgba(CLAUDE_CLAY_BUTTON_ORANGE).into();
        let track_color = cx.theme().colors().text.opacity(0.15);
        gpui::canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let center = bounds.center();
                if let Some((start, end)) = arcs.track {
                    paint_ring_arc(center, start, end, track_color, window);
                }
                paint_ring_arc(center, 0., arcs.used_end, used_color, window);
            },
        )
        .size(CONTEXT_RING_BOX)
        .flex_none()
    }
}

// --- Background tasks (raw SDK messages) ------------------------------------------

/// claude-agent-acp's extension notification that carries a raw SDK message.
const CLAUDE_SDK_MESSAGE_METHOD: &str = "_claude/sdkMessage";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentStatus {
    Working,
    Finished,
    Failed,
    Stopped,
}

/// A Claude Code task (`SDKTaskStartedMessage` and its updates).
#[derive(Clone, Debug)]
struct SdkTask {
    task_id: String,
    tool_use_id: Option<String>,
    description: String,
    /// Set for subagents (`local_agent` tasks).
    subagent_type: Option<String>,
    /// Such as `local_bash` or `local_agent`.
    task_type: Option<String>,
    /// The subagent task that launched this one.
    parent_task_id: Option<String>,
    backgrounded: bool,
    ambient: bool,
    status: AgentStatus,
    error: Option<String>,
    started_at: Instant,
    ended_at: Option<Instant>,
    total_tokens: Option<u64>,
    duration: Option<Duration>,
    summary: Option<String>,
    /// The latest progress line, such as "Running Show the build log".
    activity: Option<String>,
}

fn json_str<'a>(message: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    message
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn json_bool(message: &serde_json::Value, key: &str) -> Option<bool> {
    message.get(key).and_then(serde_json::Value::as_bool)
}

/// The task usage of `task_progress` and `task_notification`: tokens and run time.
fn task_usage(message: &serde_json::Value) -> Option<(u64, Duration)> {
    let usage = message.get("usage")?;
    let total_tokens = usage.get("total_tokens")?.as_u64()?;
    let duration_ms = usage.get("duration_ms")?.as_u64()?;
    Some((total_tokens, Duration::from_millis(duration_ms)))
}

/// Applies one raw SDK message: the task lifecycle (`task_started`, `task_progress`,
/// `task_updated`, `task_notification`) and each turn's `result`, whose usage tells
/// how long the prompt cache lives (`cache_creation` split by lifetime).
fn apply_sdk_message(activity: &mut ThreadActivity, message: &serde_json::Value, now: Instant) {
    match (json_str(message, "type"), json_str(message, "subtype")) {
        (Some("result"), _) => {
            if let Some(ttl) = message.get("usage").and_then(cache_ttl_from_usage) {
                activity.prompt_cache.ttl = Some(ttl);
            }
        }
        (Some("system"), Some("task_started")) => {
            let Some(task_id) = json_str(message, "task_id") else {
                return;
            };
            let task = SdkTask {
                task_id: task_id.to_string(),
                tool_use_id: json_str(message, "tool_use_id").map(str::to_string),
                description: json_str(message, "description")
                    .unwrap_or("Agent")
                    .to_string(),
                subagent_type: json_str(message, "subagent_type").map(str::to_string),
                task_type: json_str(message, "task_type").map(str::to_string),
                parent_task_id: json_str(message, "parent_task_id").map(str::to_string),
                backgrounded: json_bool(message, "is_backgrounded").unwrap_or(false),
                ambient: json_bool(message, "ambient").unwrap_or(false)
                    || json_bool(message, "skip_transcript").unwrap_or(false),
                status: AgentStatus::Working,
                error: None,
                started_at: now,
                ended_at: None,
                total_tokens: None,
                duration: None,
                summary: None,
                activity: None,
            };
            match activity
                .sdk_tasks
                .iter_mut()
                .find(|existing| existing.task_id == task.task_id)
            {
                // A resumed subagent starts again under the same task id.
                Some(existing) => {
                    existing.status = AgentStatus::Working;
                    existing.ended_at = None;
                    existing.backgrounded = task.backgrounded;
                }
                None => activity.sdk_tasks.push(task),
            }
        }
        (Some("system"), Some("task_progress")) => {
            let Some(task) = find_task(activity, message) else {
                return;
            };
            if let Some((total_tokens, duration)) = task_usage(message) {
                task.total_tokens = Some(total_tokens);
                task.duration = Some(duration);
            }
            if let Some(summary) = json_str(message, "summary") {
                task.summary = Some(summary.to_string());
            }
            let progress = json_str(message, "description")
                .filter(|description| *description != task.description)
                .or_else(|| json_str(message, "last_tool_name"));
            if let Some(progress) = progress {
                task.activity = Some(progress.to_string());
            }
        }
        (Some("system"), Some("task_updated")) => {
            let Some(task) = find_task(activity, message) else {
                return;
            };
            let Some(patch) = message.get("patch") else {
                return;
            };
            if let Some(backgrounded) = json_bool(patch, "is_backgrounded") {
                task.backgrounded = backgrounded;
            }
            if let Some(description) = json_str(patch, "description") {
                task.description = description.to_string();
            }
            match json_str(patch, "status") {
                Some("completed") => task.finish(AgentStatus::Finished, now),
                Some("failed") => {
                    task.error = json_str(patch, "error").map(str::to_string);
                    task.finish(AgentStatus::Failed, now);
                }
                Some("killed") => task.finish(AgentStatus::Stopped, now),
                Some("running" | "pending") => {
                    task.status = AgentStatus::Working;
                    task.ended_at = None;
                }
                _ => {}
            }
        }
        (Some("system"), Some("task_notification")) => {
            let Some(task) = find_task(activity, message) else {
                return;
            };
            if let Some((total_tokens, duration)) = task_usage(message) {
                task.total_tokens = Some(total_tokens);
                task.duration = Some(duration);
            }
            if let Some(summary) = json_str(message, "summary") {
                task.summary = Some(summary.to_string());
            }
            match json_str(message, "status") {
                Some("completed") => task.finish(AgentStatus::Finished, now),
                Some("failed") => task.finish(AgentStatus::Failed, now),
                Some("stopped") => task.finish(AgentStatus::Stopped, now),
                _ => {}
            }
        }
        (Some("system"), Some("background_tasks_changed")) => {
            // A level signal with replace semantics: a background task missing from it
            // ended, even if its end message was lost.
            let Some(live) = message.get("tasks").and_then(serde_json::Value::as_array) else {
                return;
            };
            let live_ids = live
                .iter()
                .filter_map(|task| json_str(task, "task_id"))
                .collect::<Vec<_>>();
            for task in &mut activity.sdk_tasks {
                if task.status == AgentStatus::Working
                    && task.backgrounded
                    && !live_ids.contains(&task.task_id.as_str())
                {
                    task.finish(AgentStatus::Finished, now);
                }
            }
        }
        _ => {}
    }
}

fn find_task<'a>(
    activity: &'a mut ThreadActivity,
    message: &serde_json::Value,
) -> Option<&'a mut SdkTask> {
    let task_id = json_str(message, "task_id")?;
    activity
        .sdk_tasks
        .iter_mut()
        .find(|task| task.task_id == task_id)
}

impl SdkTask {
    fn finish(&mut self, status: AgentStatus, now: Instant) {
        if self.status == AgentStatus::Working || self.ended_at.is_none() {
            self.ended_at = Some(now);
        }
        self.status = status;
    }
}

// --- Prompt cache clock -------------------------------------------------------------

/// The extension's prompt cache lifetimes.
const CACHE_TTL_FIVE_MINUTES: Duration = Duration::from_secs(5 * 60);
const CACHE_TTL_ONE_HOUR: Duration = Duration::from_secs(60 * 60);

/// The lifetime of the cache a response wrote, from its usage: one hour if it wrote any
/// one-hour cache tokens, five minutes if it wrote five-minute ones. A response that
/// neither read nor wrote the cache says nothing about it.
fn cache_ttl_from_usage(usage: &serde_json::Value) -> Option<Duration> {
    let tokens =
        |value: Option<&serde_json::Value>| value.and_then(serde_json::Value::as_u64).unwrap_or(0);
    let creation = usage.get("cache_creation")?;
    if tokens(creation.get("ephemeral_1h_input_tokens")) > 0 {
        Some(CACHE_TTL_ONE_HOUR)
    } else if tokens(creation.get("ephemeral_5m_input_tokens")) > 0 {
        Some(CACHE_TTL_FIVE_MINUTES)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct PromptCache {
    /// When the latest model request was made, which restarts the cache's lifetime.
    request_at: Option<Instant>,
    /// The cache lifetime, when the adapter reported one.
    ttl: Option<Duration>,
    compacted_at: Option<Instant>,
}

/// The prompt cache clock, as the extension computes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CacheClock {
    Unknown,
    Warm { minutes_left: u64 },
    Expired { idle: Duration },
    Compacted,
}

impl PromptCache {
    fn record_request(&mut self, now: Instant) {
        self.request_at = Some(now);
        self.compacted_at = None;
    }

    fn record_compaction(&mut self, now: Instant) {
        self.compacted_at = Some(now);
    }

    /// Without a reported lifetime (ACP v2 reports none) the clock assumes one hour,
    /// Claude Code's lifetime for subscription accounts.
    fn clock(&self, now: Instant) -> CacheClock {
        if self.compacted_at.is_some() {
            return CacheClock::Compacted;
        }
        let Some(request_at) = self.request_at else {
            return CacheClock::Unknown;
        };
        let ttl = self.ttl.unwrap_or(CACHE_TTL_ONE_HOUR);
        let elapsed = now.saturating_duration_since(request_at);
        match ttl.checked_sub(elapsed).filter(|left| !left.is_zero()) {
            Some(left) => CacheClock::Warm {
                minutes_left: left.as_millis().div_ceil(60_000) as u64,
            },
            None => CacheClock::Expired { idle: elapsed },
        }
    }
}

impl CacheClock {
    pub(crate) fn label(&self) -> Option<SharedString> {
        match self {
            Self::Warm { minutes_left } => Some(format!("{minutes_left}m").into()),
            _ => None,
        }
    }

    pub(crate) fn tooltip(&self) -> Option<SharedString> {
        match self {
            Self::Unknown => None,
            Self::Warm { minutes_left } => {
                Some(format!("Prompt cache warm, about {minutes_left} min left.").into())
            }
            Self::Expired { idle } => Some(
                format!(
                    "Prompt cache likely expired (idle {}).",
                    format_idle_time(*idle)
                )
                .into(),
            ),
            Self::Compacted => {
                Some("Prompt cache does not cover the compacted conversation.".into())
            }
        }
    }
}

/// "18m", "1h 18m" or "2d 3h", like the extension's idle time.
pub(crate) fn format_idle_time(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h {}m", minutes % 60);
    }
    format!("{}d {}h", hours / 24, hours % 24)
}

// --- Agents ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct SubagentTimes {
    started_at: Instant,
    last_activity_at: Instant,
}

/// A subagent of the session, for the "N agents" chip, the agent map and the
/// background line under its tool call.
#[derive(Clone, Debug)]
pub(crate) struct AgentSummary {
    pub key: SharedString,
    /// The tool call that launched it (ACP v1, where the task's `tool_use_id` is the
    /// tool call id).
    pub tool_call_id: Option<acp_v2::ToolCallId>,
    /// Its child session (ACP v2).
    pub session_id: Option<acp_v2::SessionId>,
    /// The agent that launched it, if another agent did.
    pub parent_key: Option<SharedString>,
    pub description: SharedString,
    pub status: AgentStatus,
    pub waiting: bool,
    pub backgrounded: bool,
    pub elapsed: Option<Duration>,
    pub total_tokens: Option<u64>,
    pub activity: Option<SharedString>,
    pub summary: Option<SharedString>,
    pub error: Option<SharedString>,
}

/// The status dot states of the extension (`statusDot*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DotState {
    Running,
    Waiting,
    Idle,
    Failed,
}

impl DotState {
    pub(crate) fn color(self, cx: &App) -> Hsla {
        match self {
            Self::Running => rgba(STATUS_BUSY).into(),
            Self::Waiting => rgba(STATUS_PENDING).into(),
            Self::Idle => cx.theme().colors().text_muted.opacity(0.4),
            Self::Failed => cx.theme().status().error,
        }
    }
}

impl AgentSummary {
    pub(crate) fn dot_state(&self) -> DotState {
        if self.waiting {
            return DotState::Waiting;
        }
        match self.status {
            AgentStatus::Working => DotState::Running,
            AgentStatus::Finished => DotState::Idle,
            AgentStatus::Failed | AgentStatus::Stopped => DotState::Failed,
        }
    }

    /// "96m 14s · 177.3k tokens": run time and tokens, each left out when unknown.
    pub(crate) fn meta(&self) -> String {
        let elapsed = self
            .elapsed
            .filter(|elapsed| elapsed.as_millis() >= 1000)
            .map(format_agent_duration);
        let tokens = self
            .total_tokens
            .filter(|tokens| *tokens > 0)
            .map(|tokens| format!("{} tokens", format_token_count(tokens)));
        [elapsed, tokens]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// The status line Claude Code shows under a subagent that runs in the background.
    pub(crate) fn background_line(&self) -> String {
        let status = if self.waiting {
            "Waiting for your permission: see the request below".to_string()
        } else {
            match self.status {
                AgentStatus::Working => match self
                    .summary
                    .as_ref()
                    .or(self.activity.as_ref())
                    .filter(|activity| **activity != self.description)
                {
                    Some(activity) => format!("Working in the background: {activity}"),
                    None => "Working in the background".to_string(),
                },
                AgentStatus::Failed => match &self.error {
                    Some(error) => format!("Failed: {error}"),
                    None => "Failed".to_string(),
                },
                AgentStatus::Finished => "Finished".to_string(),
                AgentStatus::Stopped => "Stopped".to_string(),
            }
        };
        format!("{status} · type /tasks to see its transcript")
    }
}

/// "45s" or "96m 14s".
pub(crate) fn format_agent_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

/// "950", "177.3k" or "1.2M".
pub(crate) fn format_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.)
    } else if tokens >= 1000 {
        format!("{:.1}k", tokens as f64 / 1000.)
    } else {
        tokens.to_string()
    }
}

/// "1 agent", "13 agents".
pub(crate) fn agent_count_label(count: usize) -> String {
    if count == 1 {
        "1 agent".to_string()
    } else {
        format!("{count} agents")
    }
}

fn subagent_title(thread: &AcpThread) -> SharedString {
    thread
        .subagent_association()
        .and_then(|association| association.title.clone())
        .or_else(|| thread.title())
        .unwrap_or_else(|| "Agent".into())
}

/// The session's subagents: Claude Code's subagent tasks from raw SDK messages (ACP v1)
/// or the subagent sessions of an ACP v2 session.
pub(crate) fn agent_summaries(
    thread: &AcpThread,
    activity: Option<&ThreadActivity>,
    now: Instant,
    cx: &App,
) -> Vec<AgentSummary> {
    let sdk_agents = activity
        .map(|activity| {
            activity
                .sdk_tasks
                .iter()
                .filter(|task| task.subagent_type.is_some() && !task.ambient)
                .map(|task| AgentSummary {
                    key: task.task_id.clone().into(),
                    tool_call_id: task.tool_use_id.clone().map(acp_v2::ToolCallId::new),
                    session_id: None,
                    parent_key: task.parent_task_id.clone().map(SharedString::from),
                    description: task.description.clone().into(),
                    status: task.status,
                    waiting: false,
                    backgrounded: task.backgrounded,
                    elapsed: match task.status {
                        AgentStatus::Working => {
                            Some(now.saturating_duration_since(task.started_at))
                        }
                        _ => task.duration.or_else(|| {
                            task.ended_at
                                .map(|ended_at| ended_at.saturating_duration_since(task.started_at))
                        }),
                    },
                    total_tokens: task.total_tokens,
                    activity: task.activity.clone().map(SharedString::from),
                    summary: task.summary.clone().map(SharedString::from),
                    error: task.error.clone().map(SharedString::from),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !sdk_agents.is_empty() {
        return sdk_agents;
    }

    let mut agents = Vec::new();
    collect_subagents(thread, None, activity, now, &mut agents, cx);
    agents
}

/// The ACP v2 subagents of `thread`, each followed by its own subagents.
fn collect_subagents(
    thread: &AcpThread,
    parent_key: Option<SharedString>,
    activity: Option<&ThreadActivity>,
    now: Instant,
    agents: &mut Vec<AgentSummary>,
    cx: &App,
) {
    let background_sessions = thread
        .background_work()
        .unwrap_or_default()
        .iter()
        .filter_map(|work| match work {
            acp_v2::BackgroundWork::Subagent(subagent) => Some(subagent.session_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    for subagent in thread.subagents() {
        let subagent = subagent.read(cx);
        let session_id = subagent.session_id().clone();
        let (status, waiting) = match subagent.foreground_state() {
            Some(acp_v2::StateUpdate::Running(_)) => (AgentStatus::Working, false),
            Some(acp_v2::StateUpdate::RequiresAction(_)) => (AgentStatus::Working, true),
            Some(acp_v2::StateUpdate::Idle(idle)) => match &idle.stop_reason {
                Some(acp_v2::StopReason::Cancelled) => (AgentStatus::Stopped, false),
                Some(
                    acp_v2::StopReason::Error(_)
                    | acp_v2::StopReason::MaxTokens
                    | acp_v2::StopReason::MaxTurnRequests
                    | acp_v2::StopReason::Refusal,
                ) => (AgentStatus::Failed, false),
                _ => (AgentStatus::Finished, false),
            },
            _ => (AgentStatus::Finished, false),
        };
        let times = activity.and_then(|activity| activity.subagent_times.get(&session_id));
        let elapsed = times.map(|times| match status {
            AgentStatus::Working => now.saturating_duration_since(times.started_at),
            _ => times
                .last_activity_at
                .saturating_duration_since(times.started_at),
        });
        let latest_tool_call = subagent
            .entries()
            .iter()
            .rev()
            .find_map(|entry| match entry {
                AgentThreadEntry::ToolCall(tool_call) => Some(SharedString::from(
                    tool_call.label.read(cx).source().to_string(),
                )),
                _ => None,
            });
        let key = SharedString::from(session_id.0.to_string());
        agents.push(AgentSummary {
            key: key.clone(),
            tool_call_id: None,
            backgrounded: background_sessions.contains(&session_id),
            session_id: Some(session_id),
            parent_key: parent_key.clone(),
            description: subagent_title(subagent),
            status,
            waiting,
            elapsed,
            total_tokens: subagent.token_usage().map(|usage| usage.used_tokens),
            activity: latest_tool_call,
            summary: None,
            error: None,
        });
        collect_subagents(subagent, Some(key), activity, now, agents, cx);
    }
}

/// A background task that is not an agent, such as a shell command, for the agent map.
#[derive(Clone, Debug)]
pub(crate) struct BackgroundTaskSummary {
    pub key: SharedString,
    pub title: SharedString,
    /// The extension's word for its kind: "shell", "monitor", "workflow", …
    pub kind: SharedString,
    pub elapsed: Option<Duration>,
}

impl BackgroundTaskSummary {
    /// "shell · 971m 37s".
    pub(crate) fn meta(&self) -> String {
        match self.elapsed.filter(|elapsed| elapsed.as_millis() >= 1000) {
            Some(elapsed) => format!("{} · {}", self.kind, format_agent_duration(elapsed)),
            None => self.kind.to_string(),
        }
    }
}

/// The extension's names for Claude Code task types.
fn task_type_word(task_type: Option<&str>) -> &'static str {
    match task_type {
        Some("local_agent") => "subagent",
        Some("local_workflow") => "workflow",
        Some("local_bash") => "shell",
        Some("monitor_mcp" | "monitor_ws") => "monitor",
        Some("mcp_task") => "MCP task",
        Some("in_process_teammate") => "teammate",
        Some("dream") => "dream",
        Some("auto_mode_scan") => "auto-mode scan",
        Some("local_memory_import") => "memory import",
        Some("post_turn_memory") => "memory save",
        Some("remote_agent") => "cloud session",
        _ => "task",
    }
}

/// The session's live background tasks other than agents: from raw SDK messages (ACP
/// v1), or the terminals and tool calls of an ACP v2 session's `background` list.
pub(crate) fn background_task_summaries(
    thread: &AcpThread,
    activity: Option<&ThreadActivity>,
    now: Instant,
    cx: &App,
) -> Vec<BackgroundTaskSummary> {
    if let Some(activity) = activity
        && !activity.sdk_tasks.is_empty()
    {
        return activity
            .sdk_tasks
            .iter()
            .filter(|task| {
                task.subagent_type.is_none()
                    && task.backgrounded
                    && !task.ambient
                    && task.status == AgentStatus::Working
            })
            .map(|task| BackgroundTaskSummary {
                key: task.task_id.clone().into(),
                title: task.description.clone().into(),
                kind: task_type_word(task.task_type.as_deref()).into(),
                elapsed: Some(now.saturating_duration_since(task.started_at)),
            })
            .collect();
    }
    let tool_call_title = |tool_call_id: &acp_v2::ToolCallId| {
        thread.entries().iter().find_map(|entry| match entry {
            AgentThreadEntry::ToolCall(tool_call) if &tool_call.id == tool_call_id => Some(
                SharedString::from(tool_call.label.read(cx).source().to_string()),
            ),
            _ => None,
        })
    };
    thread
        .background_work()
        .unwrap_or_default()
        .iter()
        .filter_map(|work| {
            let (id, kind) = match work {
                // claude-agent-acp names a background command's terminal after its Bash
                // tool call.
                acp_v2::BackgroundWork::Terminal(terminal) => {
                    (terminal.terminal_id.0.to_string(), "shell")
                }
                acp_v2::BackgroundWork::ToolCall(tool_call) => {
                    (tool_call.tool_call_id.0.to_string(), "task")
                }
                _ => return None,
            };
            let title = tool_call_title(&acp_v2::ToolCallId::new(id.clone()))
                .unwrap_or_else(|| kind.into());
            Some(BackgroundTaskSummary {
                key: id.into(),
                title,
                kind: kind.into(),
                elapsed: None,
            })
        })
        .collect()
}

/// The number on the agents chip: the agents still working, like the extension.
pub(crate) fn live_agent_count(agents: &[AgentSummary]) -> usize {
    agents
        .iter()
        .filter(|agent| agent.status == AgentStatus::Working)
        .count()
}

pub(crate) fn agents_chip_tooltip(dot: DotState) -> &'static str {
    match dot {
        DotState::Waiting => {
            "An agent is waiting for your permission · Click to open the agent map"
        }
        DotState::Running => "Agents are working · Click to open the agent map",
        DotState::Failed => "An agent failed · Click to open the agent map",
        DotState::Idle => "Click to open the agent map",
    }
}

/// The agents chip's dot: waiting while a working agent waits for permission, running
/// while any agent works, failed when a finished one failed, idle otherwise.
pub(crate) fn agents_dot_state(agents: &[AgentSummary]) -> DotState {
    let working = agents
        .iter()
        .filter(|agent| agent.status == AgentStatus::Working);
    if working.clone().any(|agent| agent.waiting) {
        DotState::Waiting
    } else if working.count() > 0 {
        DotState::Running
    } else if agents
        .iter()
        .any(|agent| agent.status == AgentStatus::Failed)
    {
        DotState::Failed
    } else {
        DotState::Idle
    }
}

#[derive(IntoElement)]
pub(crate) struct StatusDot {
    state: DotState,
    size: Pixels,
}

impl StatusDot {
    pub(crate) fn new(state: DotState) -> Self {
        Self {
            state,
            size: px(8.),
        }
    }

    pub(crate) fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for StatusDot {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .flex_none()
            .size(self.size)
            .rounded_full()
            .bg(self.state.color(cx))
    }
}

// --- Status bar item ------------------------------------------------------------------

/// "✻ Claude Code" at the right of the status bar: opens the Claude tab, or a new
/// session when there is none. Present only in the Claude Code style.
pub(crate) struct ClaudeCodeStatusItem {
    workspace: WeakEntity<Workspace>,
}

impl Render for ClaudeCodeStatusItem {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let workspace = self.workspace.clone();
        Button::new("claude-code-status-item", "Claude Code")
            .label_size(LabelSize::Small)
            .color(Color::Muted)
            .start_icon(
                Icon::new(IconName::AiClaude)
                    .size(IconSize::XSmall)
                    .color(Color::Muted),
            )
            .tooltip(ui::Tooltip::text("Open Claude Code"))
            .on_click(move |_, window, cx| {
                let workspace = workspace.clone();
                // The click can arrive while the workspace is updating its status bar.
                window.defer(cx, move |window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            super::thread_item::focus_or_open_thread_tab(workspace, window, cx);
                        })
                        .log_err();
                });
            })
    }
}

impl StatusItemView for ClaudeCodeStatusItem {
    fn set_active_pane_item(
        &mut self,
        _active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn hide_setting(&self, _cx: &App) -> Option<workspace::HideStatusItem> {
        // Shown only while `agent.ui_style` is `claude_code`.
        None
    }
}

fn sync_status_item(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let enabled = super::claude_code_style_enabled(cx);
    let status_bar = workspace.status_bar().clone();
    let position = status_bar
        .read(cx)
        .position_of_item::<ClaudeCodeStatusItem>();
    match (enabled, position) {
        (true, None) => {
            let workspace = cx.weak_entity();
            let item = cx.new(|_| ClaudeCodeStatusItem { workspace });
            status_bar.update(cx, |status_bar, cx| {
                status_bar.add_right_item(item, window, cx);
            });
        }
        (false, Some(position)) => {
            status_bar.update(cx, |status_bar, cx| status_bar.remove_item_at(position, cx));
        }
        _ => {}
    }
}

fn init_status_item(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        sync_status_item(workspace, window, cx);
        cx.observe_global_in::<SettingsStore>(window, sync_status_item)
            .detach();
    })
    .detach();
}

/// The prompt cache clock in the footer: the minutes the cache has left, or a red
/// clock once it has likely expired.
#[derive(IntoElement)]
pub(crate) struct CacheClockElement {
    thread_view_id: EntityId,
}

impl CacheClockElement {
    pub(crate) fn new(thread_view_id: EntityId) -> Self {
        Self { thread_view_id }
    }
}

impl RenderOnce for CacheClockElement {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Its own view, so that the minute ticks only re-render the clock.
        window.with_id(("claude-code-cache-clock", self.thread_view_id), |window| {
            window.use_state(cx, |_, cx| CacheClockView::new(self.thread_view_id, cx))
        })
    }
}

struct CacheClockView {
    thread_view_id: EntityId,
    _ticker: gpui::Task<()>,
}

impl CacheClockView {
    fn new(thread_view_id: EntityId, cx: &mut Context<Self>) -> Self {
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(15))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            thread_view_id,
            _ticker: ticker,
        }
    }
}

impl Render for CacheClockView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let clock = thread_activity(self.thread_view_id, cx)
            .map(|activity| activity.cache_clock(Instant::now()))
            .unwrap_or(CacheClock::Unknown);
        let Some(tooltip) = clock.tooltip() else {
            return gpui::Empty.into_any_element();
        };
        let icon_color = match clock {
            CacheClock::Warm { .. } => Color::Muted,
            _ => Color::Error,
        };
        h_flex()
            .id("claude-code-cache-clock")
            .flex_none()
            .gap_1()
            .h(px(24.))
            .px_1()
            .tooltip(ui::Tooltip::text(tooltip))
            .child(
                Icon::new(IconName::Clock)
                    .size(IconSize::Small)
                    .color(icon_color),
            )
            .when_some(clock.label(), |this, label| {
                this.child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
            })
            .into_any_element()
    }
}

/// The "✻ Spinning…" indicator shown at the end of a running turn.
#[derive(IntoElement)]
pub(crate) struct ClaudeCodeSpinnerElement {
    turn_generation: usize,
    started_at: Option<Instant>,
}

impl ClaudeCodeSpinnerElement {
    pub(crate) fn new(turn_generation: usize, started_at: Option<Instant>) -> Self {
        Self {
            turn_generation,
            started_at,
        }
    }
}

impl RenderOnce for ClaudeCodeSpinnerElement {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Keyed by turn so each turn gets a fresh verb and start time.
        window.with_id(("claude-code-spinner", self.turn_generation), |window| {
            window.use_state(cx, |_, _| {
                ClaudeCodeSpinner::new(self.turn_generation, self.started_at)
            })
        })
    }
}

/// The spinner is its own view so that its animation frames only re-render
/// the indicator rather than the whole thread.
struct ClaudeCodeSpinner {
    verb: &'static str,
    started_at: Option<Instant>,
}

impl ClaudeCodeSpinner {
    fn new(turn_generation: usize, started_at: Option<Instant>) -> Self {
        let verb = SPINNER_VERBS
            .get(turn_generation % SPINNER_VERBS.len())
            .copied()
            .unwrap_or("Spinning");
        Self { verb, started_at }
    }
}

impl Render for ClaudeCodeSpinner {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let elapsed_seconds = self
            .started_at
            .map(|started_at| started_at.elapsed().as_secs());
        let status = match elapsed_seconds {
            Some(seconds) => format!("({seconds}s · esc to interrupt)"),
            None => "(esc to interrupt)".to_string(),
        };

        h_flex()
            .gap_1p5()
            .child(
                div()
                    .flex_none()
                    .w(TIMELINE_GUTTER)
                    .text_color(cx.theme().colors().text_accent)
                    .with_animation(
                        "claude-code-spinner-glyph",
                        Animation::new(Duration::from_millis(1200)).repeat(),
                        |glyph, delta| {
                            let glyph_count = SPINNER_GLYPHS.len() * 2 - 2;
                            let step = ((delta * glyph_count as f32) as usize).min(glyph_count - 1);
                            let index = if step < SPINNER_GLYPHS.len() {
                                step
                            } else {
                                glyph_count - step
                            };
                            glyph.child(SPINNER_GLYPHS.get(index).copied().unwrap_or("✻"))
                        },
                    ),
            )
            .child(
                Label::new(format!("{}…", self.verb))
                    .size(LabelSize::Small)
                    .color(Color::Accent),
            )
            .child(
                Label::new(status)
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike as _;

    fn permission_option(
        id: &'static str,
        name: &'static str,
        kind: acp_v2::PermissionOptionKind,
    ) -> acp_v2::PermissionOption {
        acp_v2::PermissionOption::new(acp_v2::PermissionOptionId::new(id), name, kind)
    }

    #[test]
    fn test_permission_option_keys() {
        assert_eq!(permission_option_for_key("1", 3), Some(0));
        assert_eq!(permission_option_for_key("3", 3), Some(2));
        assert_eq!(permission_option_for_key("4", 3), None);
        assert_eq!(permission_option_for_key("0", 3), None);
        assert_eq!(permission_option_for_key("a", 3), None);
        assert_eq!(permission_option_for_key("10", 12), None);
        assert_eq!(permission_option_for_key("escape", 3), None);

        // claude-agent-acp v0.106.0, `buildExitPlanModePermissionOptions` with auto available.
        let plan_options = [
            permission_option(
                "exit-plan-auto",
                "Yes, and use auto mode",
                acp_v2::PermissionOptionKind::AllowAlways,
            ),
            permission_option(
                "exit-plan-default",
                "Yes, manually approve edits",
                acp_v2::PermissionOptionKind::AllowOnce,
            ),
            permission_option(
                "reject",
                "No, keep planning",
                acp_v2::PermissionOptionKind::RejectOnce,
            ),
        ];
        assert!(is_plan_approval(&plan_options));
        assert_eq!(reject_option_index(&plan_options), Some(2));

        let bash_options = [
            permission_option("allow-once", "Yes", acp_v2::PermissionOptionKind::AllowOnce),
            permission_option(
                "allow-with-updates",
                "Yes, and don't ask again for `git status` commands",
                acp_v2::PermissionOptionKind::AllowAlways,
            ),
            permission_option("reject", "No", acp_v2::PermissionOptionKind::RejectOnce),
        ];
        assert!(!is_plan_approval(&bash_options));
        assert_eq!(reject_option_index(&bash_options), Some(2));
        assert!(is_allow_option(&bash_options[1]));
        assert!(!is_allow_option(&bash_options[2]));
        assert_eq!(reject_option_index(&bash_options[..2]), None);
    }

    #[test]
    fn test_thought_label() {
        assert_eq!(thought_label(None, true).as_ref(), "Thinking...");
        assert_eq!(
            thought_label(Some(Duration::from_secs(3)), true).as_ref(),
            "Thinking..."
        );
        assert_eq!(thought_label(None, false).as_ref(), "Thinking");
        assert_eq!(
            thought_label(Some(Duration::from_millis(200)), false).as_ref(),
            "Thought for 0s"
        );
        assert_eq!(
            thought_label(Some(Duration::from_millis(500)), false).as_ref(),
            "Thought for 1s"
        );
        assert_eq!(
            thought_label(Some(Duration::from_millis(8_400)), false).as_ref(),
            "Thought for 8s"
        );
        assert_eq!(
            thought_label(Some(Duration::from_millis(8_600)), false).as_ref(),
            "Thought for 9s"
        );
    }

    #[test]
    fn test_attachment_names() {
        let path_style = PathStyle::local();
        assert_eq!(
            attachment_name(None, "image/png", path_style).as_ref(),
            "image.png"
        );
        let pasted = MentionUri::PastedImage {
            name: DEFAULT_PASTED_IMAGE_NAME.to_string(),
        };
        assert_eq!(
            attachment_name(Some(pasted.to_uri().as_ref()), "image/jpeg", path_style).as_ref(),
            "image.jpg"
        );
        let file = MentionUri::File {
            abs_path: std::path::PathBuf::from(if cfg!(windows) {
                "C:\\shots\\screen.png"
            } else {
                "/shots/screen.png"
            }),
        };
        assert_eq!(
            attachment_name(Some(file.to_uri().as_ref()), "image/png", path_style).as_ref(),
            "screen.png"
        );
    }

    #[test]
    fn test_sent_message_text() {
        let path_style = PathStyle::local();
        let file = MentionUri::File {
            abs_path: std::path::PathBuf::from(if cfg!(windows) {
                "C:\\repo\\main.rs"
            } else {
                "/repo/main.rs"
            }),
        };
        let blocks = vec![
            acp_v2::ContentBlock::Image(acp_v2::ImageContent::new("", "image/png")),
            acp_v2::ContentBlock::Text(acp_v2::TextContent::new("look at ")),
            acp_v2::ContentBlock::ResourceLink(acp_v2::ResourceLink::new(
                "main.rs",
                file.to_uri().to_string(),
            )),
            acp_v2::ContentBlock::Text(acp_v2::TextContent::new(" and fix it\n")),
        ];
        assert_eq!(
            sent_message_text(&blocks, path_style),
            "look at @main.rs and fix it"
        );
    }

    #[test]
    fn test_day_lines() {
        let morning = Local::now()
            .with_hour(9)
            .and_then(|time| time.with_minute(0))
            .expect("valid time");
        let next_day = morning + chrono::Duration::days(1);
        let mut activity = ThreadActivity::default();
        activity.message_times.insert((0, 0), morning);
        activity
            .message_times
            .insert((1, 2), morning + chrono::Duration::minutes(5));
        activity.message_times.insert((3, 0), next_day);
        assert!(activity.starts_new_day((0, 0)), "the first message");
        assert!(!activity.starts_new_day((1, 2)));
        assert!(activity.starts_new_day((3, 0)));
        assert!(!activity.starts_new_day((4, 0)), "no time, no line");

        let now = morning;
        assert_eq!(
            format_day(morning, now).as_ref(),
            morning.format("%a, %b %-d").to_string()
        );
        let last_year = morning - chrono::Duration::days(400);
        assert!(format_day(last_year, now).ends_with(&last_year.year().to_string()));
    }

    #[test]
    fn test_strip_code_fence() {
        assert_eq!(strip_code_fence("```console\nls\nfoo\n```"), "ls\nfoo");
        assert_eq!(strip_code_fence("```\nbar\n```\n"), "bar");
        assert_eq!(strip_code_fence("plain output"), "plain output");
        assert_eq!(strip_code_fence("```unterminated"), "```unterminated");
        assert_eq!(
            strip_code_fence("````\n```rust\nfn main() {}\n```\n````"),
            "```rust\nfn main() {}\n```"
        );
        assert_eq!(strip_code_fence("```\r\nwindows\r\n```"), "windows");
    }

    #[test]
    fn test_display_lines() {
        assert_eq!(
            display_lines(
                "On branch main\nYour branch is up to date.\n\nnothing to commit, working tree clean\n"
            ),
            vec![
                "On branch main",
                "Your branch is up to date.",
                "",
                "nothing to commit, working tree clean",
            ]
        );
        assert_eq!(
            display_lines("one\r\ntwo\rthree"),
            vec!["one", "two", "three"]
        );
        assert_eq!(
            display_lines("1\tfn main() {\n10\t}"),
            vec!["1   fn main() {", "10  }"]
        );
        assert!(display_lines("\n\n").is_empty());
    }

    fn select_option(
        id: &str,
        category: acp_v2::SessionConfigOptionCategory,
        current_value: &str,
        entries: &[(&str, &str, Option<&str>)],
    ) -> acp_v2::SessionConfigOption {
        let options = entries
            .iter()
            .map(|(value, name, description)| {
                let option = acp_v2::SessionConfigSelectOption::new(*value, *name);
                match description {
                    Some(description) => option.description(*description),
                    None => option,
                }
            })
            .collect::<Vec<_>>();
        acp_v2::SessionConfigOption::select(id, id, current_value, options).category(category)
    }

    /// The options claude-agent-acp 0.106 sends to a client with boolean config
    /// options (see `src/session-config-options.ts`).
    fn adapter_options(
        model: &str,
        effort: &str,
        ultracode: bool,
    ) -> Vec<acp_v2::SessionConfigOption> {
        vec![
            select_option(
                "mode",
                acp_v2::SessionConfigOptionCategory::Mode,
                "plan",
                &[
                    ("default", "Manual", None),
                    ("acceptEdits", "Accept edits", None),
                    ("plan", "Plan", None),
                    ("auto", "Auto", None),
                ],
            ),
            select_option(
                "model",
                acp_v2::SessionConfigOptionCategory::Model,
                model,
                &[
                    ("default", "Default (recommended)", Some("Opus 5.5")),
                    ("sonnet", "Sonnet 5", None),
                ],
            ),
            select_option(
                "effort",
                acp_v2::SessionConfigOptionCategory::ThoughtLevel,
                effort,
                &[
                    ("default", "Default", None),
                    ("high", "High", None),
                    ("xhigh", "Xhigh", None),
                ],
            ),
            acp_v2::SessionConfigOption::boolean("fast", "Fast mode", false)
                .category(acp_v2::SessionConfigOptionCategory::ModelConfig),
            acp_v2::SessionConfigOption::boolean("ultracode", "Ultracode", ultracode)
                .category(acp_v2::SessionConfigOptionCategory::ModelConfig),
        ]
    }

    #[test]
    fn test_model_chip_label() {
        assert_eq!(
            model_chip_label(&adapter_options("default", "xhigh", false)).as_deref(),
            Some("Opus 5.5 Extra high")
        );
        assert_eq!(
            model_chip_label(&adapter_options("sonnet", "high", true)).as_deref(),
            Some("Sonnet 5 High · Ultracode")
        );
        assert_eq!(
            model_chip_label(&adapter_options("sonnet", "default", false)).as_deref(),
            Some("Sonnet 5")
        );
        assert_eq!(model_chip_label(&[]), None);
    }

    #[test]
    fn test_toggle_options() {
        let options = adapter_options("default", "default", true);
        let ultracode = options
            .iter()
            .find(|option| option.config_id.0.as_ref() == "ultracode")
            .expect("ultracode option");
        assert_eq!(toggle_option_state(ultracode), Some(true));
        assert_eq!(
            toggled_option_value(ultracode, false),
            Some(acp_v2::SessionConfigOptionValue::boolean(false))
        );

        let select_fallback = select_option(
            "fast",
            acp_v2::SessionConfigOptionCategory::ModelConfig,
            "on",
            &[("on", "On", None), ("off", "Off", None)],
        );
        assert_eq!(toggle_option_state(&select_fallback), Some(true));
        assert_eq!(
            toggled_option_value(&select_fallback, false),
            Some(acp_v2::SessionConfigOptionValue::id("off"))
        );
        let mode = options.first().expect("mode option");
        assert_eq!(toggle_option_state(mode), None);
    }

    #[test]
    fn test_mode_display_names() {
        assert_eq!(mode_display_name("default", "Manual").as_ref(), "Manual");
        assert_eq!(
            mode_display_name("acceptEdits", "Accept edits").as_ref(),
            "Edit automatically"
        );
        assert_eq!(mode_display_name("plan", "Plan").as_ref(), "Plan");
        assert_eq!(mode_display_name("auto", "Auto").as_ref(), "Auto");
        assert_eq!(
            mode_display_name("bypassPermissions", "Bypass permissions").as_ref(),
            "Bypass permissions"
        );
        assert_eq!(mode_display_name("custom", "Custom").as_ref(), "Custom");
    }

    #[test]
    fn test_placeholders() {
        assert!(is_claude_code_placeholder(QUEUE_PLACEHOLDER));
        assert!(is_claude_code_placeholder(idle_placeholder()));
        assert!(idle_placeholder().ends_with("Esc to focus or unfocus Claude"));
        assert!(!is_claude_code_placeholder(
            "Message Claude Agent — @ to include context, / for commands"
        ));
    }

    #[test]
    fn test_is_claude_code_tool_name() {
        assert!(is_claude_code_tool_name("Bash"));
        assert!(is_claude_code_tool_name("mcp__github__create_issue"));
        assert!(!is_claude_code_tool_name("terminal"));
        assert!(!is_claude_code_tool_name("read_file"));
    }

    #[test]
    fn test_strip_backticks() {
        assert_eq!(strip_backticks("`git status`"), "git status");
        assert_eq!(strip_backticks("git status"), "git status");
    }

    #[test]
    fn test_claude_code_tool_name() {
        let meta = acp_v2::Meta::from_iter([(
            CLAUDE_CODE_META_KEY.into(),
            serde_json::json!({ CLAUDE_CODE_TOOL_NAME_KEY: "Bash" }),
        )]);
        assert_eq!(claude_code_tool_name(Some(&meta)).as_deref(), Some("Bash"));
        assert_eq!(claude_code_tool_name(None), None);
        let empty = acp_v2::Meta::from_iter([(
            CLAUDE_CODE_META_KEY.into(),
            serde_json::json!({ CLAUDE_CODE_TOOL_NAME_KEY: " " }),
        )]);
        assert_eq!(claude_code_tool_name(Some(&empty)), None);
    }

    #[test]
    fn test_line_stats() {
        assert_eq!(line_stats("a\nb\nc\n", "a\nB\nc\nd\n"), (2, 1));
        assert_eq!(line_stats("", "one\ntwo\n"), (2, 0));
    }

    /// The `model` option claude-agent-acp 0.106.0 returned from `session/new` for a
    /// Claude subscription (captured over stdio, no prompt sent).
    fn captured_model_option(current: &str) -> acp_v2::SessionConfigOption {
        select_option(
            "model",
            acp_v2::SessionConfigOptionCategory::Model,
            current,
            &[
                ("default", "Default (recommended)", Some("Opus")),
                (
                    "opus",
                    "Opus 5.5",
                    Some("Opus 5.5 · Best for everyday, complex tasks"),
                ),
                (
                    "claude-fable-5-1[1m]",
                    "Fable 5.1",
                    Some("Fable 5.1 · Most capable for your hardest and longest-running tasks"),
                ),
                (
                    "sonnet",
                    "Sonnet 5.5",
                    Some("Sonnet 5.5 · Efficient for routine tasks"),
                ),
                (
                    "haiku",
                    "Haiku 5.5",
                    Some("Haiku 5.5 · Fastest for quick answers"),
                ),
            ],
        )
    }

    fn menu_rows(option: &acp_v2::SessionConfigOption) -> Vec<(String, Option<String>, bool)> {
        let acp_v2::SessionConfigKind::Select(select) = &option.kind else {
            panic!("model option is a select");
        };
        model_menu_rows(select)
            .into_iter()
            .map(|row| {
                (
                    row.title.to_string(),
                    row.subtitle.map(|subtitle| subtitle.to_string()),
                    row.is_current,
                )
            })
            .collect()
    }

    #[test]
    fn test_model_menu_rows_match_the_extension() {
        let rows = menu_rows(&captured_model_option("default"));
        assert_eq!(
            rows,
            vec![
                (
                    "Default (recommended)".to_string(),
                    Some("Opus 5.5 · Best for everyday, complex tasks".to_string()),
                    true
                ),
                (
                    "Opus".to_string(),
                    Some("Opus 5.5 · Best for everyday, complex tasks".to_string()),
                    false
                ),
                (
                    "Fable".to_string(),
                    Some(
                        "Fable 5.1 · Most capable for your hardest and longest-running tasks"
                            .to_string()
                    ),
                    false
                ),
                (
                    "Sonnet".to_string(),
                    Some("Sonnet 5.5 · Efficient for routine tasks".to_string()),
                    false
                ),
                (
                    "Haiku".to_string(),
                    Some("Haiku 5.5 · Fastest for quick answers".to_string()),
                    false
                ),
            ]
        );
        assert_eq!(
            model_chip_label(&[captured_model_option("default")]).as_deref(),
            Some("Opus 5.5")
        );
    }

    #[test]
    fn test_model_menu_rows_leave_out_pinned_versions() {
        let option = select_option(
            "model",
            acp_v2::SessionConfigOptionCategory::Model,
            "opus",
            &[
                ("default", "Default (recommended)", Some("Opus")),
                (
                    "opus",
                    "Opus 5.5",
                    Some("Opus 5.5 · Best for everyday, complex tasks"),
                ),
                ("fable", "Fable 5.2", Some("Fable 5.2 · Most capable")),
                (
                    "sonnet",
                    "Sonnet 5.5",
                    Some("Sonnet 5.5 · Efficient for routine tasks"),
                ),
                (
                    "haiku",
                    "Haiku 5.5",
                    Some("Haiku 5.5 · Fastest for quick answers"),
                ),
                ("claude-fable-5-1[1m]", "Fable 5.1", Some("Fable 5.1")),
                ("claude-haiku-4-5-20251001", "Haiku 4.5", Some("Haiku 4.5")),
                ("claude-opus-4-8", "Opus 4.8", Some("Opus 4.8")),
                ("claude-sonnet-4-6", "Sonnet 4.6", Some("Sonnet 4.6")),
            ],
        );
        let titles = menu_rows(&option)
            .into_iter()
            .map(|(title, _, _)| title)
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            ["Default (recommended)", "Opus", "Fable", "Sonnet", "Haiku"]
        );

        // A session running a pinned version keeps that row, so the check has a place.
        let mut option = option;
        if let acp_v2::SessionConfigKind::Select(select) = &mut option.kind {
            select.current_value = acp_v2::SessionConfigValueId::new("claude-opus-4-8");
        }
        let rows = menu_rows(&option);
        assert_eq!(rows.len(), 6);
        assert_eq!(
            rows[5],
            ("Opus 4.8".to_string(), Some("Opus 4.8".to_string()), true)
        );
    }

    #[test]
    fn test_model_family_version() {
        assert_eq!(model_family_version("opus"), Some(("opus", None)));
        assert_eq!(model_family_version("opus[1m]"), Some(("opus", None)));
        assert_eq!(
            model_family_version("claude-fable-5-1[1m]"),
            Some(("fable", Some((5, 1))))
        );
        assert_eq!(
            model_family_version("claude-haiku-4-5-20251001"),
            Some(("haiku", Some((4, 5))))
        );
        assert_eq!(
            model_family_version("claude-sonnet-4-20250514"),
            Some(("sonnet", Some((4, 0))))
        );
        assert_eq!(model_family_version("default"), None);
        assert_eq!(model_family_version("my-gateway-model"), None);
    }

    #[test]
    fn test_effort_slider() {
        let options = adapter_options("default", "xhigh", false);
        let effort =
            first_select_option(&options, &acp_v2::SessionConfigOptionCategory::ThoughtLevel)
                .and_then(effort_slider)
                .expect("effort slider");
        assert_eq!(effort.levels.len(), 2);
        assert_eq!(effort.current, Some(1));
        assert_eq!(effort.label.as_ref(), "Extra high");

        let options = adapter_options("default", "default", false);
        let effort =
            first_select_option(&options, &acp_v2::SessionConfigOptionCategory::ThoughtLevel)
                .and_then(effort_slider)
                .expect("effort slider");
        assert_eq!(effort.current, None);
        assert_eq!(effort.label.as_ref(), "Auto");
    }

    #[test]
    fn test_sdk_task_messages_build_agents() {
        let started_at = Instant::now();
        let mut activity = ThreadActivity::default();
        apply_sdk_message(
            &mut activity,
            &serde_json::json!({
                "type": "system",
                "subtype": "task_started",
                "task_id": "agent-1",
                "tool_use_id": "toolu_1",
                "description": "Explore agent UI architecture",
                "subagent_type": "Explore",
                "task_type": "local_agent",
                "is_backgrounded": true,
            }),
            started_at,
        );
        apply_sdk_message(
            &mut activity,
            &serde_json::json!({
                "type": "system",
                "subtype": "task_started",
                "task_id": "shell-1",
                "description": "npm run dev",
                "task_type": "local_bash",
                "is_backgrounded": true,
            }),
            started_at,
        );
        apply_sdk_message(
            &mut activity,
            &serde_json::json!({
                "type": "system",
                "subtype": "task_progress",
                "task_id": "agent-1",
                "description": "Running Show code around rate_limit_event construction",
                "usage": {"total_tokens": 177_300, "tool_uses": 12, "duration_ms": 5000},
            }),
            started_at,
        );
        let agents = activity
            .sdk_tasks
            .iter()
            .filter(|task| task.subagent_type.is_some())
            .count();
        assert_eq!(agents, 1, "only subagents count as agents");
        let task = &activity.sdk_tasks[0];
        assert_eq!(task.total_tokens, Some(177_300));
        assert_eq!(
            task.activity.as_deref(),
            Some("Running Show code around rate_limit_event construction")
        );

        let summary = AgentSummary {
            key: "agent-1".into(),
            tool_call_id: Some(acp_v2::ToolCallId::new("toolu_1")),
            session_id: None,
            parent_key: None,
            description: task.description.clone().into(),
            status: task.status,
            waiting: false,
            backgrounded: task.backgrounded,
            elapsed: Some(Duration::from_secs(96 * 60 + 14)),
            total_tokens: task.total_tokens,
            activity: task.activity.clone().map(SharedString::from),
            summary: None,
            error: None,
        };
        assert_eq!(
            summary.background_line(),
            "Working in the background: Running Show code around rate_limit_event construction · type /tasks to see its transcript"
        );
        assert_eq!(summary.meta(), "96m 14s · 177.3k tokens");
        assert_eq!(summary.dot_state(), DotState::Running);

        apply_sdk_message(
            &mut activity,
            &serde_json::json!({
                "type": "system",
                "subtype": "task_notification",
                "task_id": "agent-1",
                "status": "completed",
                "output_file": "/tmp/out",
                "summary": "Done",
                "usage": {"total_tokens": 180_000, "tool_uses": 14, "duration_ms": 61_000},
            }),
            started_at + Duration::from_secs(61),
        );
        let task = &activity.sdk_tasks[0];
        assert_eq!(task.status, AgentStatus::Finished);
        assert_eq!(task.duration, Some(Duration::from_secs(61)));

        // A background task missing from the live level ended.
        apply_sdk_message(
            &mut activity,
            &serde_json::json!({
                "type": "system",
                "subtype": "background_tasks_changed",
                "tasks": [],
            }),
            started_at + Duration::from_secs(62),
        );
        assert_eq!(activity.sdk_tasks[1].status, AgentStatus::Finished);
    }

    #[test]
    fn test_sdk_background_tasks_and_nested_agents() {
        let now = Instant::now();
        let mut activity = ThreadActivity::default();
        for message in [
            serde_json::json!({
                "type": "system", "subtype": "task_started", "task_id": "agent-1",
                "description": "Track B: thinking rows", "subagent_type": "general-purpose",
                "task_type": "local_agent", "is_backgrounded": true,
            }),
            serde_json::json!({
                "type": "system", "subtype": "task_started", "task_id": "agent-2",
                "description": "Account usage", "subagent_type": "general-purpose",
                "task_type": "local_agent", "parent_task_id": "agent-1",
            }),
            serde_json::json!({
                "type": "system", "subtype": "task_started", "task_id": "shell-1",
                "description": "Wait for zed check to finish", "task_type": "local_bash",
                "is_backgrounded": true,
            }),
        ] {
            apply_sdk_message(&mut activity, &message, now);
        }
        let task = activity
            .sdk_tasks
            .iter()
            .find(|task| task.task_id == "agent-2")
            .expect("nested agent");
        assert_eq!(task.parent_task_id.as_deref(), Some("agent-1"));

        let shell = activity
            .sdk_tasks
            .iter()
            .find(|task| task.task_id == "shell-1")
            .expect("shell task");
        let summary = BackgroundTaskSummary {
            key: shell.task_id.clone().into(),
            title: shell.description.clone().into(),
            kind: task_type_word(shell.task_type.as_deref()).into(),
            elapsed: Some(Duration::from_secs(971 * 60 + 37)),
        };
        assert_eq!(summary.meta(), "shell · 971m 37s");
    }

    #[test]
    fn test_agents_chip_dot() {
        let agent = |status: AgentStatus, waiting: bool| AgentSummary {
            key: "agent".into(),
            tool_call_id: None,
            session_id: None,
            parent_key: None,
            description: "Agent".into(),
            status,
            waiting,
            backgrounded: true,
            elapsed: None,
            total_tokens: None,
            activity: None,
            summary: None,
            error: None,
        };
        assert_eq!(
            agents_dot_state(&[agent(AgentStatus::Finished, false)]),
            DotState::Idle
        );
        assert_eq!(
            agents_dot_state(&[
                agent(AgentStatus::Finished, false),
                agent(AgentStatus::Working, false)
            ]),
            DotState::Running
        );
        assert_eq!(
            agents_dot_state(&[agent(AgentStatus::Working, true)]),
            DotState::Waiting
        );
        assert_eq!(
            agents_dot_state(&[agent(AgentStatus::Failed, false)]),
            DotState::Failed
        );
        assert_eq!(
            live_agent_count(&[
                agent(AgentStatus::Finished, false),
                agent(AgentStatus::Working, false)
            ]),
            1
        );
    }

    #[test]
    fn test_prompt_cache_clock() {
        let start = Instant::now();
        let mut cache = PromptCache::default();
        assert_eq!(cache.clock(start), CacheClock::Unknown);

        cache.record_request(start);
        assert_eq!(
            cache.clock(start + Duration::from_secs(2 * 60)),
            CacheClock::Warm { minutes_left: 58 }
        );
        assert_eq!(
            cache
                .clock(start + Duration::from_secs(2 * 60))
                .label()
                .as_deref(),
            Some("58m")
        );
        let expired = cache.clock(start + Duration::from_secs(78 * 60));
        assert_eq!(
            expired.tooltip().as_deref(),
            Some("Prompt cache likely expired (idle 1h 18m).")
        );
        assert_eq!(expired.label(), None);

        let mut activity = ThreadActivity::default();
        activity.prompt_cache.record_request(start);
        apply_sdk_message(
            &mut activity,
            &serde_json::json!({
                "type": "result",
                "subtype": "success",
                "usage": {
                    "cache_read_input_tokens": 1000,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 200,
                        "ephemeral_1h_input_tokens": 0,
                    },
                },
            }),
            start,
        );
        assert_eq!(
            activity.cache_clock(start + Duration::from_secs(60)),
            CacheClock::Warm { minutes_left: 4 }
        );

        activity
            .prompt_cache
            .record_compaction(start + Duration::from_secs(90));
        assert_eq!(
            activity
                .cache_clock(start + Duration::from_secs(91))
                .tooltip()
                .as_deref(),
            Some("Prompt cache does not cover the compacted conversation.")
        );
        activity
            .prompt_cache
            .record_request(start + Duration::from_secs(120));
        assert!(matches!(
            activity.cache_clock(start + Duration::from_secs(121)),
            CacheClock::Warm { .. }
        ));
    }

    #[test]
    fn test_agent_formatting() {
        assert_eq!(format_agent_duration(Duration::from_secs(45)), "45s");
        assert_eq!(
            format_agent_duration(Duration::from_secs(5 * 60 + 1)),
            "5m 1s"
        );
        assert_eq!(format_token_count(950), "950");
        assert_eq!(format_token_count(273_200), "273.2k");
        assert_eq!(format_token_count(1_250_000), "1.2M");
        assert_eq!(agent_count_label(1), "1 agent");
        assert_eq!(agent_count_label(13), "13 agents");
        assert_eq!(format_idle_time(Duration::from_secs(18 * 60)), "18m");
        assert_eq!(format_idle_time(Duration::from_secs(26 * 3600)), "1d 2h");
    }

    #[test]
    fn test_context_ring() {
        // 200k tokens, less the 20k output reserve and the 13k auto-compact buffer.
        assert_eq!(context_ring_percentage(80_000, 200_000, None), None);
        let percentage = context_ring_percentage(100_200, 200_000, None).expect("over half used");
        assert!((percentage - 60.).abs() < 0.01);
        assert_eq!(
            context_ring_tooltip(percentage),
            "40% of context remaining until auto-compact."
        );
        assert_eq!(context_ring_percentage(500_000, 200_000, None), Some(100.));
        assert_eq!(context_ring_percentage(10_000, 0, None), None);
        assert_eq!(context_ring_percentage(10_000, 30_000, None), None);
        // A smaller output limit leaves more of the window.
        let percentage =
            context_ring_percentage(107_400, 200_000, Some(8_000)).expect("over half used");
        assert!((percentage - 60.).abs() < 0.01);

        assert_eq!(context_ring_arcs(60.).used_end, 180.);
        assert_eq!(context_ring_arcs(70.).used_end, 252.5);
        assert_eq!(context_ring_arcs(90.).track, None);
    }

    #[test]
    fn test_mention_trigger_text() {
        assert_eq!(mention_trigger_text(None), "@");
        assert_eq!(mention_trigger_text(Some(' ')), "@");
        assert_eq!(mention_trigger_text(Some('\n')), "@");
        assert_eq!(mention_trigger_text(Some('e')), " @");
    }

    #[test]
    fn test_remove_entries_shifts_activity() {
        let now = Instant::now();
        let mut activity = ThreadActivity::default();
        for entry_ix in [1, 3, 5] {
            activity.thoughts.insert(
                (entry_ix, 0),
                ThoughtTiming {
                    started_at: now,
                    last_updated_at: now + Duration::from_secs(entry_ix as u64),
                },
            );
            activity.message_times.insert((entry_ix, 0), Local::now());
        }
        activity.remove_entries(2..4);
        assert_eq!(
            activity.thought_duration(1, 0),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            activity.thought_duration(3, 0),
            Some(Duration::from_secs(5))
        );
        assert_eq!(activity.thought_duration(5, 0), None);
        assert!(activity.message_time((3, 0)).is_some());
        assert!(activity.message_time((5, 0)).is_none());
    }
}
