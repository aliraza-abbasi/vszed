// This file is declared as a child module of `thread_view` (via a `#[path]`
// attribute) rather than of `ui_styles::claude_code`, so that it can reuse
// `ThreadView`'s private state and render helpers without widening their
// visibility upstream.

#[path = "permission_card.rs"]
mod claude_code_permission_card;
#[path = "prompt_box.rs"]
mod claude_code_prompt_box;

use super::*;
use crate::ui_styles::claude_code::{
    ClaudeCodeSpinnerElement, StepStatus, TIMELINE_GUTTER, TimelineDot, ToolRow, edit_line_stats,
    format_timestamp, summarize_tool_call, thought_label, thread_activity, timeline_row,
    tool_output_text, ui_font_text_style,
};
use claude_code_permission_card::claude_code_card_handles;
use editor::{EditorElement, EditorStyle};

impl ThreadView {
    pub(super) fn render_claude_code_entries(&mut self, cx: &mut Context<Self>) -> List {
        let max_content_width = AgentSettings::get_global(cx).max_content_width;
        let centered_container = move |content: AnyElement| {
            h_flex().w_full().justify_center().child(
                div()
                    .when_some(max_content_width, |this, max_width| this.max_w(max_width))
                    .w_full()
                    .child(content),
            )
        };

        list(
            self.list_state.clone(),
            cx.processor(move |this, index: usize, window, cx| {
                let entries = this.thread.read(cx).entries();
                if let Some(entry) = entries.get(index) {
                    let rendered =
                        this.render_claude_code_entry(index, entries.len(), entry, window, cx);
                    centered_container(rendered).into_any_element()
                } else if this.generating_indicator_in_list {
                    centered_container(this.render_claude_code_generating(cx)).into_any_element()
                } else {
                    Empty.into_any()
                }
            }),
        )
        .with_sizing_behavior(gpui::ListSizingBehavior::Auto)
        .flex_grow_1()
    }

    fn render_claude_code_entry(
        &self,
        entry_ix: usize,
        total_entries: usize,
        entry: &AgentThreadEntry,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let is_thread_bottom = entry_ix + 1 == total_entries;
        let entry_focus_handle = self
            .entry_view_state
            .read(cx)
            .entry(entry_ix)
            .and_then(|entry| entry.focus_handle(cx));
        let mut assistant_message_is_blank = false;

        let primary = match entry {
            AgentThreadEntry::UserMessage(message) => {
                if message.content.source_blocks().is_empty() {
                    return Empty.into_any_element();
                }
                let Some(user_message) =
                    self.render_claude_code_user_message(entry_ix, message, window, cx)
                else {
                    return Empty.into_any_element();
                };
                user_message
            }
            AgentThreadEntry::AssistantMessage(message) => {
                match self.render_claude_code_assistant_message(entry_ix, message, window, cx) {
                    Some(rows) => rows
                        .when_some(entry_focus_handle, |this, handle| this.track_focus(&handle))
                        .into_any_element(),
                    None => {
                        assistant_message_is_blank = true;
                        Empty.into_any()
                    }
                }
            }
            AgentThreadEntry::ToolCall(tool_call) => {
                if Self::claude_code_tool_call_is_hidden(tool_call, cx) {
                    return Empty.into_any_element();
                }
                self.render_claude_code_tool_call(entry_ix, tool_call, window, cx)
                    .when_some(entry_focus_handle, |this, handle| this.track_focus(&handle))
                    .into_any_element()
            }
            AgentThreadEntry::Elicitation(elicitation_id) => {
                let thread = self.thread.read(cx);
                match thread.elicitation(elicitation_id) {
                    Some((_, elicitation)) if should_render_elicitation(elicitation) => self
                        .render_elicitation(entry_ix, elicitation, window, cx)
                        .when_some(entry_focus_handle, |this, handle| this.track_focus(&handle))
                        .into_any_element(),
                    _ => Empty.into_any(),
                }
            }
            AgentThreadEntry::ContextCompaction(compaction) => {
                self.render_claude_code_compaction(entry_ix, compaction, window, cx)
            }
            AgentThreadEntry::SessionMessage(message) => {
                let session_message =
                    match acp_subagent_card(self.thread.read(cx), entry_ix, message, cx) {
                        Some(card) => self.render_acp_subagent_card(entry_ix, &card, window, cx),
                        None => self.render_session_message(entry_ix, message, window, cx),
                    };
                session_message
                    .when_some(entry_focus_handle, |this, handle| this.track_focus(&handle))
                    .into_any_element()
            }
        };

        let thread = self.thread.clone();
        let is_generating = matches!(thread.read(cx).status(), ThreadStatus::Generating);
        let is_turn_end = Self::entry_is_finalized_turn_end(thread.read(cx).entries(), entry_ix)
            .unwrap_or(!is_generating);

        let primary = if is_turn_end && !assistant_message_is_blank {
            let hover_group = SharedString::from(format!("claude-code-turn-{entry_ix}"));
            v_flex()
                .w_full()
                .group(hover_group.clone())
                .child(primary)
                .children(self.render_claude_code_turn_controls(
                    entry_ix,
                    is_thread_bottom,
                    hover_group,
                    cx,
                ))
                .into_any_element()
        } else {
            primary
        };

        let primary = if is_thread_bottom {
            v_flex()
                .w_full()
                .child(primary)
                .when_some(
                    self.thread_feedback.comments_editor.clone(),
                    |this, editor| this.child(Self::render_feedback_feedback_editor(editor, cx)),
                )
                .into_any_element()
        } else {
            primary
        };

        match self.editing_message {
            Some(editing_index) if editing_index < entry_ix => {
                let backdrop = div()
                    .id(("claude-code-backdrop", entry_ix))
                    .size_full()
                    .absolute()
                    .inset_0()
                    .bg(cx.theme().colors().panel_background)
                    .opacity(0.8)
                    .block_mouse_except_scroll()
                    .on_click(cx.listener(Self::cancel_editing));
                div()
                    .relative()
                    .child(primary)
                    .when(!self.is_subagent(), |this| this.child(backdrop))
                    .into_any_element()
            }
            _ => primary,
        }
    }

