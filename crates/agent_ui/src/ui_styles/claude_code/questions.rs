//! Claude Code's AskUserQuestion tool. claude-agent-acp (0.106.0,
//! `src/elicitation.ts`) asks its questions as an ACP form elicitation scoped to
//! the tool call: one `question_<n>` field per question (a titled `oneOf` string
//! for a single choice, an array of titled `anyOf` items for several), each
//! followed by a `question_<n>_custom` "Other" text field. An option's `preview`
//! travels in the option's `_meta["_claude/askUserQuestionOption"]`. The tool's
//! own input (`questions`) carries the same questions for the timeline row, and
//! its result is the Claude Code CLI's text, which the row reads back the way
//! the extension does.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

use acp_thread::{ElicitationEntryId, ToolCall, ToolCallStatus};
use agent_client_protocol::v2::schema as acp_v2;
use editor::Editor;
use gpui::{App, Entity, EntityId, Pixels, ScrollHandle, SharedString, Task};

use super::{thread_activity, thread_activity_mut};

pub(crate) const ASK_USER_QUESTION_TOOL: &str = "AskUserQuestion";
const OPTION_META_KEY: &str = "_claude/askUserQuestionOption";
const QUESTION_FIELD_PREFIX: &str = "question_";
const CUSTOM_FIELD_SUFFIX: &str = "_custom";

// The extension's strings (webview `index.js`, 2.1.322).
pub(crate) const OTHER_LABEL: &str = "Other";
pub(crate) const OTHER_PLACEHOLDER: &str = "Type your answer…";
pub(crate) const SUBMIT_LABEL: &str = "Submit answers";
pub(crate) const CANCEL_HINT: &str = "Esc to cancel";
pub(crate) const NO_PREVIEW_LABEL: &str = "No preview for this option";
pub(crate) const TOOL_INTERRUPTED_LABEL: &str = "Tool interrupted";

// The CLI's AskUserQuestion results (2.1.322, `mapToolResultToToolResultBlockParam`).
const ANSWERED_PREFIXES: [&str; 2] = ["Your questions have been answered: ", "The user answered: "];
const ANSWERED_SUFFIXES: [&str; 2] = [
    ". You can now continue with these answers in mind.",
    ". Read the answers carefully \u{2014} they may request clarification, changes, or that you not proceed \u{2014} and follow what they actually say.",
];
const RESPONDED_PREFIX: &str = "The user responded: ";
const NOT_ANSWERED: &str = "The user did not answer the questions.";
/// An answer followed by one of these carries a note or a chosen preview, which
/// the extension does not try to read back from the text.
const ANNOTATION_MARKERS: [&str; 2] = [" selected preview:", " notes: "];

// Results that are not answers, as the extension tells them apart.
const PERMISSION_REQUEST_FAILED: &str = "Tool permission request failed";
const INTERRUPTED_FOR_TOOL_USE: &str = "[Request interrupted by user for tool use]";
const SESSION_ENDED: &str = "[Tool call interrupted: the session ended before this call's result was recorded, so its outcome is unknown. Check whether it took effect before relying on it or running it again.]";
const RESULT_NOT_IN_COPY: &str = "[Tool call result not in this copy: this session was copied from another session before that session recorded this call's result. The call may have finished there, may still be running there, or may never have run. Check whether it took effect before relying on it or running it again.]";
const REJECTED_PREFIX: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). ";
const REJECTED: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.";

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QuestionOption {
    /// What the form expects back for this option (the label, for claude-agent-acp).
    pub value: String,
    pub label: SharedString,
    pub description: Option<SharedString>,
    pub preview: Option<SharedString>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Question {
    pub header: Option<SharedString>,
    pub question: SharedString,
    pub multi_select: bool,
    pub options: Vec<QuestionOption>,
}

impl Question {
    /// The extension only shows previews for single-choice questions.
    pub(crate) fn has_previews(&self) -> bool {
        !self.multi_select && self.options.iter().any(|option| option.preview.is_some())
    }

    /// "Other" comes after the options.
    pub(crate) fn other_row(&self) -> usize {
        self.options.len()
    }

    pub(crate) fn row_count(&self) -> usize {
        self.options.len() + 1
    }

    fn row_label(&self, row: usize) -> Option<&str> {
        if row == self.other_row() {
            return Some(OTHER_LABEL);
        }
        self.options.get(row).map(|option| option.label.as_ref())
    }
}

/// The form fields that answer one question.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QuestionFields {
    pub pick: String,
    pub custom: String,
}

/// A pending AskUserQuestion elicitation, as the question card shows it.
#[derive(Clone, Debug)]
pub(crate) struct QuestionForm {
    pub session_id: acp_v2::SessionId,
    pub elicitation_id: ElicitationEntryId,
    pub tool_call_id: Option<acp_v2::ToolCallId>,
    pub questions: Vec<Question>,
    pub fields: Vec<QuestionFields>,
}

fn non_empty(text: Option<&str>) -> Option<SharedString> {
    text.map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| SharedString::from(text.to_string()))
}

fn option_preview(meta: Option<&acp_v2::Meta>) -> Option<SharedString> {
    let preview = meta?.get(OPTION_META_KEY)?.get("preview")?.as_str()?;
    (!preview.trim().is_empty()).then(|| SharedString::from(preview.to_string()))
}

