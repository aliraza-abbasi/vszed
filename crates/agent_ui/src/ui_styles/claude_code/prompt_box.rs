// Declared from `thread.rs` (via a `#[path]` attribute), so this module is a
// descendant of `thread_view` and can reuse `ThreadView`'s private state and
// helpers, such as the add-context menu and the token usage ring.

use std::time::Instant;

use editor::code_context_menus::CodeContextMenu;
use editor::{EditorMode, ToOffset as _};
use gpui::{BoxShadow, canvas, linear_color_stop, linear_gradient, point};
use ui::DocumentationSide;

use super::*;
use crate::ui::documentation_aside_side;
use crate::ui_styles::claude_code::{
    CacheClockElement, ClaudeCodeModelPicker, ConfigOptionsHandle, PromptAccent, QUEUE_PLACEHOLDER,
    StatusDot, agent_count_label, agent_summaries, agents_chip_tooltip, agents_dot_state,
    first_select_option, idle_placeholder, is_claude_code_placeholder, live_agent_count,
    mode_display_name, mode_icon, model_chip_parts, open_agent_map, prompt_accent,
    remember_zed_placeholder, select_entries, set_model_picker, thread_activity,
};

pub(super) const PROMPT_BOX_RADIUS: Pixels = px(8.);
/// The extension's prompt box is at most 680px wide, centered in the editor.
const PROMPT_BOX_MAX_WIDTH: Pixels = px(680.);
/// The transcript fades out over this height above the bottom of the editor, behind
/// and beside the floating prompt box.
const TRANSCRIPT_FADE_HEIGHT: Pixels = px(150.);
/// The input grows with its content up to about 200px, then scrolls.
const MAX_INPUT_LINES: usize = 9;

impl ThreadView {
    /// The Claude Code prompt box: a rounded, bordered box with the message editor on
    /// top and a footer of `+`, `/`, the prompt cache clock, the agents pill, the model
    /// pill and the context indicator on the left, and the mode chip and the send/stop
    /// button on the right.
    pub(in crate::conversation_view::thread_view) fn render_claude_code_message_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.sync_claude_code_placeholder(window, cx);
        self.sync_claude_code_editor_mode(cx);

        // Like Claude Code, a pending permission prompt replaces the prompt box.
        // Focus follows it: the card while it is up, the editor once it's answered.
        let permission_focus = thread_activity(cx.entity_id(), cx)
            .and_then(|activity| activity.permission_focus())
            .cloned();
        if let Some(permission_focus) = permission_focus {
            if let Some(prompt) = self.claude_code_permission_prompt(cx) {
                if self.message_editor.focus_handle(cx).is_focused(window) {
                    let permission_focus = permission_focus.clone();
                    cx.defer_in(window, move |_, window, cx| {
                        window.focus(&permission_focus, cx);
                    });
                }
                return self.render_claude_code_permission_card(
                    prompt,
                    &permission_focus,
                    window,
                    cx,
                );
            }
            if permission_focus.is_focused(window) {
                let editor_focus = self.message_editor.focus_handle(cx);
                cx.defer_in(window, move |_, window, cx| {
                    window.focus(&editor_focus, cx);
                });
            }
        }

        let colors = cx.theme().colors().clone();
        let editor_background = colors.editor_background;
        let config_options = self.claude_code_config_options(cx);
        let mode_id = config_options.as_ref().and_then(|config_options| {
            let options = config_options.options();
            let mode = first_select_option(&options, &acp_v2::SessionConfigOptionCategory::Mode)?;
            match &mode.kind {
                acp_v2::SessionConfigKind::Select(select) => {
                    Some(select.current_value.0.to_string())
                }
                _ => None,
            }
        });
        let accent = prompt_accent(mode_id.as_deref(), cx);
        let is_focused = self
            .message_editor
            .focus_handle(cx)
            .contains_focused(window, cx);
        let (border_color, shadows) = if is_focused {
            (
                accent.ring,
                vec![BoxShadow {
                    color: accent.ring.opacity(0.2),
                    offset: point(px(0.), px(1.)),
                    blur_radius: px(2.),
                    spread_radius: px(0.),
                    inset: false,
                }],
            )
        } else {
            (
                colors.border,
                vec![BoxShadow {
                    color: gpui::black().opacity(0.1),
                    offset: point(px(0.), px(1.)),
                    blur_radius: px(2.),
                    spread_radius: px(0.),
                    inset: false,
                }],
            )
        };
        let editor_expanded = self.editor_expanded && self.has_visible_conversation(cx);
        let composer_is_read_only = self.message_editor.read(cx).editor().read(cx).read_only(cx);
        let discard_draft_button = composer_is_read_only.then(|| {
            Button::new("discard-protected-draft", "Discard draft")
                .label_size(LabelSize::Small)
                .on_click(cx.listener(|this, _, window, cx| {
                    this._draft_resolve_task.take();
                    this.message_editor.update(cx, |editor, cx| {
                        editor.set_read_only(false, cx);
                        editor.set_message(Vec::new(), window, cx);
                    });
                    this.clear_thread_error(cx);
                }))
        });

