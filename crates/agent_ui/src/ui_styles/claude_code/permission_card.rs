// Declared from `thread.rs` (via a `#[path]` attribute), so this module is a
// descendant of `thread_view` and can resolve permission requests through the
// same `ThreadView` and `ConversationView` paths as Zed's permission buttons.

use acp_thread::{PermissionOptions, PermissionRequest, PermissionRequestId};
use gpui::{FocusHandle, FontWeight, KeyDownEvent};

use super::claude_code_prompt_box::PROMPT_BOX_RADIUS;
use super::*;
use crate::ui_styles::claude_code::{
    is_allow_option, is_plan_approval, permission_option_for_key, reject_option_index,
    set_permission_selection,
};

/// A pending permission request, as the Claude Code permission card shows it:
/// the adapter's title and options verbatim.
pub(in crate::conversation_view::thread_view) struct ClaudeCodePermissionPrompt {
    session_id: acp_v2::SessionId,
    request_id: PermissionRequestId,
    /// ACP v2 requests are answered by option id; ACP v1 (legacy) requests
    /// through `authorize_permission_request`.
    is_generic: bool,
    title: SharedString,
    description: Option<SharedString>,
    options: Vec<acp_v2::PermissionOption>,
    plan: Option<Entity<Markdown>>,
    allow_disabled: bool,
}

/// Whether the Claude Code card answers this request. Zed's own agent asks
/// with dropdown choices and permission patterns, which keep Zed's buttons.
pub(in crate::conversation_view::thread_view) fn claude_code_card_handles(
    request: &PermissionRequest,
) -> bool {
    request.generic_request().is_some()
        || matches!(request.legacy_options(), Some(PermissionOptions::Flat(_)))
}

impl ThreadView {
    /// The request the card shows: the one `allow_once` and the other
    /// keyboard shortcuts would answer.
    pub(in crate::conversation_view::thread_view) fn claude_code_permission_prompt(
        &self,
        cx: &App,
    ) -> Option<ClaudeCodePermissionPrompt> {
        let session_id = self.thread.read(cx).session_id().clone();
        let conversation = self.conversation.read(cx);
        let (request_session_id, request) =
            conversation.pending_permission_request(&session_id, cx)?;
        if !claude_code_card_handles(request) {
            return None;
        }
        let thread = conversation.thread(&request_session_id)?.read(cx);

        let (is_generic, title, description, options, tool_call) = match request.generic_request() {
            Some(generic) => (
                true,
                SharedString::from(generic.title.clone()),
                generic.description.clone().map(SharedString::from),
                generic.options.clone(),
                Self::permission_tool_call(thread, generic).map(|(_, tool_call)| tool_call),
            ),
            None => {
                let Some(PermissionOptions::Flat(options)) = request.legacy_options() else {
                    return None;
                };
                let tool_call = request
                    .legacy_tool_call_id()
                    .and_then(|tool_call_id| thread.tool_call(tool_call_id))
                    .map(|(_, tool_call)| tool_call);
                // ACP v1 requests carry no title of their own, so the tool call's stands in.
                let title = tool_call
                    .map(|tool_call| tool_call.label.read(cx).source().to_string().into())
                    .unwrap_or_default();
                (false, title, None, options.clone(), tool_call)
            }
        };
        if options.is_empty() {
            return None;
        }

        let plan = if is_plan_approval(&options) {
            tool_call.and_then(|tool_call| {
                tool_call
                    .content()
                    .iter()
                    .find_map(|content| match content {
                        ToolCallContent::ContentBlock { block, .. } => block.markdown().cloned(),
                        _ => None,
                    })
            })
        } else {
            None
        };
        let allow_disabled =
            tool_call.is_some_and(|tool_call| self.sandbox_confusables_block_allow(tool_call, cx));

        Some(ClaudeCodePermissionPrompt {
            session_id: request_session_id,
            request_id: request.id,
            is_generic,
            title,
            description,
            options,
            plan,
            allow_disabled,
        })
    }