fn form_option(option: &acp_v2::EnumOption) -> QuestionOption {
    QuestionOption {
        value: option.value.clone(),
        label: option.title.clone().into(),
        description: non_empty(option.description.as_deref()),
        preview: option_preview(option.meta.as_ref()),
    }
}

/// The questions of an AskUserQuestion elicitation, with the fields that answer
/// them, or `None` for any other request (an MCP server's form, a URL, the
/// refusal fallback), which keeps Zed's own form.
pub(crate) fn ask_user_question_form(
    request: &acp_v2::CreateElicitationRequest,
) -> Option<(Vec<Question>, Vec<QuestionFields>)> {
    let acp_v2::ElicitationMode::Form(mode) = &request.mode else {
        return None;
    };
    if !matches!(request.scope(), acp_v2::ElicitationScope::Session(_)) {
        return None;
    }

    let mut picks = BTreeMap::new();
    let mut customs = BTreeMap::new();
    for (name, property) in &mode.requested_schema.properties {
        let suffix = name.strip_prefix(QUESTION_FIELD_PREFIX)?;
        match suffix.strip_suffix(CUSTOM_FIELD_SUFFIX) {
            Some(index) => {
                customs.insert(index.parse::<usize>().ok()?, (name, property));
            }
            None => {
                picks.insert(suffix.parse::<usize>().ok()?, (name, property));
            }
        }
    }
    if picks.is_empty() || picks.len() != customs.len() {
        return None;
    }

    let mut questions = Vec::with_capacity(picks.len());
    let mut fields = Vec::with_capacity(picks.len());
    for (index, (pick_name, property)) in picks {
        let (custom_name, custom_property) = customs.get(&index)?;
        if !matches!(
            custom_property,
            acp_v2::ElicitationPropertySchema::String(_)
        ) {
            return None;
        }
        let (title, description, multi_select, options) = match property {
            acp_v2::ElicitationPropertySchema::String(schema) => {
                let options = schema.one_of.as_ref()?;
                (&schema.title, &schema.description, false, options)
            }
            acp_v2::ElicitationPropertySchema::Array(schema) => {
                let acp_v2::MultiSelectItems::Titled(items) = &schema.items else {
                    return None;
                };
                (&schema.title, &schema.description, true, &items.options)
            }
            _ => return None,
        };
        if options.is_empty() {
            return None;
        }
        // claude-agent-acp puts a lone question in the request's message and
        // each of several in its field's description.
        let question = description
            .as_deref()
            .filter(|description| !description.trim().is_empty())
            .unwrap_or(&request.message);
        questions.push(Question {
            header: non_empty(title.as_deref()),
            question: question.to_string().into(),
            multi_select,
            options: options.iter().map(form_option).collect(),
        });
        fields.push(QuestionFields {
            pick: pick_name.clone(),
            custom: (*custom_name).clone(),
        });
    }
    Some((questions, fields))
}