        let model_chip = match &config_options {
            Some(config_options) => self
                .render_claude_code_model_chip(config_options, cx)
                .map(IntoElement::into_any_element),
            None => self
                .model_selector
                .clone()
                .map(IntoElement::into_any_element),
        };
        let mode_chip = match &config_options {
            Some(config_options) => self
                .render_claude_code_mode_chip(config_options)
                .map(IntoElement::into_any_element),
            None => self
                .mode_selector
                .clone()
                .map(IntoElement::into_any_element),
        };
        // Zed's own agent has no adapter config options; keep its controls
        // usable in this style.
        let native_controls = if config_options.is_none() {
            Some(
                h_flex()
                    .gap_0p5()
                    .children(self.render_fast_mode_control(cx))
                    .children(self.render_thinking_control(cx))
                    .children(self.profile_selector.clone()),
            )
        } else {
            None
        };

        let footer = h_flex()
            .w_full()
            .min_w_0()
            .flex_none()
            .flex_wrap()
            .justify_between()
            .gap_1p5()
            .p_1()
            .border_t_1()
            .border_color(colors.border_variant)
            .child(
                h_flex()
                    .min_w_0()
                    .flex_wrap()
                    .gap_1()
                    .child(self.render_add_context_button(cx))
                    .child(self.render_claude_code_slash_button(cx))
                    .child(CacheClockElement::new(cx.entity_id()))
                    .children(self.render_claude_code_agents_chip(cx))
                    .children(model_chip)
                    .children(native_controls)
                    .children(self.render_token_usage(cx)),
            )
            .child(
                h_flex()
                    .min_w_0()
                    .flex_wrap()
                    .gap_1()
                    .children(self.render_activity_status_label(cx))
                    .children(discard_draft_button)
                    .children(mode_chip)
                    .child(self.render_claude_code_send_button(&accent, cx)),
            );

        let model_picker = thread_activity(cx.entity_id(), cx)
            .and_then(|activity| activity.model_picker().cloned());