    /// Claude Code's only control under a response is **Copy response**, shown
    /// while the response is hovered. Zed's other turn controls (feedback,
    /// scroll to the prompt or to the top, turn stats) have no counterpart there.
    fn render_claude_code_turn_controls(
        &self,
        entry_ix: usize,
        is_thread_bottom: bool,
        hover_group: SharedString,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let thread = self.thread.read(cx);
        let is_generating = matches!(thread.status(), ThreadStatus::Generating);
        let needs_confirmation =
            thread.is_waiting_for_confirmation() || self.has_pending_request_elicitation(cx);
        if is_thread_bottom && (is_generating || needs_confirmation) {
            return None;
        }
        let response_index = Self::agent_message_copy_index(thread.entries(), entry_ix, cx)?;
        let thread = self.thread.clone();
        Some(
            h_flex()
                .w_full()
                .px_5()
                .pb_1()
                .justify_end()
                .debug_selector(move || format!("claude-code-turn-controls-{entry_ix}"))
                .child(
                    CopyButton::new(("claude-code-copy-response", entry_ix), "")
                        .icon_size(IconSize::Small)
                        .tooltip_label("Copy response")
                        .visible_on_hover(hover_group)
                        .custom_on_click(move |_, cx| {
                            let entries = thread.read(cx).entries();
                            if let Some(text) =
                                Self::get_agent_message_content(entries, response_index, cx)
                            {
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                            }
                        }),
                )
                .into_any_element(),
        )
    }

    /// A canceled tool call that never produced output only adds noise.
    fn claude_code_tool_call_is_hidden(tool_call: &ToolCall, cx: &App) -> bool {
        if !matches!(tool_call.status(), ToolCallStatus::Canceled) {
            return false;
        }
        !tool_call.content().iter().any(|content| match content {
            ToolCallContent::ContentBlock { block, .. } => block.visible_content(cx),
            ToolCallContent::Diff(_)
            | ToolCallContent::LegacyDiff { .. }
            | ToolCallContent::Terminal { .. }
            | ToolCallContent::SessionReference(_) => true,
            ToolCallContent::DiffPatch { render, .. } => {
                !render.files.is_empty()
                    || render
                        .fallback
                        .as_ref()
                        .is_some_and(|markdown| !markdown.read(cx).source().is_empty())
            }
            ToolCallContent::Other { markdown, .. } => !markdown.read(cx).source().is_empty(),
        })
    }

    fn render_claude_code_user_message(
        &self,
        entry_ix: usize,
        message: &acp_thread::UserMessage,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let editor = self
            .entry_view_state
            .read(cx)
            .entry(entry_ix)
            .and_then(|entry| entry.message_editor())
            .cloned()?;

        let editing = self.editing_message == Some(entry_ix);
        let editor_focused = editor.focus_handle(cx).is_focused(window);
        let can_rewind = self
            .thread
            .read(cx)
            .can_rewind_to(message.client_id.as_ref(), cx);
        let source_is_representable = message
            .content
            .source_blocks()
            .iter()
            .all(acp_thread::content::can_convert_to_v1);
        let is_editable = can_rewind && source_is_representable;
        let sent_at = thread_activity(cx.entity_id(), cx)
            .and_then(|activity| activity.user_message_sent_at(entry_ix));

        let colors = cx.theme().colors();
        let focus_border = colors.border_focused;
        let border_color = if editing && editor_focused {
            focus_border
        } else {
            colors.border_variant
        };

        let edit_controls = (editor_focused && is_editable).then(|| {
            let is_loading_contents = self.is_loading_contents;
            h_flex()
                .absolute()
                .top_neg_3p5()
                .right_3()
                .gap_1()
                .rounded_sm()
                .border_1()
                .border_color(colors.border)
                .bg(colors.editor_background)
                .overflow_hidden()
                .child(
                    IconButton::new("cancel", IconName::Close)
                        .disabled(is_loading_contents)
                        .icon_color(Color::Error)
                        .icon_size(IconSize::XSmall)
                        .on_click(cx.listener(Self::cancel_editing)),
                )
                .child(
                    IconButton::new("regenerate", IconName::Return)
                        .disabled(is_loading_contents)
                        .icon_color(Color::Muted)
                        .icon_size(IconSize::XSmall)
                        .tooltip(Tooltip::text(
                            "Editing will restart the thread from this point.",
                        ))
                        .on_click(cx.listener({
                            let editor = editor.clone();
                            move |this, _, window, cx| {
                                this.regenerate(entry_ix, editor.clone(), window, cx);
                            }
                        })),
                )
        });

        Some(
            v_flex()
                .id(("claude-code-user-message", entry_ix))
                .debug_selector(move || format!("claude-code-user-message-{entry_ix}"))
                .w_full()
                .px_5()
                .pt_4()
                .pb_2()
                .gap_1()
                .when_some(sent_at, |this, sent_at| {
                    this.child(
                        h_flex().w_full().justify_end().child(
                            Label::new(format_timestamp(sent_at))
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        ),
                    )
                })
                .child(
                    div()
                        .relative()
                        .w_full()
                        .child(
                            div()
                                .w_full()
                                .p(px(10.))
                                .rounded(px(6.))
                                .border_1()
                                .border_color(border_color)
                                .when(editing && !editor_focused, |this| this.border_dashed())
                                .bg(colors.elevated_surface_background)
                                .when(is_editable && !editing, |this| {
                                    this.hover(|style| {
                                        style.border_color(focus_border.opacity(0.8))
                                    })
                                })
                                .child(if editing {
                                    editor.into_any_element()
                                } else {
                                    Self::render_claude_code_sent_message(&editor, cx)
                                }),
                        )
                        .children(edit_controls),
                )
                .into_any_element(),
        )
    }