/// The questions of an AskUserQuestion tool call's input, which the timeline
/// row lists. Entries without question text are skipped, as the extension does.
pub(crate) fn questions_from_input(raw_input: Option<&serde_json::Value>) -> Vec<Question> {
    let Some(entries) = raw_input
        .and_then(|input| input.get("questions"))
        .and_then(|questions| questions.as_array())
    else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let question = entry.get("question")?.as_str()?;
            let options = entry
                .get("options")
                .and_then(|options| options.as_array())
                .map(|options| {
                    options
                        .iter()
                        .filter_map(|option| {
                            let label = option.get("label")?.as_str()?;
                            Some(QuestionOption {
                                value: label.to_string(),
                                label: label.to_string().into(),
                                description: non_empty(
                                    option.get("description").and_then(|value| value.as_str()),
                                ),
                                preview: option
                                    .get("preview")
                                    .and_then(|value| value.as_str())
                                    .filter(|preview| !preview.trim().is_empty())
                                    .map(|preview| SharedString::from(preview.to_string())),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(Question {
                header: non_empty(entry.get("header").and_then(|value| value.as_str())),
                question: question.to_string().into(),
                multi_select: entry
                    .get("multiSelect")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
                options,
            })
        })
        .collect()
}

/// The answer the extension records for one question: the chosen labels joined
/// with ", ", with the typed text in place of "Other" (or "Other" itself when
/// nothing was typed).
pub(crate) fn answer_text(
    question: &Question,
    selection: &[usize],
    other_text: &str,
) -> Option<String> {
    let other_text = other_text.trim();
    let mut parts = Vec::new();
    let mut chose_other = false;
    for row in selection {
        if *row == question.other_row() {
            chose_other = true;
        } else if let Some(label) = question.row_label(*row) {
            parts.push(label.to_string());
        }
    }
    if chose_other {
        parts.push(if other_text.is_empty() {
            OTHER_LABEL.to_string()
        } else {
            other_text.to_string()
        });
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// The accepted form's content: each question's options in the order they were
/// chosen, and its "Other" text. claude-agent-acp turns these into the answers
/// the extension would have sent (`applyAskElicitationResponse`).
pub(crate) fn response_content(
    questions: &[Question],
    fields: &[QuestionFields],
    selections: &[Vec<usize>],
    other_texts: &[String],
) -> BTreeMap<String, acp_v2::ElicitationContentValue> {
    let mut content = BTreeMap::new();
    for (index, (question, fields)) in questions.iter().zip(fields).enumerate() {
        let selection = selections.get(index).map(Vec::as_slice).unwrap_or_default();
        let values = selection
            .iter()
            .filter_map(|row| question.options.get(*row))
            .map(|option| option.value.clone())
            .collect::<Vec<_>>();
        if question.multi_select {
            if !values.is_empty() {
                content.insert(fields.pick.clone(), values.into());
            }
        } else if let Some(value) = values.into_iter().next() {
            content.insert(fields.pick.clone(), value.into());
        }
        if selection.contains(&question.other_row()) {
            let other_text = other_texts
                .get(index)
                .map(|text| text.trim())
                .filter(|text| !text.is_empty())
                .unwrap_or(OTHER_LABEL);
            content.insert(fields.custom.clone(), other_text.into());
        }
    }
    content
}

/// What the user did with a question card, kept for the timeline row because
/// ACP reports neither a decline nor the answers as data.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum QuestionOutcome {
    /// Each question's text with the answer the extension would have recorded.
    Answered(Vec<(SharedString, String)>),
    Declined,
}

/// The status line of the extension's AskUserQuestion row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuestionStatus {
    Pending,
    NoAnswer,
    Declined,
    SessionEnded,
    Answered,
    /// A result the extension does not recognize; the row shows it like any
    /// other tool's output.
    Other,
}

impl QuestionStatus {
    pub(crate) fn label(self) -> Option<&'static str> {
        match self {
            Self::NoAnswer => Some("No answer"),
            Self::Declined => Some("Declined"),
            Self::SessionEnded => Some("Session ended before an answer"),
            Self::Answered => Some("Answered"),
            Self::Pending | Self::Other => None,
        }
    }
}

/// The extension's reading of an AskUserQuestion result.
pub(crate) fn question_status(result: Option<&str>, is_error: bool) -> QuestionStatus {
    let Some(result) = result else {
        return QuestionStatus::Pending;
    };
    if is_error {
        return if result.starts_with(SESSION_ENDED) {
            QuestionStatus::SessionEnded
        } else if result.starts_with(PERMISSION_REQUEST_FAILED)
            || result.starts_with(INTERRUPTED_FOR_TOOL_USE)
            || result.starts_with(RESULT_NOT_IN_COPY)
        {
            QuestionStatus::NoAnswer
        } else if result.starts_with(REJECTED_PREFIX) {
            QuestionStatus::Declined
        } else {
            QuestionStatus::Other
        };
    }
    if result == NOT_ANSWERED {
        QuestionStatus::NoAnswer
    } else if ANSWERED_PREFIXES
        .iter()
        .chain([&RESPONDED_PREFIX])
        .any(|prefix| result.starts_with(prefix))
    {
        QuestionStatus::Answered
    } else {
        QuestionStatus::Other
    }
}

/// Whether answers can be read back from the result text: no question or
/// option label may contain a double quote.
pub(crate) fn answers_are_parseable(questions: &[Question]) -> bool {
    questions.iter().all(|question| {
        !question.question.contains('"')
            && question
                .options
                .iter()
                .all(|option| !option.label.contains('"'))
    })
}

/// The `"question"="answer"` pairs of an answered result, as the extension reads
/// them back. Gives up (like the extension) when an answer carries a note or a
/// preview, or when the pairs don't rebuild the text exactly.
pub(crate) fn parse_answers(
    questions: &[SharedString],
    result: &str,
) -> Option<Vec<(SharedString, String)>> {
    let prefix = ANSWERED_PREFIXES
        .iter()
        .find(|prefix| result.starts_with(*prefix))?;
    let suffix = ANSWERED_SUFFIXES
        .iter()
        .find(|suffix| result.ends_with(*suffix))?;
    let body = result.get(prefix.len()..result.len().checked_sub(suffix.len())?)?;

    let mut answers = Vec::new();
    let mut position = 0;
    for (index, question) in questions.iter().enumerate() {
        let key = format!("\"{question}\"=");
        if !body
            .get(position..)
            .is_some_and(|rest| rest.starts_with(&key))
        {
            continue;
        }
        let value_start = position + key.len();
        let mut value_end = body.len();
        for later in questions.iter().skip(index + 1) {
            let separator = format!(", \"{later}\"=");
            if let Some(found) = body
                .get(value_start..)
                .and_then(|rest| rest.find(&separator))
            {
                value_end = value_end.min(value_start + found);
            }
        }
        let value = body.get(value_start..value_end)?;
        if !value.starts_with('"')
            || ANNOTATION_MARKERS
                .iter()
                .any(|marker| value.contains(&format!("\"{marker}")))
        {
            return None;
        }
        if let Some(last_quote) = value.rfind('"')
            && last_quote > 1
        {
            answers.push((question.clone(), value.get(1..last_quote)?.to_string()));
        }
        position = value_end + 2;
    }
    if answers.is_empty() {
        return None;
    }
    let rebuilt = answers
        .iter()
        .map(|(question, answer)| format!("\"{question}\"=\"{answer}\""))
        .collect::<Vec<_>>()
        .join(", ");
    (rebuilt == body).then_some(answers)
}

/// One question as the timeline lists it after an answer: the chosen labels and
/// any typed answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ReviewedAnswer {
    pub picked: Vec<String>,
    pub typed: Option<String>,
}

impl ReviewedAnswer {
    pub(crate) fn is_answered(&self) -> bool {
        !self.picked.is_empty() || self.typed.is_some()
    }

    pub(crate) fn shows_other(&self) -> bool {
        self.typed.is_some() || self.picked.iter().any(|label| label == OTHER_LABEL)
    }
}

/// Splits a recorded answer into the options it names and typed text, as the
/// extension does: a single choice is an option label or typed text; several
/// choices are labels separated by ", " followed by any typed text.
pub(crate) fn review_answer(question: &Question, answer: &str) -> ReviewedAnswer {
    let labels = question
        .options
        .iter()
        .map(|option| option.label.as_ref())
        .chain([OTHER_LABEL])
        .collect::<Vec<_>>();
    if !question.multi_select {
        return if labels.contains(&answer) {
            ReviewedAnswer {
                picked: vec![answer.to_string()],
                typed: None,
            }
        } else {
            ReviewedAnswer {
                picked: Vec::new(),
                typed: Some(answer.to_string()),
            }
        };
    }
    // Labels that contain the separator make the answer ambiguous.
    if question
        .options
        .iter()
        .any(|option| option.label.contains(", "))
    {
        return ReviewedAnswer {
            picked: Vec::new(),
            typed: Some(answer.to_string()),
        };
    }
    let mut picked = Vec::new();
    let mut rest = answer;
    loop {
        let longest = labels
            .iter()
            .filter(|label| rest == **label || rest.starts_with(&format!("{label}, ")))
            .max_by_key(|label| label.len());
        let Some(label) = longest else {
            break;
        };
        picked.push(label.to_string());
        rest = rest.get(label.len()..).unwrap_or_default();
        if rest.is_empty() {
            return ReviewedAnswer {
                picked,
                typed: None,
            };
        }
        rest = rest.get(2..).unwrap_or_default();
    }
    ReviewedAnswer {
        picked,
        typed: Some(rest.to_string()),
    }
}

/// The text result of an AskUserQuestion call: claude-agent-acp sends the CLI's
/// tool result as `rawOutput` (a string, or a list holding one text block).
pub(crate) fn question_result_text(tool_call: &ToolCall) -> Option<String> {
    match tool_call.raw_output.as_ref()? {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(blocks) => match blocks.as_slice() {
            [block] if block.get("type").and_then(|kind| kind.as_str()) == Some("text") => {
                block.get("text")?.as_str().map(str::to_string)
            }
            _ => None,
        },
        _ => None,
    }
}

/// How the timeline row presents an AskUserQuestion call.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QuestionRowState {
    pub status: QuestionStatus,
    /// Each question's recorded answer, when the call was answered and the
    /// answers are known.
    pub answers: Option<Vec<(SharedString, String)>>,
    /// The extension's "Tool interrupted" line follows the row.
    pub interrupted: bool,
    /// The result is shown as OUT, because it is not one of the texts the
    /// status line already says.
    pub shows_output: bool,
}

pub(crate) fn question_row_state(
    questions: &[Question],
    tool_status: ToolCallStatus,
    result: Option<&str>,
    outcome: Option<&QuestionOutcome>,
) -> QuestionRowState {
    let is_error = matches!(
        tool_status,
        ToolCallStatus::Failed | ToolCallStatus::Rejected
    );
    let row = |status, answers, interrupted, shows_output| QuestionRowState {
        status,
        answers,
        interrupted,
        shows_output,
    };
    // Esc in the card declines and interrupts the turn, as in the extension;
    // over ACP that reaches the agent as a cancelled request.
    if outcome == Some(&QuestionOutcome::Declined) {
        return row(QuestionStatus::Declined, None, true, false);
    }
    // The turn was stopped while the question was up.
    if tool_status == ToolCallStatus::Canceled && result.is_none() {
        return row(QuestionStatus::NoAnswer, None, true, false);
    }
    let status = question_status(result, is_error);
    let Some(result) = result else {
        return row(status, None, false, false);
    };
    let interrupted = is_error
        && (result.starts_with(PERMISSION_REQUEST_FAILED)
            || result.starts_with(INTERRUPTED_FOR_TOOL_USE));
    match status {
        QuestionStatus::Pending => row(status, None, false, false),
        QuestionStatus::Other => row(status, None, false, true),
        QuestionStatus::NoAnswer => {
            let is_standard = result == NOT_ANSWERED
                || result == INTERRUPTED_FOR_TOOL_USE
                || result == SESSION_ENDED;
            row(status, None, interrupted, !is_standard && !interrupted)
        }
        QuestionStatus::SessionEnded => {
            let is_standard = result == INTERRUPTED_FOR_TOOL_USE || result == SESSION_ENDED;
            row(status, None, interrupted, !is_standard)
        }
        QuestionStatus::Declined => row(status, None, interrupted, result != REJECTED),
        QuestionStatus::Answered => {
            let answers = match outcome {
                Some(QuestionOutcome::Answered(answers)) => Some(answers.clone()),
                _ if answers_are_parseable(questions) => {
                    let texts = questions
                        .iter()
                        .map(|question| question.question.clone())
                        .collect::<Vec<_>>();
                    parse_answers(&texts, result)
                }
                _ => None,
            };
            let shows_output = answers.is_none();
            row(status, answers, interrupted, shows_output)
        }
    }
}

/// The question card's state while a question is pending: the question shown,
/// the highlighted row, each question's chosen rows (in the order they were
/// chosen, which is the order the answer lists them in) and its "Other" text.
pub(crate) struct QuestionCardState {
    pub elicitation_id: ElicitationEntryId,
    pub active_question: usize,
    pub highlighted_row: usize,
    pub selections: Vec<Vec<usize>>,
    pub other_editors: Vec<Option<Entity<Editor>>>,
    pub folded: bool,
    /// A single choice is shown chosen for a moment before the next question.
    pub confirming_row: Option<usize>,
    pub advance_task: Option<Task<()>>,
    pub scroll_handle: ScrollHandle,
    /// The card's width in the last frame, which picks the preview layout.
    pub width: Rc<Cell<Pixels>>,
}

/// What choosing a row leads to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowChoice {
    Stay,
    /// "Other" was chosen: its text field takes focus.
    EditOther,
    /// A single choice that moves on to the next question shortly.
    Advance,
}