        v_flex()
            .id("claude-code-prompt-box")
            .debug_selector(|| "claude-code-prompt-box".into())
            .relative()
            .w_full()
            .min_w_0()
            .when(editor_expanded, |this| this.h(vh(0.8, window)))
            .rounded(PROMPT_BOX_RADIUS)
            .border_1()
            .border_color(border_color)
            .shadow(shadows)
            .bg(editor_background)
            .block_mouse_except_scroll()
            .on_action(cx.listener(Self::handle_message_editor_move_up))
            .on_action(cx.listener(Self::expand_message_editor))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .when(editor_expanded, |this| this.flex_1().min_h_0())
                    .px_3p5()
                    .py_2p5()
                    .child(self.message_editor.clone()),
            )
            .child(footer)
            .when_some(model_picker, |this, picker| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_full()
                        .mb_1()
                        .child(picker),
                )
            })
            .into_any_element()
    }

    /// The Claude Code style lays the transcript over the whole editor and floats the
    /// prompt box (with Zed's plan, edits and queue rows above it) over its bottom,
    /// centered and at most 680px wide. The transcript fades out behind and beside the
    /// box and keeps its last entry clear of it.
    pub(in crate::conversation_view::thread_view) fn render_ui_style_conversation(
        &mut self,
        conversation: AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !crate::ui_styles::claude_code_style_enabled(cx) {
            return conversation;
        }
        let activity_bar = self.render_claude_code_activity_bar(window, cx);
        let prompt_box =
            (!self.is_subagent()).then(|| self.render_claude_code_message_editor(window, cx));
        let editor_background = cx.theme().colors().editor_background;
        let thread_view_id = cx.entity_id();
        let floating_height =
            thread_activity(thread_view_id, cx).map(|activity| activity.floating_prompt_height());

        let floating = h_flex()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .justify_center()
            .px_3()
            .pt_1()
            .pb_3()
            .child(
                v_flex()
                    .w_full()
                    .max_w(PROMPT_BOX_MAX_WIDTH)
                    .min_w_0()
                    .gap_1()
                    .children(activity_bar)
                    .children(prompt_box),
            )
            .when_some(floating_height, |this, floating_height| {
                this.child(
                    canvas(
                        move |bounds, _, cx| {
                            let height = bounds.size.height;
                            if floating_height.get() != height {
                                floating_height.set(height);
                                // Re-render with the new list padding once this frame is
                                // drawn; views can't be invalidated while drawing.
                                cx.defer(move |cx| cx.notify(thread_view_id));
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
            });

        div()
            .relative()
            .flex_1()
            .min_h_0()
            .size_full()
            .child(conversation)
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(TRANSCRIPT_FADE_HEIGHT)
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(editor_background.opacity(0.), 0.),
                        linear_color_stop(editor_background, 1.),
                    )),
            )
            .child(floating)
            .into_any_element()
    }

    /// The prompt starts at one line and grows with its content, like Claude Code's.
    fn sync_claude_code_editor_mode(&mut self, cx: &mut Context<Self>) {
        if self.editor_expanded {
            return;
        }
        let mode = EditorMode::AutoHeight {
            min_lines: 1,
            max_lines: Some(MAX_INPUT_LINES),
        };
        let is_current = self.message_editor.read(cx).editor().read(cx).mode() == &mode;
        if !is_current {
            self.message_editor.update(cx, |message_editor, cx| {
                message_editor.set_mode(mode, cx);
            });
        }
    }

    /// Zed's activity bar (pending permissions, the plan, edited files and the
    /// message queue) with the same rows and behavior, drawn as a box with the
    /// prompt box's width, radius and colors so the two read as one column.
    pub(in crate::conversation_view::thread_view) fn render_claude_code_activity_bar(
        &self,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let thread = self.thread.read(cx);
        let action_log = thread.action_log();
        let telemetry = ActionLogTelemetry::from(thread);
        let changed_buffers = action_log.read(cx).changed_buffers(cx).collect::<Vec<_>>();
        let plan = thread.plan().filter(|plan| !plan.is_empty(cx));
        let has_plan = plan.is_some();
        let has_changed_buffers = !changed_buffers.is_empty();
        let has_queue = self.has_queued_messages();

        // The permission card in the prompt box's place answers these requests.
        let awaiting_permission = if self.claude_code_permission_prompt(cx).is_some() {
            None
        } else {
            let awaiting_permission = self
                .render_main_agent_awaiting_permission(window, cx)
                .or_else(|| self.render_subagents_awaiting_permission(cx));
            match (self.render_generic_permissions(cx), awaiting_permission) {
                (Some(generic), Some(legacy)) => Some(
                    v_flex()
                        .child(generic)
                        .child(Divider::horizontal().color(DividerColor::Border))
                        .child(legacy)
                        .into_any(),
                ),
                (generic, legacy) => generic.or(legacy),
            }
        };
        let has_awaiting_permission = awaiting_permission.is_some();

        if !has_changed_buffers && !has_plan && !has_queue && !has_awaiting_permission {
            return None;
        }

        // Mirrors `render_activity_bar`, which keeps ACP edit controls enabled.
        let pending_edits = false;
        let colors = cx.theme().colors();

        let activity_box = v_flex()
            .id("claude-code-activity-bar")
            .debug_selector(|| "claude-code-activity-bar".into())
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .rounded(PROMPT_BOX_RADIUS)
            .border_1()
            .border_color(colors.border)
            .bg(self.activity_bar_bg(cx))
            .block_mouse_except_scroll()
            .when_some(awaiting_permission, |this, element| this.child(element))
            .when(
                has_awaiting_permission && (has_plan || has_changed_buffers || has_queue),
                |this| this.child(Divider::horizontal().color(DividerColor::Border)),
            )
            .when_some(plan, |this, plan| {
                this.child(self.render_plan_summary(plan, window, cx))
                    .when(self.plan_expanded, |this| {
                        this.children(self.render_plan_body(plan, window, cx))
                    })
            })
            .when(has_plan && has_changed_buffers, |this| {
                this.child(Divider::horizontal().color(DividerColor::Border))
            })
            .when(
                has_changed_buffers && thread.parent_session_id().is_none(),
                |this| {
                    this.child(self.render_edits_summary(
                        &changed_buffers,
                        self.edits_expanded,
                        pending_edits,
                        cx,
                    ))
                    .when(self.edits_expanded, |this| {
                        this.child(self.render_edited_files(
                            action_log,
                            telemetry.clone(),
                            &changed_buffers,
                            pending_edits,
                            cx,
                        ))
                    })
                },
            )
            .when(has_queue, |this| {
                this.when(has_plan || has_changed_buffers, |this| {
                    this.child(Divider::horizontal().color(DividerColor::Border))
                })
                .child(self.render_message_queue_summary(window, cx))
                .when(self.queue_expanded, |this| {
                    this.child(self.render_message_queue_entries(window, cx))
                })
            });

        Some(activity_box.into_any_element())
    }

    fn claude_code_config_options(&self, cx: &App) -> Option<ConfigOptionsHandle> {
        let view = self.config_options_view.as_ref()?.read(cx);
        Some(ConfigOptionsHandle {
            config_options: view.config_options.clone(),
            agent_server: view.agent_server.clone(),
            fs: view.fs.clone(),
        })
    }

    /// Claude Code shows "Queue another message…" while a turn runs and names the
    /// focus shortcut otherwise. Zed's own placeholder is remembered so switching
    /// styles back restores it.
    fn sync_claude_code_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let is_running = matches!(
            self.thread.read(cx).status(),
            ThreadStatus::Generating | ThreadStatus::Unconfirmed
        );
        let placeholder = if is_running {
            QUEUE_PLACEHOLDER
        } else {
            idle_placeholder()
        };
        let editor = self.message_editor.read(cx).editor().clone();
        let current = editor.update(cx, |editor, cx| editor.placeholder_text(cx));
        if current.as_deref() == Some(placeholder) {
            return;
        }
        if let Some(current) = current.filter(|current| !is_claude_code_placeholder(current)) {
            remember_zed_placeholder(cx.entity_id(), current, cx);
        }
        self.message_editor.update(cx, |message_editor, cx| {
            message_editor.set_placeholder_text(placeholder, window, cx);
        });
    }

    /// The extension's command button: a slash in a small outlined square.
    fn render_claude_code_slash_button(&self, cx: &Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        div()
            .id("claude-code-slash-commands")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(24.))
            .rounded(px(5.))
            .cursor_pointer()
            .hover(|style| style.bg(colors.ghost_element_hover))
            .tooltip(Tooltip::text("Commands"))
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_claude_code_slash_menu(window, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(15.))
                    .rounded(px(3.))
                    .border_1()
                    .border_color(colors.icon_muted)
                    .child(
                        Icon::new(IconName::Slash)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    ),
            )
    }

    /// Types a `/` where a slash command can start and opens the completions,
    /// like typing it in the editor would.
    pub(in crate::conversation_view::thread_view) fn open_claude_code_slash_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = self.message_editor.read(cx).editor().clone();
        window.focus(&editor.focus_handle(cx), cx);
        cx.spawn_in(window, async move |_, cx| {
            editor
                .update_in(cx, |editor, window, cx| {
                    let menu_is_open =
                        editor.context_menu().borrow().as_ref().is_some_and(|menu| {
                            matches!(menu, CodeContextMenu::Completions(_)) && menu.visible()
                        });
                    let previous_character = {
                        let snapshot = editor.display_snapshot(cx);
                        let cursor = editor.selections.newest::<text::Point>(&snapshot).head();
                        let offset = cursor.to_offset(&snapshot);
                        snapshot.buffer_snapshot().reversed_chars_at(offset).next()
                    };
                    if menu_is_open && previous_character == Some('/') {
                        return;
                    }
                    let prefix = match previous_character {
                        Some(character) if !character.is_whitespace() => " /",
                        _ => "/",
                    };
                    editor.insert(prefix, window, cx);
                    editor.show_completions(&editor::actions::ShowCompletions, window, cx);
                })
                .log_err();
        })
        .detach();
    }

    /// The model pill: the model, its effort level muted, and "· Ultracode" when on.
    /// It opens Claude Code's model picker above the prompt box.
    fn render_claude_code_model_chip(
        &self,
        config_options: &ConfigOptionsHandle,
        cx: &Context<Self>,
    ) -> Option<impl IntoElement> {
        let options = config_options.options();
        let parts = model_chip_parts(&options)?;
        let is_read_only = config_options.is_read_only();
        let config_options = config_options.clone();
        Some(
            render_pill("claude-code-model-chip", cx)
                .tooltip(Tooltip::text("Switch model"))
                .when(!is_read_only, |this| {
                    this.on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_claude_code_model_picker(config_options.clone(), window, cx);
                    }))
                })
                .when_some(parts.model, |this, model| {
                    this.child(Label::new(model).size(LabelSize::Small))
                })
                .when_some(parts.effort, |this, effort| {
                    this.child(
                        Label::new(effort)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                })
                .when(parts.ultracode, |this| {
                    this.child(
                        Label::new("· Ultracode")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                }),
        )
    }

    fn toggle_claude_code_model_picker(
        &mut self,
        config_options: ConfigOptionsHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let thread_view_id = cx.entity_id();
        let activity = thread_activity(thread_view_id, cx);
        // The click that dismissed the picker also lands here; don't reopen it.
        let just_dismissed = activity
            .and_then(|activity| activity.model_picker_dismissed_at())
            .is_some_and(|dismissed_at| dismissed_at.elapsed().as_millis() < 300);
        if activity.is_some_and(|activity| activity.model_picker().is_some()) || just_dismissed {
            set_model_picker(thread_view_id, None, cx);
            cx.notify();
            return;
        }
        let picker = cx.new(|cx| ClaudeCodeModelPicker::new(config_options, cx));
        cx.subscribe_in(
            &picker,
            window,
            move |this, _, _: &gpui::DismissEvent, window, cx| {
                set_model_picker(thread_view_id, None, cx);
                let editor_focus = this.message_editor.focus_handle(cx);
                window.focus(&editor_focus, cx);
                cx.notify();
            },
        )
        .detach();
        window.focus(&picker.focus_handle(cx), cx);
        set_model_picker(thread_view_id, Some(picker.into()), cx);
        cx.notify();
    }

    /// "● N agents": shown once the session has subagents, counting the ones still
    /// working, with a green dot while any works. Opens the agent map.
    fn render_claude_code_agents_chip(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        let agents = agent_summaries(
            self.thread.read(cx),
            thread_activity(cx.entity_id(), cx),
            Instant::now(),
            cx,
        );
        if agents.is_empty() {
            return None;
        }
        let dot = agents_dot_state(&agents);
        Some(
            render_pill("claude-code-agents-chip", cx)
                .gap_1()
                .tooltip(Tooltip::text(agents_chip_tooltip(dot)))
                .on_click(
                    cx.listener(|this, _, window, cx| this.open_claude_code_agent_map(window, cx)),
                )
                .child(StatusDot::new(dot).size(px(6.)))
                .child(
                    Label::new(agent_count_label(live_agent_count(&agents))).size(LabelSize::Small),
                ),
        )
    }

    fn open_claude_code_agent_map(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let model_name = self
            .claude_code_config_options(cx)
            .and_then(|config_options| {
                model_chip_parts(&config_options.options()).and_then(|parts| parts.model)
            });
        let thread = self.thread.clone();
        let thread_view_id = cx.entity_id();
        // The workspace may be updating this view; open the modal once it is done.
        window.defer(cx, move |window, cx| {
            open_agent_map(&workspace, thread, thread_view_id, model_name, window, cx);
        });
    }

    fn render_claude_code_mode_chip(
        &self,
        config_options: &ConfigOptionsHandle,
    ) -> Option<impl IntoElement> {
        let options = config_options.options();
        let mode_option =
            first_select_option(&options, &acp_v2::SessionConfigOptionCategory::Mode)?.clone();
        let acp_v2::SessionConfigKind::Select(select) = &mode_option.kind else {
            return None;
        };
        let current_mode = select.current_value.0.clone();
        let adapter_name = select_entries(select)
            .into_iter()
            .find(|entry| entry.value == select.current_value)
            .map(|entry| entry.name)
            .unwrap_or_else(|| current_mode.to_string());
        let is_read_only = config_options.config_options.is_read_only();
        let trigger = Button::new(
            "claude-code-mode-chip",
            mode_display_name(&current_mode, &adapter_name),
        )
        .label_size(LabelSize::Small)
        .color(Color::Muted)
        .start_icon(
            Icon::new(mode_icon(&current_mode))
                .size(IconSize::XSmall)
                .color(Color::Muted),
        )
        .disabled(is_read_only);
        let config_options = config_options.clone();
        let config_id = mode_option.config_id.clone();
        Some(
            PopoverMenu::new("claude-code-mode-menu")
                .trigger_with_tooltip(trigger, Tooltip::text("Permission mode"))
                .anchor(gpui::Anchor::BottomRight)
                .offset(gpui::Point {
                    x: px(0.0),
                    y: px(-2.0),
                })
                .menu(move |window, cx| {
                    let config_options = config_options.clone();
                    let config_id = config_id.clone();
                    Some(ContextMenu::build(window, cx, move |menu, _, cx| {
                        build_mode_menu(
                            menu,
                            &config_options,
                            &config_id,
                            documentation_aside_side(cx),
                        )
                    }))
                }),
        )
    }

    fn render_claude_code_send_button(
        &self,
        accent: &PromptAccent,
        cx: &Context<Self>,
    ) -> AnyElement {
        let message_editor = self.message_editor.read(cx);
        let is_editor_empty = message_editor.is_empty(cx);
        let status = self.thread.read(cx).status();
        let is_generating = status == ThreadStatus::Generating;
        let is_unconfirmed = status == ThreadStatus::Unconfirmed;

        if self.is_loading_contents {
            return div()
                .id("loading-message-content")
                .px_1()
                .tooltip(Tooltip::text("Loading Added Context…"))
                .child(loading_contents_spinner(IconSize::default()))
                .into_any_element();
        }

        if (is_generating || is_unconfirmed) && is_editor_empty {
            return IconButton::new("claude-code-stop", IconName::Stop)
                .icon_size(IconSize::Small)
                .icon_color(Color::Accent)
                .style(ButtonStyle::Tinted(TintColor::Accent))
                .tooltip(move |_window, cx| {
                    Tooltip::for_action("Stop", &editor::actions::Cancel, cx)
                })
                .on_click(cx.listener(|this, _event, _, cx| this.cancel_generation(cx)))
                .into_any_element();
        }

        // The extension's send button: filled with the mode's accent, dimmed while
        // there is nothing to send.
        let is_disabled = is_editor_empty && !is_generating;
        div()
            .id("claude-code-send")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(24.))
            .rounded(px(5.))
            .bg(accent.button_background)
            .when(is_disabled, |this| this.opacity(0.4))
            .when(!is_disabled, |this| {
                this.cursor_pointer()
                    .hover(|style| style.opacity(0.9))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.send(window, cx);
                    }))
            })
            .tooltip(move |_window, cx| {
                let label = if is_generating {
                    "Queue message"
                } else {
                    "Send message"
                };
                Tooltip::for_action(label, &Chat, cx)
            })
            .child(
                Icon::new(IconName::ArrowUp)
                    .size(IconSize::Small)
                    .color(Color::Custom(accent.button_foreground)),
            )
            .into_any_element()
    }
}