    /// A sent message reads as plain UI text directly inside its box, like
    /// Claude Code. The message's own editor still renders it, so mention
    /// creases stay, and clicking it focuses the editor to start editing,
    /// which switches to the full `MessageEditor`.
    fn render_claude_code_sent_message(
        message_editor: &Entity<MessageEditor>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let editor = message_editor.read(cx).editor().clone();
        let text_style = ui_font_text_style(cx);
        div()
            .w_full()
            .child(EditorElement::new(
                &editor,
                EditorStyle {
                    background: gpui::transparent_black(),
                    local_player: cx.theme().players().local(),
                    text: text_style,
                    syntax: cx.theme().syntax().clone(),
                    inlay_hints_style: editor::make_inlay_hints_style(cx),
                    ..Default::default()
                },
            ))
            .into_any_element()
    }

    fn render_claude_code_assistant_message(
        &self,
        entry_ix: usize,
        message: &AssistantMessage,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<Div> {
        let thread = self.thread.read(cx);
        let is_streaming_entry = matches!(thread.status(), ThreadStatus::Generating)
            && entry_ix + 1 == thread.entries().len();
        let last_chunk_ix = message.chunks.len().checked_sub(1);
        let activity = thread_activity(cx.entity_id(), cx);

        let rows = message
            .chunks
            .iter()
            .enumerate()
            .filter_map(|(chunk_ix, chunk)| match chunk {
                AssistantMessageChunk::Message { block, .. } => {
                    block.visible_content(cx).then(|| {
                        timeline_row(
                            TimelineDot::new(
                                ("claude-code-message-dot", chunk_ix),
                                StepStatus::Neutral,
                            ),
                            self.render_message_content(entry_ix, chunk_ix, block, window, cx),
                            window,
                        )
                        .into_any_element()
                    })
                }
                AssistantMessageChunk::Thought { block, .. } => {
                    block.visible_content(cx).then(|| {
                        let is_streaming = is_streaming_entry && Some(chunk_ix) == last_chunk_ix;
                        let duration = activity
                            .and_then(|activity| activity.thought_duration(entry_ix, chunk_ix));
                        self.render_claude_code_thought(
                            entry_ix,
                            chunk_ix,
                            block,
                            thought_label(duration, is_streaming),
                            window,
                            cx,
                        )
                    })
                }
            })
            .collect::<Vec<_>>();

        if rows.is_empty() {
            return None;
        }
        Some(
            v_flex()
                .w_full()
                .px_5()
                .py_1()
                .gap_2()
                .text_ui(cx)
                .children(rows),
        )
    }

    fn render_claude_code_thought(
        &self,
        entry_ix: usize,
        chunk_ix: usize,
        block: &acp_thread::MessageContent,
        label: SharedString,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let key = (entry_ix, chunk_ix);
        let entry_view_state = self.entry_view_state.read(cx);
        let (is_open, is_constrained) = entry_view_state.thinking_block_state(key, cx);
        let scroll_handle = entry_view_state
            .entry(entry_ix)
            .and_then(|entry| entry.scroll_handle_for_assistant_message_chunk(chunk_ix));
        if entry_view_state.is_auto_expanded_thinking_block(key)
            && let Some(scroll_handle) = scroll_handle.as_ref()
        {
            scroll_handle.scroll_to_bottom();
        }

        v_flex()
            .id(("claude-code-thought", chunk_ix))
            .w_full()
            .gap_1()
            .child(
                div()
                    .id(("claude-code-thought-header", chunk_ix))
                    .debug_selector(move || format!("claude-code-thought-{entry_ix}-{chunk_ix}"))
                    .cursor_pointer()
                    .child(timeline_row(
                        TimelineDot::new(("claude-code-thought-dot", chunk_ix), StepStatus::Muted),
                        Label::new(label).color(Color::Muted),
                        window,
                    ))
                    .on_click(cx.listener(move |this, _event: &ClickEvent, window, cx| {
                        this.toggle_thinking_block_expansion(key, window, cx);
                    })),
            )
            .when(is_open, |this| {
                this.child(
                    div().pl(TIMELINE_GUTTER).child(
                        div()
                            .id(("claude-code-thought-content", chunk_ix))
                            .when(is_constrained, |this| this.max_h_64())
                            .when_some(scroll_handle, |this, scroll_handle| {
                                this.track_scroll(&scroll_handle)
                            })
                            .overflow_hidden()
                            .child(
                                self.render_message_content(entry_ix, chunk_ix, block, window, cx),
                            ),
                    ),
                )
            })
            .into_any_element()
    }

    fn render_claude_code_tool_call(
        &self,
        entry_ix: usize,
        tool_call: &ToolCall,
        window: &Window,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let session_id = self.thread.read(cx).session_id().clone();
        let focus_handle = self.focus_handle(cx);

        if tool_call.is_subagent() {
            return self.render_any_tool_call(
                &session_id,
                entry_ix,
                tool_call,
                &focus_handle,
                ToolCallLayout::Standalone,
                window,
                cx,
            );
        }

        let label = tool_call.label.read(cx).source().to_string();
        let summary = summarize_tool_call(tool_call, &label);
        let status = tool_call.status();
        let has_failed = matches!(
            status,
            ToolCallStatus::Rejected | ToolCallStatus::Canceled | ToolCallStatus::Failed
        );
        let command_failed = tool_call.terminals().any(|terminal| {
            terminal.read(cx).output().is_some_and(|output| {
                output
                    .exit_status
                    .exit_code
                    .is_some_and(|exit_code| exit_code != 0)
                    || output.exit_status.signal.is_some()
            })
        });
        let step_status = if command_failed {
            StepStatus::Failed
        } else {
            StepStatus::from_tool_call_status(status)
        };

        let is_edit = matches!(tool_call.kind(), acp_v2::ToolKind::Edit)
            || tool_call.diffs().next().is_some()
            || tool_call
                .content()
                .iter()
                .any(|content| matches!(content, ToolCallContent::DiffPatch { .. }));
        let is_expanded = self
            .entry_view_state
            .read(cx)
            .is_tool_call_expanded(&tool_call.id);
        let toggle_expansion = || {
            let tool_call_id = tool_call.id.clone();
            cx.listener(move |this, _event: &ClickEvent, window, cx| {
                this.entry_view_state.update(cx, |state, _cx| {
                    state.toggle_tool_call_expansion(&tool_call_id);
                });
                this.materialize_tool_call_views(entry_ix, window, cx);
                this.refresh_thread_search(window, cx);
                cx.notify();
            })
        };

        let description = summary.description.clone().map(|description| {
            self.claude_code_project_relative_path(&description, cx)
                .unwrap_or(description)
        });
        let mut row = ToolRow::new(("claude-code-tool", entry_ix), summary.name, step_status)
            .description(description);

        if is_edit {
            if let Some((lines_added, lines_removed)) = edit_line_stats(tool_call) {
                row = row.header_suffix(
                    DiffStat::new(
                        ("claude-code-diff-stat", entry_ix),
                        lines_added,
                        lines_removed,
                    )
                    .label_size(LabelSize::XSmall),
                );
            }
            row = row.on_header_click(toggle_expansion());
            if is_expanded {
                let diff_contents = tool_call
                    .content()
                    .iter()
                    .enumerate()
                    .filter(|(_, content)| {
                        matches!(
                            content,
                            ToolCallContent::Diff(_)
                                | ToolCallContent::LegacyDiff { .. }
                                | ToolCallContent::DiffPatch { .. }
                        )
                    })
                    .map(|(content_ix, content)| {
                        self.render_tool_call_content(
                            &session_id,
                            entry_ix,
                            content,
                            content_ix,
                            tool_call,
                            true,
                            has_failed,
                            &focus_handle,
                            window,
                            cx,
                        )
                    })
                    .collect::<Vec<_>>();
                if !diff_contents.is_empty() {
                    row = row.body(
                        v_flex()
                            .w_full()
                            .overflow_hidden()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(cx.theme().colors().border_variant)
                            .children(diff_contents),
                    );
                }
            }
        } else {
            row = row
                .input(summary.input)
                .output(tool_output_text(
                    tool_call,
                    summary.description.as_deref(),
                    cx,
                ))
                .output_expanded(is_expanded)
                .on_toggle_output(toggle_expansion());

            let images = tool_call
                .content()
                .iter()
                .enumerate()
                .filter(|(_, content)| content.image().is_some())
                .map(|(content_ix, content)| {
                    self.render_tool_call_content(
                        &session_id,
                        entry_ix,
                        content,
                        content_ix,
                        tool_call,
                        false,
                        has_failed,
                        &focus_handle,
                        window,
                        cx,
                    )
                })
                .collect::<Vec<_>>();
            if !images.is_empty() {
                row = row.body(v_flex().w_full().gap_1().children(images));
            }
        }

        if matches!(status, ToolCallStatus::WaitingForConfirmation) {
            if let Some(details) = tool_call.sandbox_authorization_details.as_ref() {
                row = row.footer(self.render_sandbox_authorization_details(
                    entry_ix,
                    &tool_call.id,
                    details,
                    window,
                    cx,
                ));
            }
            if let Some(details) = tool_call.sandbox_fallback_authorization_details.as_ref() {
                row = row.footer(self.render_sandbox_fallback_authorization_details(details, cx));
            }
        }
        // ACP v2 requests and flat ACP v1 options are answered in the
        // permission card that replaces the prompt box, as in Claude Code.
        if let Some(request) = tool_call
            .authorization_id()
            .and_then(|id| self.thread.read(cx).permission_request(id))
            .filter(|request| !claude_code_card_handles(request))
        {
            row = row.footer(self.render_permission_buttons(
                session_id.clone(),
                self.is_first_tool_call(&session_id, &tool_call.id, cx),
                request,
                entry_ix,
                &focus_handle,
                self.sandbox_confusables_block_allow(tool_call, cx),
                cx,
            ));
        }

        div()
            .id(("claude-code-tool-entry", entry_ix))
            .debug_selector(move || format!("claude-code-tool-{entry_ix}"))
            .w_full()
            .px_5()
            .py_1()
            .text_ui(cx)
            .child(row)
    }

    /// Agents report absolute paths; Claude Code shows them relative to the workspace.
    fn claude_code_project_relative_path(
        &self,
        text: &SharedString,
        cx: &App,
    ) -> Option<SharedString> {
        let path = Path::new(text.as_ref());
        if !path.is_absolute() {
            return None;
        }
        let project = self.project.upgrade()?;
        let project = project.read(cx);
        let project_path = project.find_project_path(path, cx)?;
        if project_path.path.is_empty() {
            return None;
        }
        Some(
            project_path
                .path
                .display(project.path_style(cx))
                .into_owned()
                .into(),
        )
    }

    fn render_claude_code_compaction(
        &self,
        entry_ix: usize,
        compaction: &acp_thread::ContextCompaction,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (label, step_status) = match &compaction.status {
            acp_thread::ContextCompactionStatus::InProgress => {
                ("Compacting conversation…", StepStatus::Running)
            }
            acp_thread::ContextCompactionStatus::Completed => {
                ("Conversation compacted", StepStatus::Muted)
            }
            acp_thread::ContextCompactionStatus::Failed => {
                ("Conversation compaction failed", StepStatus::Failed)
            }
            acp_thread::ContextCompactionStatus::Canceled => {
                ("Conversation compaction canceled", StepStatus::Canceled)
            }
            acp_thread::ContextCompactionStatus::Other(_) => {
                ("Conversation compaction", StepStatus::Muted)
            }
        };
        let summary = &compaction.summary;
        let error = compaction.error.clone();
        let has_details = summary.blocks().len() > 0 || error.is_some();
        let is_expanded = has_details
            && self
                .entry_view_state
                .read(cx)
                .is_compaction_expanded(entry_ix);

        v_flex()
            .id(("claude-code-compaction", entry_ix))
            .w_full()
            .px_5()
            .py_1()
            .gap_1()
            .text_ui(cx)
            .child(
                div()
                    .id("header")
                    .child(timeline_row(
                        TimelineDot::new("dot", step_status),
                        Label::new(label).color(Color::Muted),
                        window,
                    ))
                    .when(has_details, |this| {
                        this.cursor_pointer().on_click(cx.listener(
                            move |this, _event: &ClickEvent, window, cx| {
                                this.toggle_compaction_expansion(entry_ix, window, cx);
                            },
                        ))
                    }),
            )
            .when(is_expanded, |this| {
                this.child(
                    v_flex()
                        .pl(TIMELINE_GUTTER)
                        .gap_2()
                        .children(summary.blocks().enumerate().map(|(content_ix, content)| {
                            self.render_output_content_block(
                                entry_ix, content_ix, content, None, true, window, cx,
                            )
                        }))
                        .when_some(error, |this, error| {
                            let mut style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
                            style.base_text_style.color = Color::Error.color(cx);
                            this.child(self.render_markdown(error, style, cx))
                        }),
                )
            })
            .into_any_element()
    }

    fn render_claude_code_generating(&self, cx: &Context<Self>) -> AnyElement {
        // Like Claude Code, the spinner steps aside while a prompt waits for the user.
        let awaiting_user = self.thread.read(cx).is_waiting_for_confirmation()
            || self.has_pending_request_elicitation(cx);
        if awaiting_user {
            return Empty.into_any_element();
        }
        div()
            .debug_selector(|| "claude-code-spinner".to_string())
            .px_5()
            .py_2()
            .child(ClaudeCodeSpinnerElement::new(
                self.turn_fields.turn_generation,
                self.turn_fields.turn_started_at,
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation_view::tests::{StubAgentServer, init_test};
    use acp_thread::StubAgentConnection;
    use agent_client_protocol::v1::schema as acp_v1;
    use fs::FakeFs;
    use gpui::{KeyBinding, TestAppContext, TextStyle, UpdateGlobal as _, VisualTestContext};
    use serde_json::json;
    use settings::SettingsStore;
    use theme_settings::ThemeSettings;
    use workspace::{Item, MultiWorkspace};

    struct TestThreadItem {
        conversation_view: Entity<ConversationView>,
    }

    impl Item for TestThreadItem {
        type Event = ();

        fn include_in_nav_history() -> bool {
            false
        }

        fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
            "Test".into()
        }
    }

    impl EventEmitter<()> for TestThreadItem {}

    impl Focusable for TestThreadItem {
        fn focus_handle(&self, cx: &App) -> FocusHandle {
            self.conversation_view.read(cx).focus_handle(cx)
        }
    }

    impl Render for TestThreadItem {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            v_flex().size_full().child(self.conversation_view.clone())
        }
    }

    fn set_ui_style(ui_style: &str, cx: &mut VisualTestContext) {
        cx.update(|_window, cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(
                        &format!(r#"{{ "agent": {{ "ui_style": "{ui_style}" }} }}"#),
                        cx,
                    )
                    .expect("valid ui_style settings");
            });
        });
        cx.run_until_parked();
    }