impl QuestionCardState {
    pub(crate) fn new(elicitation_id: ElicitationEntryId, question_count: usize) -> Self {
        Self {
            elicitation_id,
            active_question: 0,
            highlighted_row: 0,
            selections: vec![Vec::new(); question_count],
            other_editors: vec![None; question_count],
            folded: false,
            confirming_row: None,
            advance_task: None,
            scroll_handle: ScrollHandle::new(),
            width: Rc::new(Cell::new(Pixels::ZERO)),
        }
    }

    pub(crate) fn is_answered(&self, question_ix: usize) -> bool {
        self.selections
            .get(question_ix)
            .is_some_and(|selection| !selection.is_empty())
    }

    /// The extension only submits once every question has an answer.
    pub(crate) fn all_answered(&self) -> bool {
        (0..self.selections.len()).all(|question_ix| self.is_answered(question_ix))
    }

    pub(crate) fn is_selected(&self, question_ix: usize, row: usize) -> bool {
        self.selections
            .get(question_ix)
            .is_some_and(|selection| selection.contains(&row))
    }

    /// Chooses `row` of question `question_ix`: a single choice replaces the
    /// previous one, a multiple choice toggles.
    pub(crate) fn choose(
        &mut self,
        question: &Question,
        question_ix: usize,
        row: usize,
    ) -> RowChoice {
        let question_count = self.selections.len();
        let Some(selection) = self.selections.get_mut(question_ix) else {
            return RowChoice::Stay;
        };
        if question.multi_select {
            if let Some(position) = selection.iter().position(|chosen| *chosen == row) {
                selection.remove(position);
                return RowChoice::Stay;
            }
            selection.push(row);
            return if row == question.other_row() {
                RowChoice::EditOther
            } else {
                RowChoice::Stay
            };
        }
        selection.clear();
        selection.push(row);
        if row == question.other_row() {
            RowChoice::EditOther
        } else if self.confirming_row.is_none() && question_ix + 1 < question_count {
            self.confirming_row = Some(row);
            RowChoice::Advance
        } else {
            RowChoice::Stay
        }
    }