fn build_mode_menu(
    mut menu: ContextMenu,
    config_options: &ConfigOptionsHandle,
    config_id: &acp_v2::SessionConfigId,
    aside_side: DocumentationSide,
) -> ContextMenu {
    let options = config_options.options();
    let Some(option) = options.iter().find(|option| &option.config_id == config_id) else {
        return menu;
    };
    let acp_v2::SessionConfigKind::Select(select) = &option.kind else {
        return menu;
    };
    let is_read_only = config_options.config_options.is_read_only();
    for entry in select_entries(select) {
        let is_selected = entry.value == select.current_value;
        let label = mode_display_name(&entry.value.0, &entry.name);
        let icon = mode_icon(&entry.value.0);
        let config_options = config_options.clone();
        let config_id = config_id.clone();
        let value = entry.value.clone();
        menu = menu.item(
            ContextMenuEntry::new(label)
                .icon(icon)
                .icon_size(IconSize::XSmall)
                .icon_color(Color::Muted)
                .toggleable(IconPosition::End, is_selected)
                .disabled(is_read_only)
                .when_some(entry.description.clone(), |item, description| {
                    item.documentation_aside(aside_side, move |_| {
                        Label::new(description.clone()).into_any_element()
                    })
                })
                .handler(move |_, cx| {
                    config_options.set(
                        config_id.clone(),
                        acp_v2::SessionConfigOptionValue::id(value.clone()),
                        cx,
                    );
                }),
        );
    }
    menu
}

/// A footer pill, like the extension's model and agent pills.
fn render_pill(id: &'static str, cx: &App) -> Stateful<Div> {
    let colors = cx.theme().colors();
    h_flex()
        .id(id)
        .debug_selector(move || id.to_string())
        .flex_none()
        .gap_1()
        .h(px(20.))
        .px_2()
        .rounded_full()
        .bg(colors.text.opacity(0.1))
        .cursor_pointer()
        .hover(|style| style.bg(colors.text.opacity(0.15)))
}