    async fn setup_thread_view(
        connection: StubAgentConnection,
        cx: &mut TestAppContext,
    ) -> (Entity<ThreadView>, &mut VisualTestContext) {
        init_test(cx);
        cx.update(crate::ui_styles::init);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
        let thread_store = cx.update(|_window, cx| cx.new(|cx| ThreadStore::new(cx)));
        let connection_store =
            cx.update(|_window, cx| cx.new(|cx| AgentConnectionStore::new(project.clone(), cx)));

        let conversation_view = cx.update(|window, cx| {
            cx.new(|cx| {
                ConversationView::new(
                    Rc::new(StubAgentServer::new(connection)),
                    connection_store,
                    Agent::Custom { id: "Test".into() },
                    None,
                    None,
                    None,
                    None,
                    None,
                    workspace.downgrade(),
                    project,
                    Some(thread_store),
                    AgentThreadSource::AgentPanel,
                    window,
                    cx,
                )
            })
        });
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(
                Box::new(cx.new(|_| TestThreadItem {
                    conversation_view: conversation_view.clone(),
                })),
                None,
                true,
                window,
                cx,
            );
        });
        cx.run_until_parked();

        let thread_view = cx.read(|cx| {
            conversation_view
                .read(cx)
                .active_thread()
                .cloned()
                .expect("active thread")
        });
        (thread_view, cx)
    }

    fn bash_tool_call() -> acp_v1::ToolCall {
        acp_v1::ToolCall::new("bash-1", "`git remote -v && ls crates`")
            .kind(acp_v1::ToolKind::Execute)
            .status(acp_v1::ToolCallStatus::Completed)
            .raw_input(json!({
                "command": "git remote -v && ls crates",
                "description": "Show remotes and list crates",
            }))
            .content(vec!["```console\norigin\nagent_ui\ngpui\nzed\n```".into()])
            .meta(acp_v2::Meta::from_iter([(
                "claudeCode".into(),
                json!({ "toolName": "Bash" }),
            )]))
    }

    #[gpui::test]
    async fn test_claude_code_style_renders_timeline(cx: &mut TestAppContext) {
        let connection = StubAgentConnection::new();
        connection.set_next_prompt_updates(vec![
            acp_v1::SessionUpdate::AgentThoughtChunk(acp_v1::ContentChunk::new(
                "Let me look at the repository.".into(),
            )),
            acp_v1::SessionUpdate::ToolCall(bash_tool_call()),
            acp_v1::SessionUpdate::AgentMessageChunk(acp_v1::ContentChunk::new(
                "Here is what I found.".into(),
            )),
        ]);
        let (thread_view, cx) = setup_thread_view(connection, cx).await;
        set_ui_style("claude_code", cx);

        let thread = thread_view.read_with(cx, |view, _| view.thread.clone());
        thread
            .update(cx, |thread, cx| thread.send_raw("Show me the repo", cx))
            .await
            .expect("prompt should succeed");
        cx.run_until_parked();

        let (tool_call_ix, summary, output) = thread_view.read_with(cx, |view, cx| {
            let entries = view.thread.read(cx).entries();
            entries
                .iter()
                .enumerate()
                .find_map(|(entry_ix, entry)| match entry {
                    AgentThreadEntry::ToolCall(tool_call) => {
                        let label = tool_call.label.read(cx).source().to_string();
                        let summary = summarize_tool_call(tool_call, &label);
                        let output =
                            tool_output_text(tool_call, summary.description.as_deref(), cx);
                        Some((entry_ix, summary, output))
                    }
                    _ => None,
                })
                .expect("tool call entry")
        });
        assert_eq!(summary.name.as_ref(), "Bash");
        assert_eq!(
            summary.description.as_deref(),
            Some("Show remotes and list crates")
        );
        assert_eq!(summary.input.as_deref(), Some("git remote -v && ls crates"));
        assert_eq!(output.as_deref(), Some("origin\nagent_ui\ngpui\nzed"));

        assert!(cx.debug_bounds("claude-code-user-message-0").is_some());
        assert!(cx.debug_bounds("claude-code-thought-1-0").is_some());
        assert_eq!(tool_call_ix, 2);
        assert!(cx.debug_bounds("claude-code-tool-2").is_some());

        thread_view.read_with(cx, |_, cx| {
            let activity =
                thread_activity(thread_view.entity_id(), cx).expect("thread activity is tracked");
            assert!(activity.user_message_sent_at(0).is_some());
            assert!(activity.thought_duration(1, 0).is_some());
        });
    }

    #[gpui::test]
    async fn test_claude_code_turn_controls(cx: &mut TestAppContext) {
        let connection = StubAgentConnection::new();
        let (thread_view, cx) = setup_thread_view(connection.clone(), cx).await;
        set_ui_style("claude_code", cx);

        let message_editor = thread_view.read_with(cx, |view, _| view.message_editor.clone());
        message_editor.update_in(cx, |editor, window, cx| {
            editor.set_text("Hello", window, cx);
        });
        thread_view.update_in(cx, |view, window, cx| view.send(window, cx));
        cx.run_until_parked();
        let session_id =
            thread_view.read_with(cx, |view, cx| view.thread.read(cx).session_id().clone());
        cx.update(|_window, cx| {
            connection.send_update(
                session_id.clone(),
                acp_v1::SessionUpdate::AgentMessageChunk(acp_v1::ContentChunk::new(
                    "Here is what I found.".into(),
                )),
                cx,
            );
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("claude-code-turn-controls-1").is_none(),
            "no controls while the turn runs"
        );

        connection.end_turn(session_id, acp_v1::StopReason::EndTurn);
        cx.run_until_parked();
        assert!(cx.debug_bounds("claude-code-turn-controls-1").is_some());
    }

    #[gpui::test]
    async fn test_claude_code_activity_bar_matches_prompt_box(cx: &mut TestAppContext) {
        let connection = StubAgentConnection::new();
        let (thread_view, cx) = setup_thread_view(connection.clone(), cx).await;
        let session_id =
            thread_view.read_with(cx, |view, cx| view.thread.read(cx).session_id().clone());
        cx.update(|_window, cx| {
            connection.send_update(
                session_id,
                acp_v1::SessionUpdate::Plan(acp_v1::Plan::new(vec![acp_v1::PlanEntry::new(
                    "Do the thing",
                    acp_v1::PlanEntryPriority::Medium,
                    acp_v1::PlanEntryStatus::Pending,
                )])),
                cx,
            );
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("plan_summary").is_some());
        assert!(cx.debug_bounds("claude-code-activity-bar").is_none());

        set_ui_style("claude_code", cx);
        let activity_bar = cx
            .debug_bounds("claude-code-activity-bar")
            .expect("activity bar in the Claude Code style");
        let plan_summary = cx.debug_bounds("plan_summary").expect("plan summary");
        let prompt_box = cx
            .debug_bounds("claude-code-prompt-box")
            .expect("prompt box");
        assert!(
            plan_summary.top() >= activity_bar.top()
                && plan_summary.bottom() <= activity_bar.bottom()
        );
        assert_eq!(activity_bar.left(), prompt_box.left());
        assert_eq!(activity_bar.right(), prompt_box.right());
        assert!(activity_bar.bottom() <= prompt_box.top());
    }

    /// The default keymap's global menu bindings, plus the `Workspace` escape
    /// binding they have to win over.
    fn bind_permission_keys(cx: &mut VisualTestContext) {
        cx.update(|_window, cx| {
            cx.bind_keys([
                KeyBinding::new("escape", menu::Cancel, None),
                KeyBinding::new("escape", workspace::Unfollow, Some("Workspace")),
                KeyBinding::new("down", menu::SelectNext, None),
                KeyBinding::new("up", menu::SelectPrevious, None),
                KeyBinding::new("enter", menu::Confirm, None),
            ]);
        });
    }

    fn focus_message_editor(thread_view: &Entity<ThreadView>, cx: &mut VisualTestContext) {
        thread_view.update_in(cx, |view, window, cx| {
            let editor_focus = view.message_editor.focus_handle(cx);
            window.focus(&editor_focus, cx);
        });
        cx.run_until_parked();
    }

    fn permission_card_is_focused(
        thread_view: &Entity<ThreadView>,
        cx: &mut VisualTestContext,
    ) -> bool {
        let thread_view_id = thread_view.entity_id();
        cx.update(|window, cx| {
            thread_activity(thread_view_id, cx)
                .and_then(|activity| activity.permission_focus())
                .is_some_and(|focus| focus.is_focused(window))
        })
    }

    fn selected_legacy_option(outcome: acp_thread::RequestPermissionOutcome) -> String {
        match outcome {
            acp_thread::RequestPermissionOutcome::Selected(selected) => {
                selected.option_id.0.to_string()
            }
            other => panic!("expected a selected option, got {other:?}"),
        }
    }

    fn selected_option(outcome: acp_v2::RequestPermissionOutcome) -> String {
        match outcome {
            acp_v2::RequestPermissionOutcome::Selected(selected) => {
                selected.option_id.0.to_string()
            }
            other => panic!("expected a selected option, got {other:?}"),
        }
    }

    #[gpui::test]
    async fn test_claude_code_permission_card_keys(cx: &mut TestAppContext) {
        let (thread_view, cx) = setup_thread_view(StubAgentConnection::new(), cx).await;
        set_ui_style("claude_code", cx);
        bind_permission_keys(cx);
        let thread = thread_view.read_with(cx, |view, _| view.thread.clone());

        // claude-agent-acp v0.106.0 Bash options, sent as an ACP v1 request.
        let request_authorization = |tool_call_id: &str, cx: &mut VisualTestContext| {
            let task = thread.update(cx, |thread, cx| {
                thread
                    .request_tool_call_authorization(
                        acp_v1::ToolCall::new(tool_call_id.to_string(), "git status")
                            .kind(acp_v1::ToolKind::Execute)
                            .into(),
                        acp_thread::PermissionOptions::Flat(vec![
                            acp_v2::PermissionOption::new(
                                "allow-once",
                                "Yes",
                                acp_v2::PermissionOptionKind::AllowOnce,
                            ),
                            acp_v2::PermissionOption::new(
                                "allow-with-updates",
                                "Yes, and don't ask again for `git status` commands",
                                acp_v2::PermissionOptionKind::AllowAlways,
                            ),
                            acp_v2::PermissionOption::new(
                                "reject",
                                "No",
                                acp_v2::PermissionOptionKind::RejectOnce,
                            ),
                        ]),
                        acp_thread::AuthorizationKind::PermissionGrant,
                        cx,
                    )
                    .expect("authorization request")
            });
            cx.run_until_parked();
            task
        };

        focus_message_editor(&thread_view, cx);
        let first = request_authorization("bash-1", cx);
        assert!(cx.debug_bounds("claude-code-permission-card").is_some());
        assert!(cx.debug_bounds("claude-code-permission-option-2").is_some());
        assert!(cx.debug_bounds("claude-code-permission-option-3").is_none());
        assert!(
            cx.debug_bounds("claude-code-prompt-box").is_none(),
            "the card takes the prompt box's place"
        );
        assert!(
            permission_card_is_focused(&thread_view, cx),
            "focus moves from the prompt to the card"
        );
        cx.simulate_keystrokes("2");
        assert_eq!(selected_legacy_option(first.await), "allow-with-updates");
        cx.run_until_parked();
        assert!(cx.debug_bounds("claude-code-permission-card").is_none());
        assert!(cx.debug_bounds("claude-code-prompt-box").is_some());
        let editor_focused = thread_view.update_in(cx, |view, window, cx| {
            view.message_editor.focus_handle(cx).is_focused(window)
        });
        assert!(editor_focused, "focus returns to the prompt once answered");

        let second = request_authorization("bash-2", cx);
        assert!(permission_card_is_focused(&thread_view, cx));
        cx.simulate_keystrokes("escape");
        assert_eq!(selected_legacy_option(second.await), "reject");
        cx.run_until_parked();

        let third = request_authorization("bash-3", cx);
        cx.simulate_keystrokes("down enter");
        assert_eq!(selected_legacy_option(third.await), "allow-with-updates");
    }

    #[gpui::test]
    async fn test_claude_code_plan_approval_card(cx: &mut TestAppContext) {
        let (thread_view, cx) = setup_thread_view(StubAgentConnection::new(), cx).await;
        set_ui_style("claude_code", cx);
        bind_permission_keys(cx);
        let thread = thread_view.read_with(cx, |view, _| view.thread.clone());
        let session_id = thread.read_with(cx, |thread, _| thread.session_id().0.to_string());

        // claude-agent-acp v0.106.0 ExitPlanMode request over ACP v2, with auto mode available.
        let request_plan_approval = |tool_call_id: &str, cx: &mut VisualTestContext| {
            let request: acp_v2::RequestPermissionRequest = serde_json::from_value(json!({
                "sessionId": session_id,
                "title": "Ready to code?",
                "subject": {
                    "type": "tool_call",
                    "toolCall": {
                        "toolCallId": tool_call_id,
                        "title": "Approve Plan",
                        "kind": "switch_mode",
                        "status": "pending",
                        "content": [{
                            "type": "content",
                            "content": { "type": "text", "text": "# Plan\n\n1. Add the card" }
                        }]
                    }
                },
                "options": [
                    { "optionId": "exit-plan-auto", "name": "Yes, and use auto mode", "kind": "allow_always" },
                    { "optionId": "exit-plan-default", "name": "Yes, manually approve edits", "kind": "allow_once" },
                    { "optionId": "reject", "name": "No, keep planning", "kind": "reject_once" }
                ]
            }))
            .expect("plan approval request");
            let (_, task) = thread.update(cx, |thread, cx| {
                thread
                    .request_permission(request, cx)
                    .expect("permission request")
            });
            cx.run_until_parked();
            task
        };

        focus_message_editor(&thread_view, cx);
        let first = request_plan_approval("plan-1", cx);
        assert!(cx.debug_bounds("claude-code-permission-card").is_some());
        assert!(cx.debug_bounds("claude-code-permission-plan").is_some());
        assert!(cx.debug_bounds("claude-code-prompt-box").is_none());
        assert!(
            cx.debug_bounds("standalone-generic-permissions").is_none()
                && cx.debug_bounds("claude-code-activity-bar").is_none(),
            "only the card asks"
        );
        assert!(permission_card_is_focused(&thread_view, cx));
        cx.simulate_keystrokes("1");
        assert_eq!(selected_option(first.await), "exit-plan-auto");
        cx.run_until_parked();

        let second = request_plan_approval("plan-2", cx);
        cx.simulate_keystrokes("escape");
        assert_eq!(selected_option(second.await), "reject");
        cx.run_until_parked();

        let third = request_plan_approval("plan-3", cx);
        cx.simulate_keystrokes("down enter");
        assert_eq!(selected_option(third.await), "exit-plan-default");
    }

    #[gpui::test]
    async fn test_switching_ui_style_rerenders_thread(cx: &mut TestAppContext) {
        let connection = StubAgentConnection::new();
        connection.set_next_prompt_updates(vec![acp_v1::SessionUpdate::ToolCall(bash_tool_call())]);
        let (thread_view, cx) = setup_thread_view(connection, cx).await;
        let thread = thread_view.read_with(cx, |view, _| view.thread.clone());
        thread
            .update(cx, |thread, cx| thread.send_raw("Run a command", cx))
            .await
            .expect("prompt should succeed");
        cx.run_until_parked();

        assert!(cx.debug_bounds("claude-code-user-message-0").is_none());
        assert!(cx.debug_bounds("claude-code-tool-1").is_none());

        set_ui_style("claude_code", cx);
        assert!(cx.debug_bounds("claude-code-user-message-0").is_some());
        assert!(cx.debug_bounds("claude-code-tool-1").is_some());

        set_ui_style("zed", cx);
        assert!(cx.debug_bounds("claude-code-user-message-0").is_none());
        assert!(cx.debug_bounds("claude-code-tool-1").is_none());
    }

    fn placeholder(message_editor: &Entity<MessageEditor>, cx: &mut VisualTestContext) -> String {
        message_editor.update(cx, |message_editor, cx| {
            message_editor
                .editor()
                .update(cx, |editor, cx| editor.placeholder_text(cx))
                .unwrap_or_default()
        })
    }

    #[gpui::test]
    async fn test_claude_code_prompt_box(cx: &mut TestAppContext) {
        let connection = StubAgentConnection::new();
        let (thread_view, cx) = setup_thread_view(connection.clone(), cx).await;
        let message_editor = thread_view.read_with(cx, |view, _| view.message_editor.clone());
        let zed_placeholder = placeholder(&message_editor, cx);
        assert!(cx.debug_bounds("claude-code-prompt-box").is_none());

        set_ui_style("claude_code", cx);
        assert!(cx.debug_bounds("claude-code-prompt-box").is_some());
        assert_eq!(placeholder(&message_editor, cx), "Message Test…");

        message_editor.update_in(cx, |editor, window, cx| {
            editor.set_text("Hello", window, cx);
        });
        thread_view.update_in(cx, |view, window, cx| view.send(window, cx));
        cx.run_until_parked();
        assert_eq!(placeholder(&message_editor, cx), "Queue another message…");

        let session_id =
            thread_view.read_with(cx, |view, cx| view.thread.read(cx).session_id().clone());
        connection.end_turn(session_id, acp_v1::StopReason::EndTurn);
        cx.run_until_parked();
        assert_eq!(placeholder(&message_editor, cx), "Message Test…");

        set_ui_style("zed", cx);
        assert!(cx.debug_bounds("claude-code-prompt-box").is_none());
        assert_eq!(placeholder(&message_editor, cx), zed_placeholder);
    }

    #[gpui::test]
    async fn test_claude_code_prompt_uses_ui_font(cx: &mut TestAppContext) {
        let (_thread_view, cx) = setup_thread_view(StubAgentConnection::new(), cx).await;
        let set_settings = |ui_style: &str, cx: &mut VisualTestContext| {
            cx.update(|_window, cx| {
                SettingsStore::update_global(cx, |store, cx| {
                    store
                        .set_user_settings(
                            &format!(
                                r#"{{
                                    "ui_font_family": "Helvetica",
                                    "buffer_font_family": "Menlo",
                                    "agent": {{ "ui_style": "{ui_style}" }}
                                }}"#
                            ),
                            cx,
                        )
                        .expect("valid settings");
                });
            });
            cx.run_until_parked();
        };
        let zed_style = TextStyle {
            font_family: "Menlo".into(),
            ..TextStyle::default()
        };
        let prompt_font = |cx: &mut VisualTestContext| {
            cx.update(|_window, cx| {
                crate::ui_styles::message_editor_text_style(zed_style.clone(), cx).font_family
            })
        };

        set_settings("zed", cx);
        assert_eq!(prompt_font(cx).as_ref(), "Menlo");

        set_settings("claude_code", cx);
        assert_eq!(prompt_font(cx).as_ref(), "Helvetica");
        assert_eq!(
            cx.update(|_window, cx| ThemeSettings::get_global(cx).ui_font.family.clone())
                .as_ref(),
            "Helvetica"
        );

        set_settings("zed", cx);
        assert_eq!(prompt_font(cx).as_ref(), "Menlo");
    }

    #[gpui::test]
    async fn test_claude_code_slash_button_opens_commands(cx: &mut TestAppContext) {
        let (thread_view, cx) = setup_thread_view(StubAgentConnection::new(), cx).await;
        set_ui_style("claude_code", cx);
        let message_editor = thread_view.read_with(cx, |view, _| view.message_editor.clone());

        thread_view.update_in(cx, |view, window, cx| {
            view.open_claude_code_slash_menu(window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            message_editor.read_with(cx, |editor, cx| editor.text(cx)),
            "/"
        );

        message_editor.update_in(cx, |editor, window, cx| {
            editor.set_text("explain", window, cx);
            editor.editor().update(cx, |editor, cx| {
                editor.move_to_end(&editor::actions::MoveToEnd, window, cx)
            });
        });
        thread_view.update_in(cx, |view, window, cx| {
            view.open_claude_code_slash_menu(window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            message_editor.read_with(cx, |editor, cx| editor.text(cx)),
            "explain /"
        );
    }

    #[gpui::test]
    async fn test_claude_code_spinner_while_generating(cx: &mut TestAppContext) {
        let connection = StubAgentConnection::new();
        let (thread_view, cx) = setup_thread_view(connection.clone(), cx).await;
        set_ui_style("claude_code", cx);

        let message_editor = thread_view.read_with(cx, |view, _| view.message_editor.clone());
        message_editor.update_in(cx, |editor, window, cx| {
            editor.set_text("Hello", window, cx);
        });
        thread_view.update_in(cx, |view, window, cx| view.send(window, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("claude-code-spinner").is_some());

        let session_id =
            thread_view.read_with(cx, |view, cx| view.thread.read(cx).session_id().clone());
        cx.update(|_window, cx| {
            connection.send_update(
                session_id.clone(),
                acp_v1::SessionUpdate::AgentMessageChunk(acp_v1::ContentChunk::new(
                    "Working on it".into(),
                )),
                cx,
            );
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("claude-code-spinner").is_some());

        connection.end_turn(session_id, acp_v1::StopReason::EndTurn);
        cx.run_until_parked();
        assert!(cx.debug_bounds("claude-code-spinner").is_none());
    }
}