    pub(crate) fn show_question(&mut self, question_ix: usize) {
        if question_ix >= self.selections.len() || question_ix == self.active_question {
            return;
        }
        self.active_question = question_ix;
        self.highlighted_row = 0;
        // A pending move to the next question sees this and does nothing.
        self.confirming_row = None;
    }

    /// Moves the highlight through the options and "Other", wrapping around.
    pub(crate) fn move_highlight(&mut self, row_count: usize, offset: isize) {
        if row_count == 0 {
            return;
        }
        let current = self.highlighted_row.min(row_count - 1) as isize;
        self.highlighted_row = (current + offset).rem_euclid(row_count as isize) as usize;
    }

    pub(crate) fn other_texts(&self, cx: &App) -> Vec<String> {
        self.other_editors
            .iter()
            .map(|editor| {
                editor
                    .as_ref()
                    .map(|editor| editor.read(cx).text(cx))
                    .unwrap_or_default()
            })
            .collect()
    }

    pub(crate) fn answers(&self, questions: &[Question], cx: &App) -> Vec<(SharedString, String)> {
        let other_texts = self.other_texts(cx);
        questions
            .iter()
            .enumerate()
            .filter_map(|(index, question)| {
                let selection = self.selections.get(index)?;
                let other_text = other_texts
                    .get(index)
                    .map(String::as_str)
                    .unwrap_or_default();
                Some((
                    question.question.clone(),
                    answer_text(question, selection, other_text)?,
                ))
            })
            .collect()
    }
}

pub(crate) fn question_outcome(
    thread_view_id: EntityId,
    tool_call_id: &acp_v2::ToolCallId,
    cx: &App,
) -> Option<QuestionOutcome> {
    thread_activity(thread_view_id, cx)?
        .question_outcomes
        .get(tool_call_id)
        .cloned()
}

pub(crate) fn set_question_outcome(
    thread_view_id: EntityId,
    tool_call_id: acp_v2::ToolCallId,
    outcome: QuestionOutcome,
    cx: &mut App,
) {
    if let Some(activity) = thread_activity_mut(thread_view_id, cx) {
        activity.question_outcomes.insert(tool_call_id, outcome);
    }
}

/// The extension shows an answered row's questions until its status line is clicked.
pub(crate) fn question_row_collapsed(
    thread_view_id: EntityId,
    tool_call_id: &acp_v2::ToolCallId,
    cx: &App,
) -> bool {
    thread_activity(thread_view_id, cx)
        .is_some_and(|activity| activity.collapsed_question_rows.contains(tool_call_id))
}

