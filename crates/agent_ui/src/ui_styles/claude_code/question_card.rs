// Declared from `thread.rs` (via a `#[path]` attribute), so this module is a
// descendant of `thread_view` and can answer questions through the same
// `ThreadView` and `Conversation` paths as Zed's elicitation form.

use std::time::Duration;

use acp_thread::ElicitationStatus;
use editor::Editor;
use gpui::{FocusHandle, FontWeight, KeyDownEvent, canvas};

use super::claude_code_prompt_box::PROMPT_BOX_RADIUS;
use super::*;
use crate::ui_styles::claude_code::{
    ASK_USER_QUESTION_TOOL, CANCEL_HINT, NO_PREVIEW_LABEL, OTHER_LABEL, OTHER_PLACEHOLDER,
    Question, QuestionCardState, QuestionForm, QuestionOutcome, QuestionStatus, RowChoice,
    SUBMIT_LABEL, StepStatus, TOOL_INTERRUPTED_LABEL, ToolRow, ask_user_question_form,
    display_lines, question_card_state, question_card_state_mut, question_outcome,
    question_result_text, question_row_collapsed, question_row_state, questions_from_input,
    reported_claude_code_tool, response_content, review_answer, set_question_outcome,
    sync_question_card, toggle_question_row, tool_output_text,
};

/// The extension shows a single choice as chosen for this long before moving
/// on to the next question.
const ADVANCE_DELAY: Duration = Duration::from_millis(300);
/// Below this card width (the extension's 576px media query, less the prompt
/// area's padding) a preview goes under the options instead of beside them.
const SIDE_BY_SIDE_PREVIEW_WIDTH: Pixels = px(552.);
const MARKER_SIZE: Pixels = px(16.);
const TAB_MAX_WIDTH: Pixels = px(120.);
/// The "Other" answer grows with Shift+Enter newlines up to this many lines,
/// then scrolls.
const OTHER_MAX_LINES: usize = 6;

/// A question's option or "Other", as a card or timeline row draws it.
struct OptionRowContent {
    multi_select: bool,
    label: SharedString,
    description: Option<SharedString>,
    selected: bool,
}

fn option_marker(multi_select: bool, selected: bool, cx: &App) -> AnyElement {
    let colors = cx.theme().colors();
    let accent = colors.text_accent;
    let marker = div()
        .flex_none()
        .size(MARKER_SIZE)
        .flex()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(if selected { accent } else { colors.border });
    if multi_select {
        marker
            .rounded(px(3.))
            .when(selected, |this| {
                this.bg(accent).child(
                    Icon::new(IconName::Check)
                        .size(IconSize::XSmall)
                        .color(Color::Custom(colors.editor_background)),
                )
            })
            .into_any_element()
    } else {
        marker
            .rounded_full()
            .when(selected, |this| {
                this.child(div().size(px(8.)).rounded_full().bg(accent))
            })
            .into_any_element()
    }
}

/// The marker, the bold label and the muted description of one option.
fn option_row_body(content: OptionRowContent, cx: &App) -> Div {
    h_flex()
        .w_full()
        .min_w_0()
        .items_start()
        .gap(px(10.))
        .p_1()
        .rounded(px(4.))
        .child(div().flex_none().self_center().child(option_marker(
            content.multi_select,
            content.selected,
            cx,
        )))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(Label::new(content.label).weight(FontWeight::MEDIUM))
                .children(content.description.map(|description| {
                    Label::new(description)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                })),
        )
}

/// A preview, as the extension shows it: monospace, one line per line, unwrapped.
fn preview_lines(preview: &str) -> Vec<String> {
    let lines = display_lines(preview);
    // A preview that is one fenced block shows the block's content.
    match lines.as_slice() {
        [first, middle @ .., last]
            if first.trim_start().starts_with("```") && last.trim() == "```" =>
        {
            middle.to_vec()
        }
        _ => lines,
    }
}

