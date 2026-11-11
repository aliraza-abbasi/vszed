mod tool_row;

pub(crate) use tool_row::ToolRow;

use std::ops::Range;
use std::time::{Duration, Instant};

use acp_thread::{
    AcpThread, AcpThreadEvent, AgentThreadEntry, AssistantMessageChunk, ToolCall, ToolCallContent,
    ToolCallStatus,
};
use agent_client_protocol::v2::schema as acp_v2;
use chrono::{DateTime, Local};
use collections::HashMap;
use gpui::{
    Animation, AnimationExt as _, App, Context, ElementId, EntityId, Global, Hsla, Pixels,
    pulsating_between, px,
};
use settings::{Settings as _, SettingsStore};
use ui::prelude::*;

use super::AgentUiStyleSettings;
use crate::conversation_view::ThreadView;

/// Width of the left gutter that holds each timeline step's status dot.
pub(crate) const TIMELINE_GUTTER: Pixels = px(16.);
const TIMELINE_DOT_SIZE: Pixels = px(7.);

/// The `_meta` namespace claude-agent-acp uses for Claude Code specific data.
const CLAUDE_CODE_META_KEY: &str = "claudeCode";
const CLAUDE_CODE_TOOL_NAME_KEY: &str = "toolName";

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
    last_activity_at: Option<Instant>,
    thoughts: HashMap<(usize, usize), ThoughtTiming>,
    user_messages_sent_at: HashMap<usize, DateTime<Local>>,
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

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|thread_view: &mut ThreadView, _window, cx| {
        track_thread_view(thread_view, cx);
    })
    .detach();
}

/// Records timeline activity for every thread view, whichever style is active,
/// so that switching to the Claude Code style mid-thread still knows recent
/// timings. Also re-measures the list when the style changes, because every
/// row changes height.
fn track_thread_view(thread_view: &mut ThreadView, cx: &mut Context<ThreadView>) {
    let thread_view_id = cx.entity_id();
    cx.default_global::<ClaudeCodeActivity>()
        .threads
        .insert(thread_view_id, ThreadActivity::default());

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
    let settings_subscription = cx.observe_global::<SettingsStore>(move |thread_view, cx| {
        let new_ui_style = AgentUiStyleSettings::get_global(cx).ui_style;
        if new_ui_style != ui_style {
            ui_style = new_ui_style;
            thread_view.list_state.remeasure();
            cx.notify();
        }
    });

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

pub(crate) fn summarize_tool_call(tool_call: &ToolCall, label: &str) -> ToolCallSummary {
    let label = label.trim();
    let is_execute = matches!(tool_call.kind(), acp_v2::ToolKind::Execute);

    let name =
        claude_code_tool_name(tool_call.meta.as_ref()).unwrap_or_else(|| match tool_call.kind() {
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
/// send tool output as markdown.
fn strip_code_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(after_fence) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let Some((_info, body)) = after_fence.split_once('\n') else {
        return trimmed;
    };
    body.trim_end()
        .strip_suffix("```")
        .map(|body| body.trim_end_matches('\n'))
        .unwrap_or(trimmed)
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