pub(crate) fn toggle_question_row(
    thread_view_id: EntityId,
    tool_call_id: &acp_v2::ToolCallId,
    cx: &mut App,
) {
    if let Some(activity) = thread_activity_mut(thread_view_id, cx)
        && !activity.collapsed_question_rows.remove(tool_call_id)
    {
        activity
            .collapsed_question_rows
            .insert(tool_call_id.clone());
    }
}

pub(crate) fn question_card_state(
    thread_view_id: EntityId,
    cx: &App,
) -> Option<&QuestionCardState> {
    thread_activity(thread_view_id, cx)?.question_card.as_ref()
}

pub(crate) fn question_card_state_mut(
    thread_view_id: EntityId,
    cx: &mut App,
) -> Option<&mut QuestionCardState> {
    thread_activity_mut(thread_view_id, cx)?
        .question_card
        .as_mut()
}

/// Starts the card for `elicitation_id` unless it is already showing, and
/// returns the state it replaced (another question's), if any.
pub(crate) fn sync_question_card(
    thread_view_id: EntityId,
    elicitation_id: Option<&ElicitationEntryId>,
    question_count: usize,
    cx: &mut App,
) -> Option<QuestionCardState> {
    let activity = thread_activity_mut(thread_view_id, cx)?;
    match elicitation_id {
        Some(elicitation_id)
            if activity
                .question_card
                .as_ref()
                .is_some_and(|card| &card.elicitation_id == elicitation_id) =>
        {
            None
        }
        Some(elicitation_id) => activity.question_card.replace(QuestionCardState::new(
            elicitation_id.clone(),
            question_count,
        )),
        None => activity.question_card.take(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The request claude-agent-acp 0.106.0 sends for one question with a preview
    /// (its `ask-user-question` v2 scenario snapshot).
    fn single_question_request() -> acp_v2::CreateElicitationRequest {
        serde_json::from_value(json!({
            "message": "Which database?",
            "mode": "form",
            "requestedSchema": {
                "properties": {
                    "question_0": {
                        "oneOf": [
                            {
                                "_meta": { "_claude/askUserQuestionOption": { "preview": "CREATE TABLE t ();" } },
                                "const": "Postgres",
                                "description": "Relational",
                                "title": "Postgres"
                            },
                            { "const": "Redis", "description": "Key-value", "title": "Redis" }
                        ],
                        "title": "Database",
                        "type": "string"
                    },
                    "question_0_custom": {
                        "description": "Type your own answer, or add a note to the option you chose above (optional).",
                        "title": "Other",
                        "type": "string"
                    }
                },
                "type": "object"
            },
            "sessionId": "session-1",
            "toolCallId": "toolu_ask"
        }))
        .expect("adapter elicitation request")
    }

    fn two_question_request(session_id: &str, tool_call_id: &str) -> serde_json::Value {
        json!({
            "message": "Please answer the following questions.",
            "mode": "form",
            "requestedSchema": {
                "properties": {
                    "question_0": {
                        "oneOf": [
                            { "const": "Right sidebar", "description": "Next to the editor.", "title": "Right sidebar" },
                            { "const": "Bottom panel", "title": "Bottom panel" }
                        ],
                        "title": "Dock target",
                        "description": "Which panel should the agent dock into?",
                        "type": "string"
                    },
                    "question_0_custom": { "title": "Other", "type": "string" },
                    "question_1": {
                        "items": { "anyOf": [
                            { "const": "Inline diffs", "title": "Inline diffs" },
                            { "const": "Tool call rows", "title": "Tool call rows" },
                            { "const": "Codicon icons", "title": "Codicon icons" }
                        ] },
                        "title": "Features",
                        "description": "Which features do you want?",
                        "type": "array"
                    },
                    "question_1_custom": { "title": "Other", "type": "string" }
                },
                "type": "object"
            },
            "sessionId": session_id,
            "toolCallId": tool_call_id
        })
    }

    #[test]
    fn test_ask_user_question_form() {
        let (questions, fields) =
            ask_user_question_form(&single_question_request()).expect("an AskUserQuestion form");
        assert_eq!(
            questions,
            vec![Question {
                header: Some("Database".into()),
                question: "Which database?".into(),
                multi_select: false,
                options: vec![
                    QuestionOption {
                        value: "Postgres".into(),
                        label: "Postgres".into(),
                        description: Some("Relational".into()),
                        preview: Some("CREATE TABLE t ();".into()),
                    },
                    QuestionOption {
                        value: "Redis".into(),
                        label: "Redis".into(),
                        description: Some("Key-value".into()),
                        preview: None,
                    },
                ],
            }]
        );
        assert_eq!(
            fields,
            vec![QuestionFields {
                pick: "question_0".into(),
                custom: "question_0_custom".into(),
            }]
        );
        assert!(questions[0].has_previews());

        let request: acp_v2::CreateElicitationRequest =
            serde_json::from_value(two_question_request("session-1", "toolu_ask"))
                .expect("adapter elicitation request");
        let (questions, _) = ask_user_question_form(&request).expect("an AskUserQuestion form");
        assert_eq!(
            questions[0].question,
            "Which panel should the agent dock into?"
        );
        assert!(!questions[0].multi_select);
        assert_eq!(questions[1].question, "Which features do you want?");
        assert!(questions[1].multi_select);
        assert_eq!(questions[1].options.len(), 3);

        // The refusal fallback and MCP servers' forms keep Zed's form.
        let refusal: acp_v2::CreateElicitationRequest = serde_json::from_value(json!({
            "message": "Retry?",
            "mode": "form",
            "requestedSchema": { "type": "object", "properties": { "choice": {
                "type": "string",
                "oneOf": [{ "const": "retry_fallback", "title": "Retry" }]
            } } },
            "sessionId": "session-1"
        }))
        .expect("refusal fallback request");
        assert!(ask_user_question_form(&refusal).is_none());
    }

    fn question(multi_select: bool, labels: &[&str]) -> Question {
        Question {
            header: None,
            question: "Pick".into(),
            multi_select,
            options: labels
                .iter()
                .map(|label| QuestionOption {
                    value: label.to_string(),
                    label: label.to_string().into(),
                    description: None,
                    preview: None,
                })
                .collect(),
        }
    }

    #[test]
    fn test_answers_and_response_content() {
        let single = question(false, &["Postgres", "Redis"]);
        let multi = question(true, &["A", "B", "C"]);
        assert_eq!(answer_text(&single, &[1], ""), Some("Redis".into()));
        assert_eq!(
            answer_text(&single, &[2], "  SQLite "),
            Some("SQLite".into())
        );
        assert_eq!(answer_text(&single, &[2], ""), Some("Other".into()));
        assert_eq!(answer_text(&multi, &[2, 0, 3], "D"), Some("C, A, D".into()));
        assert_eq!(answer_text(&multi, &[], ""), None);

        let fields = vec![
            QuestionFields {
                pick: "question_0".into(),
                custom: "question_0_custom".into(),
            },
            QuestionFields {
                pick: "question_1".into(),
                custom: "question_1_custom".into(),
            },
        ];
        let content = response_content(
            &[single.clone(), multi.clone()],
            &fields,
            &[vec![2], vec![2, 0, 3]],
            &["SQLite".into(), String::new()],
        );
        assert_eq!(
            serde_json::to_value(&content).expect("content serializes"),
            json!({
                "question_0_custom": "SQLite",
                "question_1": ["C", "A"],
                "question_1_custom": "Other",
            })
        );
        let content = response_content(&[single], &fields[..1], &[vec![0]], &[String::new()]);
        assert_eq!(
            serde_json::to_value(&content).expect("content serializes"),
            json!({ "question_0": "Postgres" })
        );
    }

    #[test]
    fn test_card_choices() {
        let single = question(false, &["A", "B"]);
        let multi = question(true, &["A", "B"]);
        let mut card = QuestionCardState::new(ElicitationEntryId("id".into()), 2);
        assert!(!card.all_answered());
        assert_eq!(card.choose(&single, 0, 1), RowChoice::Advance);
        // A second choice while the first is shown chosen doesn't advance twice.
        assert_eq!(card.choose(&single, 0, 0), RowChoice::Stay);
        assert_eq!(card.selections[0], vec![0]);
        assert_eq!(card.choose(&single, 0, 2), RowChoice::EditOther);
        card.show_question(1);
        assert_eq!(card.confirming_row, None);
        assert_eq!(card.choose(&multi, 1, 1), RowChoice::Stay);
        assert_eq!(card.choose(&multi, 1, 0), RowChoice::Stay);
        assert_eq!(card.selections[1], vec![1, 0]);
        assert!(card.all_answered());
        assert_eq!(card.choose(&multi, 1, 1), RowChoice::Stay);
        assert_eq!(card.selections[1], vec![0]);
        assert_eq!(card.choose(&multi, 1, 2), RowChoice::EditOther);
        // The last question never advances.
        assert_eq!(card.choose(&single, 1, 0), RowChoice::Stay);

        card.highlighted_row = 0;
        card.move_highlight(3, -1);
        assert_eq!(card.highlighted_row, 2);
        card.move_highlight(3, 1);
        assert_eq!(card.highlighted_row, 0);
    }

    #[test]
    fn test_question_status() {
        assert_eq!(question_status(None, false), QuestionStatus::Pending);
        assert_eq!(
            question_status(
                Some(
                    "Your questions have been answered: \"Q\"=\"A\". You can now continue with these answers in mind."
                ),
                false
            ),
            QuestionStatus::Answered
        );
        assert_eq!(
            question_status(Some("The user responded: later"), false),
            QuestionStatus::Answered
        );
        assert_eq!(
            question_status(Some(NOT_ANSWERED), false),
            QuestionStatus::NoAnswer
        );
        assert_eq!(
            question_status(
                Some("Tool permission request failed: Error: Tool use aborted"),
                true
            ),
            QuestionStatus::NoAnswer
        );
        assert_eq!(
            question_status(Some(SESSION_ENDED), true),
            QuestionStatus::SessionEnded
        );
        assert_eq!(
            question_status(Some(REJECTED), true),
            QuestionStatus::Declined
        );
        assert_eq!(question_status(Some("Boom"), true), QuestionStatus::Other);
        assert_eq!(
            question_status(Some("Something else"), false),
            QuestionStatus::Other
        );
    }

    #[test]
    fn test_parse_answers() {
        let questions: Vec<SharedString> =
            vec!["Which?".into(), "Features?".into(), "Skipped?".into()];
        assert_eq!(
            parse_answers(
                &questions,
                "Your questions have been answered: \"Which?\"=\"Right, sidebar\", \"Features?\"=\"A, B\". You can now continue with these answers in mind."
            ),
            Some(vec![
                ("Which?".into(), "Right, sidebar".into()),
                ("Features?".into(), "A, B".into()),
            ])
        );
        assert_eq!(
            parse_answers(
                &questions,
                "The user answered: \"Features?\"=\"Something typed\". Read the answers carefully \u{2014} they may request clarification, changes, or that you not proceed \u{2014} and follow what they actually say."
            ),
            Some(vec![("Features?".into(), "Something typed".into())])
        );
        // Notes can't be told apart from the answer reliably.
        assert_eq!(
            parse_answers(
                &questions,
                "The user answered: \"Which?\"=\"A\" notes: hi. Read the answers carefully \u{2014} they may request clarification, changes, or that you not proceed \u{2014} and follow what they actually say."
            ),
            None
        );
        assert_eq!(parse_answers(&questions, NOT_ANSWERED), None);
    }

    #[test]
    fn test_review_answer() {
        let single = question(false, &["Postgres", "Redis"]);
        assert_eq!(
            review_answer(&single, "Redis"),
            ReviewedAnswer {
                picked: vec!["Redis".into()],
                typed: None
            }
        );
        assert_eq!(
            review_answer(&single, "SQLite"),
            ReviewedAnswer {
                picked: Vec::new(),
                typed: Some("SQLite".into())
            }
        );
        let multi = question(true, &["A", "A B", "C"]);
        assert_eq!(
            review_answer(&multi, "A B, C, typed, more"),
            ReviewedAnswer {
                picked: vec!["A B".into(), "C".into()],
                typed: Some("typed, more".into())
            }
        );
        assert_eq!(
            review_answer(&multi, "C, Other"),
            ReviewedAnswer {
                picked: vec!["C".into(), "Other".into()],
                typed: None
            }
        );
        let ambiguous = question(true, &["A, B", "C"]);
        assert_eq!(
            review_answer(&ambiguous, "C"),
            ReviewedAnswer {
                picked: Vec::new(),
                typed: Some("C".into())
            }
        );
    }

    #[test]
    fn test_question_row_state() {
        let questions = vec![question(false, &["Postgres", "Redis"])];
        let answered = "Your questions have been answered: \"Pick\"=\"Redis\". You can now continue with these answers in mind.";
        assert_eq!(
            question_row_state(&questions, ToolCallStatus::Completed, Some(answered), None),
            QuestionRowState {
                status: QuestionStatus::Answered,
                answers: Some(vec![("Pick".into(), "Redis".into())]),
                interrupted: false,
                shows_output: false,
            }
        );
        let aborted = "Tool permission request failed: Error: Tool use aborted";
        assert_eq!(
            question_row_state(&questions, ToolCallStatus::Failed, Some(aborted), None),
            QuestionRowState {
                status: QuestionStatus::NoAnswer,
                answers: None,
                interrupted: true,
                shows_output: false,
            }
        );
        assert_eq!(
            question_row_state(
                &questions,
                ToolCallStatus::Failed,
                Some(aborted),
                Some(&QuestionOutcome::Declined)
            )
            .status,
            QuestionStatus::Declined
        );
        assert_eq!(
            question_row_state(&questions, ToolCallStatus::Canceled, None, None),
            QuestionRowState {
                status: QuestionStatus::NoAnswer,
                answers: None,
                interrupted: true,
                shows_output: false,
            }
        );
        assert_eq!(
            question_row_state(&questions, ToolCallStatus::InProgress, None, None).status,
            QuestionStatus::Pending
        );
        assert_eq!(
            question_row_state(
                &questions,
                ToolCallStatus::Completed,
                Some(NOT_ANSWERED),
                None
            ),
            QuestionRowState {
                status: QuestionStatus::NoAnswer,
                answers: None,
                interrupted: false,
                shows_output: false,
            }
        );
        let noted = "The user answered: \"Pick\"=\"Redis\" notes: fast. Read the answers carefully \u{2014} they may request clarification, changes, or that you not proceed \u{2014} and follow what they actually say.";
        let local = QuestionOutcome::Answered(vec![("Pick".into(), "Redis".into())]);
        assert!(
            question_row_state(&questions, ToolCallStatus::Completed, Some(noted), None)
                .shows_output,
            "unreadable answers show the result instead"
        );
        assert_eq!(
            question_row_state(
                &questions,
                ToolCallStatus::Completed,
                Some(noted),
                Some(&local)
            )
            .answers,
            Some(vec![("Pick".into(), "Redis".into())])
        );
    }

    #[test]
    fn test_questions_from_input() {
        let questions = questions_from_input(Some(&json!({
            "questions": [
                {
                    "question": "Which database?",
                    "header": "Database",
                    "multiSelect": false,
                    "options": [
                        { "label": "Postgres", "description": "Relational", "preview": "CREATE TABLE t ();" },
                        { "label": "Redis" },
                        { "description": "no label" }
                    ]
                },
                { "header": "No question" }
            ]
        })));
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].header.as_deref(), Some("Database"));
        assert_eq!(questions[0].options.len(), 2);
        assert_eq!(
            questions[0].options[0].preview.as_deref(),
            Some("CREATE TABLE t ();")
        );
        assert!(questions_from_input(Some(&json!({}))).is_empty());
    }
}