impl ThreadView {
    /// The pending AskUserQuestion elicitation the card answers: this thread's
    /// own, else one of its subagents'.
    pub(in crate::conversation_view::thread_view) fn claude_code_question_form(
        &self,
        cx: &App,
    ) -> Option<QuestionForm> {
        let own_session_id = self.thread.read(cx).session_id().clone();
        let conversation = self.conversation.read(cx);
        let sessions = std::iter::once(&own_session_id).chain(
            conversation
                .elicitation_requests
                .keys()
                .filter(|session_id| **session_id != own_session_id),
        );
        for session_id in sessions {
            let (Some(elicitation_ids), Some(thread)) = (
                conversation.elicitation_requests.get(session_id),
                conversation.thread(session_id),
            ) else {
                continue;
            };
            let thread = thread.read(cx);
            for elicitation_id in elicitation_ids {
                let Some((_, elicitation)) = thread.elicitation(elicitation_id) else {
                    continue;
                };
                if !matches!(elicitation.status, ElicitationStatus::Pending { .. }) {
                    continue;
                }
                let Some((questions, fields)) = ask_user_question_form(&elicitation.request) else {
                    continue;
                };
                let tool_call_id = match elicitation.request.scope() {
                    acp_v2::ElicitationScope::Session(scope) => scope.tool_call_id.clone(),
                    _ => None,
                };
                return Some(QuestionForm {
                    session_id: session_id.clone(),
                    elicitation_id: elicitation_id.clone(),
                    tool_call_id,
                    questions,
                    fields,
                });
            }
        }
        None
    }

    /// Forgets the card once no question is pending. Focus left in its "Other"
    /// field goes back to the prompt.
    pub(in crate::conversation_view::thread_view) fn close_claude_code_question_card(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(card) = sync_question_card(cx.entity_id(), None, 0, cx) else {
            return;
        };
        if Self::card_editor_focused(&card, window, cx) {
            let editor_focus = self.message_editor.focus_handle(cx);
            cx.defer_in(window, move |_, window, cx| {
                window.focus(&editor_focus, cx);
            });
        }
    }

    fn card_editor_focused(card: &QuestionCardState, window: &Window, cx: &App) -> bool {
        card.other_editors
            .iter()
            .flatten()
            .any(|editor| editor.focus_handle(cx).is_focused(window))
    }

    fn claude_code_other_editor_focused(&self, window: &Window, cx: &Context<Self>) -> bool {
        question_card_state(cx.entity_id(), cx)
            .is_some_and(|card| Self::card_editor_focused(card, window, cx))
    }

