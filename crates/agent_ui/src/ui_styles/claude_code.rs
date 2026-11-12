pub(crate) mod sessions_panel;
mod tool_row;
pub(crate) mod usage;

pub(crate) use tool_row::ToolRow;

use std::ops::Range;
use std::time::{Duration, Instant};

use acp_thread::{
    AcpThread, AcpThreadEvent, AgentThreadEntry, AssistantMessageChunk, PermissionRequestId,
    ToolCall, ToolCallContent, ToolCallStatus,
};
use agent_client_protocol::v2::schema as acp_v2;
use chrono::{DateTime, Local};
use collections::HashMap;
use gpui::{
    Animation, AnimationExt as _, App, Context, ElementId, EntityId, FocusHandle, Focusable as _,
    Global, Hsla, Pixels, TextStyle, pulsating_between, px,
};
use settings::{AgentUiStyle, Settings as _, SettingsStore};
use theme_settings::ThemeSettings;
use ui::prelude::*;

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
const ULTRACODE_CONFIG_ID: &str = "ultracode";
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
    user_messages_sent_at: HashMap<usize, DateTime<Local>>,
    /// Focused while the permission card replaces the prompt box, so the
    /// card's number keys and Esc reach it.
    permission_focus: Option<FocusHandle>,
    /// The highlighted option of the permission card, for the request it was
    /// chosen on.
    permission_selection: Option<(PermissionRequestId, usize)>,
}

#[derive(Clone, Copy, Debug)]
struct ThoughtTiming {
    started_at: Instant,
    last_updated_at: Instant,
}

impl ThreadActivity {
    pub(crate) fn thought_duration(&self, entry_ix: usize, chunk_ix: usize) -> Option<Duration> {
        self.thoughts.get(&(entry_ix, chunk_ix)).map(|timing| {
            timing
                .last_updated_at
                .saturating_duration_since(timing.started_at)
        })
    }

    pub(crate) fn user_message_sent_at(&self, entry_ix: usize) -> Option<DateTime<Local>> {
        self.user_messages_sent_at.get(&entry_ix).copied()
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
                if let Some(AgentThreadEntry::UserMessage(_)) = thread.entries().get(entry_ix) {
                    self.user_messages_sent_at.insert(entry_ix, wall_clock);
                }
                self.record_streaming_thought(thread, entry_ix, now);
                self.last_activity_at = Some(now);
            }
            AcpThreadEvent::EntryUpdated(entry_ix) => {
                self.record_streaming_thought(thread, *entry_ix, now);
                self.last_activity_at = Some(now);
            }
            AcpThreadEvent::EntriesRemoved(range) => self.remove_entries(range.clone()),
            _ => {}
        }
    }

    /// A thought is timed from the previous activity in the thread (the model
    /// starts thinking as soon as it has the previous step's result) until its
    /// last streamed chunk.
    fn record_streaming_thought(&mut self, thread: &AcpThread, entry_ix: usize, now: Instant) {
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
        self.user_messages_sent_at = self
            .user_messages_sent_at
            .drain()
            .filter_map(|(entry_ix, sent_at)| Some((shift(entry_ix)?, sent_at)))
            .collect();
    }
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
        settings_subscription,
        release_subscription,
    ]);
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
    let settings = ThemeSettings::get_global(cx);
    TextStyle {
        color: cx.theme().colors().text,
        font_family: settings.ui_font.family.clone(),
        font_fallbacks: settings.ui_font.fallbacks.clone(),
        font_features: settings.ui_font.features.clone(),
        font_size: settings.agent_ui_font_size(cx).into(),
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

pub(crate) fn thought_label(duration: Option<Duration>, is_streaming: bool) -> SharedString {
    if is_streaming {
        return "Thinking…".into();
    }
    match duration {
        Some(duration) => {
            let seconds = (duration.as_millis() + 500) / 1000;
            format!("Thought for {}s", seconds.max(1)).into()
        }
        None => "Thought".into(),
    }
}

pub(crate) fn format_timestamp(time: DateTime<Local>) -> SharedString {
    time.format("%-I:%M %p").to_string().into()
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
/// as "Default (recommended)" and puts the model it resolves to in the
/// description, which is what Claude Code shows.
fn model_display_name(entry: &ConfigOptionEntry) -> SharedString {
    if entry.value.0.as_ref() == "default"
        && let Some(description) = entry
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
    {
        return description.to_string().into();
    }
    entry
        .name
        .strip_suffix(" (recommended)")
        .unwrap_or(&entry.name)
        .to_string()
        .into()
}

/// The label of the model chip: "Opus 5.5 Extra high · Ultracode".
pub(crate) fn model_chip_label(options: &[acp_v2::SessionConfigOption]) -> Option<SharedString> {
    let model = first_select_option(options, &acp_v2::SessionConfigOptionCategory::Model)
        .and_then(current_select_entry)
        .map(|entry| model_display_name(&entry));
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
        "auto" => IconName::Sparkle,
        "bypassPermissions" => IconName::Warning,
        _ => IconName::Circle,
    }
}

pub(crate) const QUEUE_PLACEHOLDER: &str = "Queue another message…";

pub(crate) fn idle_placeholder(agent_display_name: &str) -> String {
    format!("Message {agent_display_name}…")
}

pub(crate) fn is_claude_code_placeholder(text: &str) -> bool {
    text == QUEUE_PLACEHOLDER || (text.starts_with("Message ") && text.ends_with('…'))
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
        assert_eq!(thought_label(None, true).as_ref(), "Thinking…");
        assert_eq!(thought_label(None, false).as_ref(), "Thought");
        assert_eq!(
            thought_label(Some(Duration::from_millis(200)), false).as_ref(),
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
        assert!(is_claude_code_placeholder(&idle_placeholder(
            "Claude Agent"
        )));
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
            activity
                .user_messages_sent_at
                .insert(entry_ix, Local::now());
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
        assert!(activity.user_message_sent_at(3).is_some());
        assert!(activity.user_message_sent_at(5).is_none());
    }
}