    /// The Claude Code permission prompt: it takes the prompt box's place, with
    /// the request's title, the plan for a plan approval, and the options as
    /// numbered rows. `1`-`9` pick an option, Up/Down and Enter pick the
    /// highlighted one, and Esc picks "No".
    pub(in crate::conversation_view::thread_view) fn render_claude_code_permission_card(
        &self,
        prompt: ClaudeCodePermissionPrompt,
        focus_handle: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let is_focused = focus_handle.contains_focused(window, cx);
        let option_count = prompt.options.len();
        let selected_index = self.claude_code_permission_selection(&prompt, cx);
        let max_content_width = AgentSettings::get_global(cx).max_content_width;

        let options =
            prompt
                .options
                .iter()
                .enumerate()
                .map(|(index, option)| {
                    let is_disabled = prompt.allow_disabled && is_allow_option(option);
                    let is_selected = index == selected_index;
                    h_flex()
                        .id(("claude-code-permission-option", index))
                        .debug_selector(move || format!("claude-code-permission-option-{index}"))
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded(px(4.))
                        .border_1()
                        .map(|this| {
                            if is_selected {
                                this.border_color(colors.border_focused)
                                    .bg(colors.element_selected)
                            } else {
                                this.border_color(colors.border_variant)
                            }
                        })
                        .when(!is_disabled, |this| {
                            this.cursor_pointer()
                                .hover(|style| style.bg(colors.element_hover))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.choose_claude_code_permission_option(index, window, cx);
                                }))
                        })
                        .child(Label::new((index + 1).to_string()).color(Color::Muted))
                        .child(div().flex_1().min_w_0().child(
                            Label::new(option.name.clone()).color(if is_disabled {
                                Color::Disabled
                            } else {
                                Color::Default
                            }),
                        ))
                })
                .collect::<Vec<_>>();

        let plan = prompt.plan.clone().map(|plan| {
            div()
                .id("claude-code-permission-plan")
                .debug_selector(|| "claude-code-permission-plan".into())
                .max_h_64()
                .overflow_y_scroll()
                .p_2()
                .rounded(px(4.))
                .border_1()
                .border_color(colors.border_variant)
                .child(MarkdownElement::new(
                    plan,
                    MarkdownStyle::themed(MarkdownFont::Agent, window, cx),
                ))
        });

        let card = v_flex()
            .id("claude-code-permission-card")
            .debug_selector(|| "claude-code-permission-card".into())
            .key_context("ClaudeCodePermissionPrompt")
            .track_focus(focus_handle)
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                if let Some(index) = permission_option_for_key(&event.keystroke.key, option_count) {
                    cx.stop_propagation();
                    this.choose_claude_code_permission_option(index, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &menu::Cancel, window, cx| {
                this.reject_claude_code_permission(window, cx);
            }))
            .on_action(cx.listener(|this, _: &menu::SelectNext, _, cx| {
                this.move_claude_code_permission_selection(1, cx);
            }))
            .on_action(cx.listener(|this, _: &menu::SelectPrevious, _, cx| {
                this.move_claude_code_permission_selection(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &menu::Confirm, window, cx| {
                this.confirm_claude_code_permission_selection(window, cx);
            }))
            .w_full()
            .min_w_0()
            .p_2()
            .gap_1p5()
            .rounded(PROMPT_BOX_RADIUS)
            .border_1()
            .border_color(if is_focused {
                colors.border_focused
            } else {
                colors.border
            })
            .bg(colors.editor_background)
            .text_ui(cx)
            .child(
                div()
                    .debug_selector(|| "claude-code-permission-title".into())
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(prompt.title.clone()),
            )
            .children(prompt.description.clone().map(|description| {
                Label::new(description)
                    .size(LabelSize::Small)
                    .color(Color::Muted)
            }))
            .children(plan)
            .child(v_flex().w_full().gap_1().children(options));

        h_flex()
            .w_full()
            .flex_none()
            .justify_center()
            .px_3()
            .pt_1()
            .pb_3()
            .bg(colors.editor_background)
            .child(
                div()
                    .when_some(max_content_width, |this, max_width| {
                        this.flex_basis(max_width)
                    })
                    .when(max_content_width.is_none(), |this| this.w_full())
                    .min_w_0()
                    .flex_shrink_1()
                    .flex_grow_0()
                    .child(card),
            )
            .into_any_element()
    }

    fn choose_claude_code_permission_option(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(prompt) = self.claude_code_permission_prompt(cx) else {
            return;
        };
        let Some(option) = prompt.options.get(index) else {
            return;
        };
        if prompt.allow_disabled && is_allow_option(option) {
            return;
        }
        let option = option.clone();
        self.resolve_claude_code_permission(&prompt, &option, window, cx);
    }

    fn reject_claude_code_permission(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.claude_code_permission_prompt(cx) else {
            return;
        };
        let reject_option = reject_option_index(&prompt.options)
            .and_then(|index| prompt.options.get(index))
            .cloned();
        match reject_option {
            Some(option) => {
                self.resolve_claude_code_permission(&prompt, &option, window, cx);
            }
            None if prompt.is_generic => {
                self.conversation.update(cx, |conversation, cx| {
                    conversation.cancel_permission_request(
                        &prompt.session_id,
                        prompt.request_id,
                        cx,
                    );
                });
                cx.notify();
            }
            None => {}
        }
    }

    fn claude_code_permission_selection(
        &self,
        prompt: &ClaudeCodePermissionPrompt,
        cx: &Context<Self>,
    ) -> usize {
        thread_activity(cx.entity_id(), cx)
            .map(|activity| activity.permission_selection(prompt.request_id))
            .unwrap_or_default()
            .min(prompt.options.len().saturating_sub(1))
    }

    fn move_claude_code_permission_selection(&mut self, offset: isize, cx: &mut Context<Self>) {
        let Some(prompt) = self.claude_code_permission_prompt(cx) else {
            return;
        };
        let last_index = prompt.options.len().saturating_sub(1);
        let next = self
            .claude_code_permission_selection(&prompt, cx)
            .saturating_add_signed(offset)
            .min(last_index);
        set_permission_selection(cx.entity_id(), prompt.request_id, next, cx);
        cx.notify();
    }

    fn confirm_claude_code_permission_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(prompt) = self.claude_code_permission_prompt(cx) else {
            return;
        };
        let index = self.claude_code_permission_selection(&prompt, cx);
        self.choose_claude_code_permission_option(index, window, cx);
    }

    fn resolve_claude_code_permission(
        &mut self,
        prompt: &ClaudeCodePermissionPrompt,
        option: &acp_v2::PermissionOption,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if prompt.is_generic {
            self.conversation.update(cx, |conversation, cx| {
                conversation.select_permission_option(
                    &prompt.session_id,
                    prompt.request_id,
                    option.option_id.clone(),
                    cx,
                );
            });
            cx.notify();
        } else {
            self.authorize_permission_request(
                prompt.session_id.clone(),
                prompt.request_id,
                SelectedPermissionOutcome::new(option.option_id.clone(), option.kind.clone()),
                window,
                cx,
            );
        }
    }
}