    /// Claude Code's question card, in the prompt box's place: one tab per
    /// question, the question, its options (radio buttons for one choice, check
    /// boxes for several) with "Other", "1 Submit answers" and "Esc to cancel".
    /// Options with a preview show it beside the options, or under them when the
    /// card is narrow.
    pub(in crate::conversation_view::thread_view) fn render_claude_code_question_card(
        &mut self,
        form: QuestionForm,
        focus_handle: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let thread_view_id = cx.entity_id();
        if let Some(replaced) = sync_question_card(
            thread_view_id,
            Some(&form.elicitation_id),
            form.questions.len(),
            cx,
        ) && Self::card_editor_focused(&replaced, window, cx)
        {
            let focus_handle = focus_handle.clone();
            cx.defer_in(window, move |_, window, cx| {
                window.focus(&focus_handle, cx);
            });
        }
        let Some(card) = question_card_state(thread_view_id, cx) else {
            return Empty.into_any_element();
        };
        let active_question = card
            .active_question
            .min(form.questions.len().saturating_sub(1));
        let Some(question) = form.questions.get(active_question) else {
            return Empty.into_any_element();
        };

        let colors = cx.theme().colors();
        let accent = colors.text_accent;
        let is_focused = focus_handle.contains_focused(window, cx);
        let folded = card.folded;
        let all_answered = card.all_answered();
        let width = card.width.clone();
        let viewport_height = window.viewport_size().height;

        let tabs = form
            .questions
            .iter()
            .enumerate()
            .map(|(question_ix, tab_question)| {
                let is_active = question_ix == active_question;
                let is_answered = card.is_answered(question_ix);
                div()
                    .id(("claude-code-question-tab", question_ix))
                    .debug_selector(move || format!("claude-code-question-tab-{question_ix}"))
                    .flex_shrink_1()
                    .min_w_0()
                    .max_w(TAB_MAX_WIDTH)
                    .px_0p5()
                    .py_1()
                    .border_b_2()
                    .border_color(if is_active {
                        accent
                    } else {
                        gpui::transparent_black()
                    })
                    .cursor_pointer()
                    .hover(|style| style.opacity(0.8))
                    .child(
                        Label::new(tab_question.header.clone().unwrap_or_default())
                            .size(LabelSize::Small)
                            .weight(if is_answered {
                                FontWeight::NORMAL
                            } else {
                                FontWeight::MEDIUM
                            })
                            .color(if is_answered {
                                Color::Muted
                            } else {
                                Color::Default
                            })
                            .single_line()
                            .truncate(),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show_claude_code_question(question_ix, true, window, cx);
                    }))
            })
            .collect::<Vec<_>>();

        let navigation = h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap(px(10.))
            .when(!folded, |this| this.mb_2())
            .children(tabs)
            .child(div().flex_1())
            .child(
                IconButton::new(
                    "claude-code-question-fold",
                    if folded {
                        IconName::ChevronUp
                    } else {
                        IconName::ChevronDown
                    },
                )
                .icon_size(IconSize::Small)
                .tooltip(Tooltip::text(if folded { "Expand" } else { "Collapse" }))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_claude_code_question_fold(window, cx);
                })),
            )
            .child(
                IconButton::new("claude-code-question-close", IconName::Close)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Close"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.cancel_claude_code_questions(window, cx);
                    })),
            );

        let has_previews = question.has_previews();
        let highlighted_row = card.highlighted_row.min(question.row_count() - 1);
        let rows = (0..question.row_count())
            .map(|row| {
                let is_other = row == question.other_row();
                let option = question.options.get(row);
                let label = option
                    .map(|option| option.label.clone())
                    .unwrap_or_else(|| OTHER_LABEL.into());
                let selected = card.is_selected(active_question, row);
                let other_editor = is_other
                    .then(|| card.other_editors.get(active_question).cloned().flatten())
                    .flatten()
                    .filter(|_| selected);
                let is_highlighted = row == highlighted_row && (is_focused || has_previews);
                option_row_body(
                    OptionRowContent {
                        multi_select: question.multi_select,
                        label,
                        description: option.and_then(|option| option.description.clone()),
                        selected,
                    },
                    cx,
                )
                .id(("claude-code-question-option", row))
                .debug_selector(move || format!("claude-code-question-option-{row}"))
                .cursor_pointer()
                .hover(|style| style.bg(colors.ghost_element_hover))
                .when(is_highlighted, |this| this.bg(colors.ghost_element_hover))
                .when(is_highlighted && has_previews, |this| {
                    this.border_l_2().border_color(accent)
                })
                .when_some(other_editor, |this, editor| {
                    this.child(
                        div()
                            .w_full()
                            .mt_1p5()
                            .px_2()
                            .py_1p5()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.editor_background)
                            .text_sm()
                            // The extension's field keeps Escape to itself, and
                            // Tab moves focus out of it like a browser textarea.
                            .on_action(|_: &editor::actions::Cancel, _, _| {})
                            .capture_action(cx.listener(
                                |this, _: &editor::actions::Tab, window, cx| {
                                    cx.stop_propagation();
                                    this.focus_claude_code_question_card(window, cx);
                                },
                            ))
                            .capture_action(cx.listener(
                                |this, _: &editor::actions::Backtab, window, cx| {
                                    cx.stop_propagation();
                                    this.focus_claude_code_question_card(window, cx);
                                },
                            ))
                            .child(editor),
                    )
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.choose_claude_code_question_row(row, window, cx);
                }))
                .when(has_previews, |this| {
                    this.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.highlight_claude_code_question_row(row, cx);
                        }
                    }))
                })
                .into_any_element()
            })
            .collect::<Vec<_>>();

        let questions = v_flex()
            .id("claude-code-question-body")
            .debug_selector(|| "claude-code-question-body".into())
            .w_full()
            .min_w_0()
            .max_h(viewport_height * 0.4)
            .overflow_y_scroll()
            .track_scroll(&card.scroll_handle)
            .gap_0p5()
            .child(
                div()
                    .debug_selector(|| "claude-code-question-text".into())
                    .mb_1p5()
                    .font_weight(FontWeight::MEDIUM)
                    .child(question.question.clone()),
            )
            .children(rows);

        let preview = has_previews.then(|| {
            let highlighted_option = question.options.get(highlighted_row);
            let caption = highlighted_option
                .map(|option| option.label.clone())
                .unwrap_or_else(|| OTHER_LABEL.into());
            let preview = highlighted_option.and_then(|option| option.preview.clone());
            let line_count = question
                .options
                .iter()
                .filter_map(|option| option.preview.as_deref())
                .map(|preview| preview_lines(preview).len() + 1)
                .max()
                .unwrap_or_default()
                .max(3);
            let rem = window.rem_size();
            let min_height = (rem * (line_count as f32 * 1.3 + 3.)).min(viewport_height * 0.4);
            v_flex()
                .id("claude-code-question-preview")
                .debug_selector(|| "claude-code-question-preview".into())
                .min_w_0()
                .min_h(min_height)
                .max_h(viewport_height * 0.4)
                .overflow_scroll()
                .px_2()
                .pb_2()
                .rounded(px(4.))
                .border_1()
                .border_color(colors.border_variant)
                .bg(colors.editor_background)
                .child(
                    div().py_1().child(
                        Label::new(caption)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
                .child(match preview {
                    Some(preview) => v_flex()
                        .font_buffer(cx)
                        .text_xs()
                        .text_color(colors.text)
                        .children(preview_lines(&preview).into_iter().map(|line| {
                            let line = if line.is_empty() {
                                " ".to_string()
                            } else {
                                line
                            };
                            div().whitespace_nowrap().child(line)
                        }))
                        .into_any_element(),
                    None => Label::new(NO_PREVIEW_LABEL)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .italic()
                        .into_any_element(),
                })
        });

        let body = match preview {
            Some(preview) if width.get() >= SIDE_BY_SIDE_PREVIEW_WIDTH => h_flex()
                .w_full()
                .min_w_0()
                .items_start()
                .gap_3()
                .child(div().w(relative(0.4)).min_w_0().child(questions))
                .child(div().flex_1().min_w_0().child(preview))
                .into_any_element(),
            Some(preview) => v_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .child(questions)
                .child(preview)
                .into_any_element(),
            None => questions.into_any_element(),
        };

        let submit = h_flex()
            .id("claude-code-question-submit")
            .debug_selector(|| "claude-code-question-submit".into())
            .w_full()
            .mt_2()
            .px_2()
            .py_1p5()
            .gap_1()
            .rounded(px(4.))
            .border_1()
            .border_color(colors.border_variant)
            .child(Label::new("1").color(Color::Muted))
            .child(Label::new(SUBMIT_LABEL).weight(FontWeight::MEDIUM))
            .map(|this| {
                if all_answered {
                    this.cursor_pointer()
                        .hover(|style| style.bg(colors.ghost_element_hover))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.submit_claude_code_questions(window, cx);
                        }))
                } else {
                    this.opacity(0.5).cursor_not_allowed()
                }
            });

        let measure_width = canvas(
            move |bounds, _, cx| {
                if width.get() != bounds.size.width {
                    width.set(bounds.size.width);
                    // Lay the preview out for the new width once this frame is drawn.
                    cx.defer(move |cx| cx.notify(thread_view_id));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();

        v_flex()
            .id("claude-code-question-card")
            .debug_selector(|| "claude-code-question-card".into())
            .key_context("ClaudeCodeQuestionPrompt")
            .track_focus(focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                if this.handle_claude_code_question_key(&event.keystroke.key, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &menu::Cancel, window, cx| {
                // Escape while typing an "Other" answer does nothing, as in the
                // extension, rather than cancelling the questions.
                if !this.claude_code_other_editor_focused(window, cx) {
                    this.cancel_claude_code_questions(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &menu::SelectNext, window, cx| {
                this.move_claude_code_question_highlight(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &menu::SelectPrevious, window, cx| {
                this.move_claude_code_question_highlight(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &menu::Confirm, window, cx| {
                this.confirm_claude_code_question(window, cx);
            }))
            .relative()
            .w_full()
            .min_w_0()
            .max_h(viewport_height * 0.7)
            .p_2()
            .rounded(PROMPT_BOX_RADIUS)
            .border_1()
            .border_color(if is_focused {
                colors.border_focused
            } else {
                colors.border
            })
            .bg(colors.editor_background)
            .block_mouse_except_scroll()
            .text_ui(cx)
            .child(navigation)
            .when(!folded, |this| {
                this.child(body).child(submit).child(
                    div().mt_2().child(
                        Label::new(CANCEL_HINT)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
            })
            .child(measure_width)
            .into_any_element()
    }

    /// Keys the card reads itself, as the extension's card does: Left and Right
    /// switch questions, Space chooses the highlighted row and `1` submits once
    /// every question is answered. Returns whether the key was taken.
    fn handle_claude_code_question_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.claude_code_other_editor_focused(window, cx) {
            return false;
        }
        let Some(card) = question_card_state(cx.entity_id(), cx) else {
            return false;
        };
        if card.folded {
            return false;
        }
        let active_question = card.active_question;
        let highlighted_row = card.highlighted_row;
        match key {
            "left" => {
                if let Some(previous) = active_question.checked_sub(1) {
                    self.show_claude_code_question(previous, false, window, cx);
                }
                true
            }
            "right" => {
                self.show_claude_code_question(active_question + 1, false, window, cx);
                true
            }
            "space" => {
                self.choose_claude_code_question_row(highlighted_row, window, cx);
                true
            }
            "1" => {
                self.submit_claude_code_questions(window, cx);
                true
            }
            _ => false,
        }
    }

    fn focus_claude_code_question_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(focus) = thread_activity(cx.entity_id(), cx)
            .and_then(|activity| activity.permission_focus())
            .cloned()
        {
            window.focus(&focus, cx);
        }
    }

    fn show_claude_code_question(
        &mut self,
        question_ix: usize,
        unfold: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let other_editor_focused = self.claude_code_other_editor_focused(window, cx);
        let Some(card) = question_card_state_mut(cx.entity_id(), cx) else {
            return;
        };
        card.show_question(question_ix);
        if unfold {
            card.folded = false;
        }
        if other_editor_focused {
            self.focus_claude_code_question_card(window, cx);
        }
        cx.notify();
    }

    fn toggle_claude_code_question_fold(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(card) = question_card_state_mut(cx.entity_id(), cx) else {
            return;
        };
        card.folded = !card.folded;
        self.focus_claude_code_question_card(window, cx);
        cx.notify();
    }

    fn highlight_claude_code_question_row(&mut self, row: usize, cx: &mut Context<Self>) {
        if let Some(card) = question_card_state_mut(cx.entity_id(), cx)
            && card.highlighted_row != row
        {
            card.highlighted_row = row;
            cx.notify();
        }
    }

    /// Up and Down (and Tab) move through the options and "Other", wrapping
    /// around, as the extension's arrow keys do.
    fn move_claude_code_question_highlight(
        &mut self,
        offset: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = self.claude_code_question_form(cx) else {
            cx.propagate();
            return;
        };
        let other_editor_focused = self.claude_code_other_editor_focused(window, cx);
        let Some(card) = question_card_state_mut(cx.entity_id(), cx) else {
            cx.propagate();
            return;
        };
        if card.folded {
            cx.propagate();
            return;
        }
        let Some(question) = form.questions.get(card.active_question) else {
            return;
        };
        card.move_highlight(question.row_count(), offset);
        // The question text comes before the rows in the scrolled list.
        card.scroll_handle.scroll_to_item(card.highlighted_row + 1);
        if other_editor_focused {
            self.focus_claude_code_question_card(window, cx);
        }
        cx.notify();
    }

    /// Enter submits once every question is answered (the extension's card
    /// claims Enter for its Submit button); until then it chooses the
    /// highlighted row. In the "Other" field it moves on to the next question.
    fn confirm_claude_code_question(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(card) = question_card_state(cx.entity_id(), cx) else {
            cx.propagate();
            return;
        };
        if card.folded {
            cx.propagate();
            return;
        }
        let active_question = card.active_question;
        let highlighted_row = card.highlighted_row;
        let all_answered = card.all_answered();
        if self.claude_code_other_editor_focused(window, cx) {
            // Like the extension, Enter in "Other" moves on to the next question
            // and does nothing on the last one rather than submitting.
            let question_count = self
                .claude_code_question_form(cx)
                .map_or(0, |form| form.questions.len());
            if active_question + 1 < question_count {
                self.show_claude_code_question(active_question + 1, false, window, cx);
                self.focus_claude_code_question_card(window, cx);
            }
        } else if all_answered {
            self.submit_claude_code_questions(window, cx);
        } else {
            self.choose_claude_code_question_row(highlighted_row, window, cx);
        }
    }

    fn choose_claude_code_question_row(
        &mut self,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = self.claude_code_question_form(cx) else {
            return;
        };
        let thread_view_id = cx.entity_id();
        let Some(card) = question_card_state_mut(thread_view_id, cx) else {
            return;
        };
        let question_ix = card.active_question;
        let Some(question) = form.questions.get(question_ix) else {
            return;
        };
        if row >= question.row_count() {
            return;
        }
        card.highlighted_row = row;
        let choice = card.choose(question, question_ix, row);
        let elicitation_id = card.elicitation_id.clone();
        match choice {
            RowChoice::Stay => {}
            RowChoice::EditOther => {
                let existing = card.other_editors.get(question_ix).cloned().flatten();
                let editor = match existing {
                    Some(editor) => editor,
                    None => {
                        let editor = cx.new(|cx| {
                            // Multi-line so Shift+Enter adds a newline, as in the
                            // extension; plain Enter isn't bound in auto-height
                            // editors, so it still reaches the card's Confirm.
                            let mut editor = Editor::auto_height(1, OTHER_MAX_LINES, window, cx);
                            editor.set_placeholder_text(OTHER_PLACEHOLDER, window, cx);
                            editor
                        });
                        if let Some(slot) = question_card_state_mut(thread_view_id, cx)
                            .and_then(|card| card.other_editors.get_mut(question_ix))
                        {
                            *slot = Some(editor.clone());
                        }
                        editor
                    }
                };
                window.focus(&editor.focus_handle(cx), cx);
            }
            RowChoice::Advance => {
                let task = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(ADVANCE_DELAY).await;
                    this.update(cx, |_, cx| {
                        if let Some(card) = question_card_state_mut(thread_view_id, cx)
                            && card.elicitation_id == elicitation_id
                            && card.active_question == question_ix
                            && card.confirming_row.is_some()
                        {
                            card.show_question(question_ix + 1);
                            cx.notify();
                        }
                    })
                    .log_err();
                });
                if let Some(card) = question_card_state_mut(thread_view_id, cx) {
                    card.advance_task = Some(task);
                }
            }
        }
        cx.notify();
    }

    fn submit_claude_code_questions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.claude_code_question_form(cx) else {
            return;
        };
        let thread_view_id = cx.entity_id();
        let Some(card) = question_card_state(thread_view_id, cx) else {
            return;
        };
        if card.elicitation_id != form.elicitation_id || !card.all_answered() {
            return;
        }
        let other_texts = card.other_texts(cx);
        let content = response_content(
            &form.questions,
            &form.fields,
            &card.selections,
            &other_texts,
        );
        let answers = card.answers(&form.questions, cx);
        if let Some(tool_call_id) = form.tool_call_id.clone() {
            set_question_outcome(
                thread_view_id,
                tool_call_id,
                QuestionOutcome::Answered(answers),
                cx,
            );
        }
        self.finish_claude_code_questions(
            &form,
            acp_v2::CreateElicitationResponse::new(acp_v2::ElicitationAction::Accept(
                acp_v2::ElicitationAcceptAction::new().content(content),
            )),
            window,
            cx,
        );
    }

    /// Esc and × decline and interrupt the turn, as the extension's do (its
    /// card denies the tool use with `interrupt`). ACP has no such answer, so the
    /// request is cancelled and the turn stopped like the stop button does.
    fn cancel_claude_code_questions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.claude_code_question_form(cx) else {
            return;
        };
        if let Some(tool_call_id) = form.tool_call_id.clone() {
            set_question_outcome(cx.entity_id(), tool_call_id, QuestionOutcome::Declined, cx);
        }
        self.finish_claude_code_questions(
            &form,
            acp_v2::CreateElicitationResponse::new(acp_v2::ElicitationAction::Cancel),
            window,
            cx,
        );
        self.cancel_generation(cx);
    }

    fn finish_claude_code_questions(
        &mut self,
        form: &QuestionForm,
        response: acp_v2::CreateElicitationResponse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let card_focused = thread_activity(cx.entity_id(), cx)
            .and_then(|activity| activity.permission_focus())
            .is_some_and(|focus| focus.contains_focused(window, cx))
            || self.claude_code_other_editor_focused(window, cx);
        sync_question_card(cx.entity_id(), None, 0, cx);
        if form.session_id == self.session_id {
            self.respond_to_elicitation(form.elicitation_id.clone(), response, cx);
        } else {
            let session_id = form.session_id.clone();
            let elicitation_id = form.elicitation_id.clone();
            self.conversation.update(cx, |conversation, cx| {
                conversation.respond_to_elicitation(session_id, elicitation_id, response, cx);
            });
        }
        if card_focused {
            let editor_focus = self.message_editor.focus_handle(cx);
            window.focus(&editor_focus, cx);
        }
        cx.notify();
    }

    /// Claude Code's row for an AskUserQuestion call: "Question" or "Questions",
    /// and once it has a result, a status line ("Answered · 2 questions",
    /// "Declined · 4 questions", ...) that shows or hides the questions with the
    /// chosen options and typed answers. An interrupted call is followed by
    /// "Tool interrupted".
    pub(in crate::conversation_view::thread_view) fn render_claude_code_question_row(
        &self,
        entry_ix: usize,
        tool_call: &ToolCall,
        cx: &Context<Self>,
    ) -> Option<Stateful<Div>> {
        if reported_claude_code_tool(tool_call)?.as_ref() != ASK_USER_QUESTION_TOOL {
            return None;
        }
        let thread_view_id = cx.entity_id();
        let questions = questions_from_input(tool_call.raw_input.as_ref());
        let outcome = question_outcome(thread_view_id, &tool_call.id, cx);
        let tool_status = tool_call.status();
        let result = question_result_text(tool_call).or_else(|| {
            matches!(
                tool_status,
                ToolCallStatus::Completed | ToolCallStatus::Failed
            )
            .then(|| tool_output_text(tool_call, None, cx))
            .flatten()
            .map(|text| text.to_string())
        });
        let state =
            question_row_state(&questions, tool_status, result.as_deref(), outcome.as_ref());

        let name = if questions.len() == 1 {
            "Question"
        } else {
            "Questions"
        };
        let step_status = match state.status {
            QuestionStatus::Pending => StepStatus::Muted,
            QuestionStatus::Declined => StepStatus::Failed,
            _ => StepStatus::from_tool_call_status(tool_status),
        };
        let mut row = ToolRow::new(("claude-code-tool", entry_ix), name, step_status);

        if let Some(status_label) = state.status.label() {
            let collapsed = question_row_collapsed(thread_view_id, &tool_call.id, cx);
            let count = questions.len();
            let status_text = if count > 0 {
                format!(
                    "{status_label} · {count} {}",
                    if count == 1 { "question" } else { "questions" }
                )
            } else {
                status_label.to_string()
            };
            let tool_call_id = tool_call.id.clone();
            let status_line = h_flex()
                .id(("claude-code-question-status", entry_ix))
                .debug_selector(move || format!("claude-code-question-status-{entry_ix}"))
                .gap_0p5()
                .opacity(0.7)
                .text_sm()
                .text_color(cx.theme().colors().text_muted)
                .cursor_pointer()
                .hover(|style| style.underline())
                .child(status_text)
                .child(
                    Icon::new(if collapsed {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronUp
                    })
                    .size(IconSize::XSmall)
                    .color(Color::Muted),
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    toggle_question_row(cx.entity_id(), &tool_call_id, cx);
                    cx.notify();
                }));

            // An answer that can't be read back shows the result instead.
            let review = !(state.status == QuestionStatus::Answered && state.answers.is_none())
                && !questions.is_empty();
            let review = review.then(|| {
                self.render_claude_code_question_review(entry_ix, &questions, &state.answers, cx)
            });
            let output = state
                .shows_output
                .then(|| result.clone())
                .flatten()
                .map(|result| Self::render_claude_code_question_output(entry_ix, &result, cx));
            let has_block = review.is_some() || output.is_some();
            let has_both = review.is_some() && output.is_some();
            row = row.body(v_flex().w_full().gap_1().child(status_line).when(
                !collapsed && has_block,
                |this| {
                    this.child(
                        v_flex()
                            .w_full()
                            .overflow_hidden()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(cx.theme().colors().border_variant)
                            .bg(cx.theme().colors().editor_background)
                            .children(review)
                            .children(output.map(|output| {
                                output
                                    .when(has_both, |this| this.border_t_1())
                                    .border_color(cx.theme().colors().border_variant)
                            })),
                    )
                },
            ));
        } else if state.status == QuestionStatus::Other {
            row = row.output(result.map(SharedString::from));
        }

        let interrupted = state.interrupted.then(|| {
            div()
                .w_full()
                .pt_2()
                .debug_selector(move || format!("claude-code-question-interrupted-{entry_ix}"))
                .child(
                    Label::new(TOOL_INTERRUPTED_LABEL)
                        .italic()
                        .color(Color::Muted),
                )
        });

        Some(
            div()
                .id(("claude-code-tool-entry", entry_ix))
                .debug_selector(move || format!("claude-code-tool-{entry_ix}"))
                .w_full()
                .px_5()
                .py_1()
                .text_ui(cx)
                .child(row)
                .children(interrupted),
        )
    }

    /// The questions of an answered or declined call, read-only: the chosen
    /// options marked and the rest dimmed, and a typed answer under "Other".
    fn render_claude_code_question_review(
        &self,
        entry_ix: usize,
        questions: &[Question],
        answers: &Option<Vec<(SharedString, String)>>,
        cx: &Context<Self>,
    ) -> Div {
        v_flex()
            .w_full()
            .p_2()
            .gap_4()
            .children(questions.iter().enumerate().map(|(question_ix, question)| {
                let reviewed = answers
                    .as_ref()
                    .and_then(|answers| {
                        answers
                            .iter()
                            .find(|(text, _)| *text == question.question)
                            .map(|(_, answer)| review_answer(question, answer))
                    })
                    .unwrap_or_default();
                let is_answered = reviewed.is_answered();
                let option_rows = question.options.iter().enumerate().map(|(row, option)| {
                    let selected = reviewed.picked.iter().any(|label| *label == option.label);
                    option_row_body(
                        OptionRowContent {
                            multi_select: question.multi_select,
                            label: option.label.clone(),
                            description: option.description.clone(),
                            selected,
                        },
                        cx,
                    )
                    .when(is_answered && !selected, |this| this.opacity(0.6))
                    .when(selected, |this| {
                        this.debug_selector(move || {
                            format!("claude-code-question-answer-{entry_ix}-{question_ix}-{row}")
                        })
                    })
                });
                let other_row = reviewed.shows_other().then(|| {
                    let typed = reviewed.typed.clone();
                    option_row_body(
                        OptionRowContent {
                            multi_select: question.multi_select,
                            label: OTHER_LABEL.into(),
                            description: None,
                            selected: true,
                        },
                        cx,
                    )
                    .debug_selector(move || {
                        format!("claude-code-question-other-{entry_ix}-{question_ix}")
                    })
                    .when_some(typed, |this, typed| {
                        this.child(
                            div()
                                .w_full()
                                .pl(MARKER_SIZE + px(10.))
                                .debug_selector(move || {
                                    format!("claude-code-question-typed-{entry_ix}-{question_ix}")
                                })
                                .child(
                                    Label::new(typed).size(LabelSize::Small).color(Color::Muted),
                                ),
                        )
                    })
                });
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(question.question.clone()),
                    )
                    .child(
                        v_flex()
                            .w_full()
                            .gap_0p5()
                            .children(option_rows)
                            .children(other_row),
                    )
            }))
    }

    fn render_claude_code_question_output(entry_ix: usize, result: &str, cx: &App) -> Div {
        h_flex()
            .debug_selector(move || format!("claude-code-tool-{entry_ix}-output"))
            .w_full()
            .items_start()
            .px_2p5()
            .py_1p5()
            .child(
                div().flex_none().w(px(36.)).child(
                    Label::new("OUT")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .font_buffer(cx)
                    .text_xs()
                    .text_color(cx.theme().colors().text)
                    .children(display_lines(result).into_iter().map(|line| {
                        let line = if line.is_empty() {
                            " ".to_string()
                        } else {
                            line
                        };
                        div().w_full().min_w_0().child(line)
                    })),
            )
    }
}
