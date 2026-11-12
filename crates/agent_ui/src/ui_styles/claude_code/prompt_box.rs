// Declared from `thread.rs` (via a `#[path]` attribute), so this module is a
// descendant of `thread_view` and can reuse `ThreadView`'s private state and
// helpers, such as the add-context menu and the token usage ring.

use std::rc::Rc;

use acp_thread::AgentSessionConfigOptions;
use agent_servers::AgentServer;
use editor::ToOffset as _;
use editor::code_context_menus::CodeContextMenu;
use fs::Fs;
use ui::DocumentationSide;

use super::*;
use crate::config_options::set_config_option_and_default;
use crate::ui::documentation_aside_side;
use crate::ui_styles::claude_code::{
    QUEUE_PLACEHOLDER, effort_display_name, first_select_option, idle_placeholder,
    is_claude_code_placeholder, mode_display_name, mode_icon, model_chip_label,
    remember_zed_placeholder, select_entries, toggle_option_state, toggled_option_value,
};

pub(super) const PROMPT_BOX_RADIUS: Pixels = px(8.);

/// What the chips need to read and change the session's config options.
#[derive(Clone)]
struct ConfigOptionsHandle {
    config_options: Rc<dyn AgentSessionConfigOptions>,
    agent_server: Rc<dyn AgentServer>,
    fs: Arc<dyn Fs>,
}

impl ConfigOptionsHandle {
    fn options(&self) -> Vec<acp_v2::SessionConfigOption> {
        self.config_options.config_options()
    }

    fn set(
        &self,
        config_id: acp_v2::SessionConfigId,
        value: acp_v2::SessionConfigOptionValue,
        cx: &mut App,
    ) {
        set_config_option_and_default(
            self.config_options.clone(),
            self.agent_server.clone(),
            config_id,
            value,
            self.fs.clone(),
            cx,
        )
        .detach_and_log_err(cx);
    }
}

impl ThreadView {
    /// The Claude Code prompt box: a rounded, bordered box at the bottom of the
    /// chat column with the message editor on top and a footer of `+`, `/`, the
    /// model chip and the context indicator on the left, and the mode chip and
    /// the send/stop button on the right.
    pub(in crate::conversation_view::thread_view) fn render_claude_code_message_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.sync_claude_code_placeholder(window, cx);

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

        let editor_background = cx.theme().colors().editor_background;
        let border_color = if self
            .message_editor
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            cx.theme().colors().border_focused
        } else {
            cx.theme().colors().border
        };
        let max_content_width = AgentSettings::get_global(cx).max_content_width;
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

        let config_options = self.claude_code_config_options(cx);
        let model_chip = match &config_options {
            Some(config_options) => self
                .render_claude_code_model_chip(config_options)
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
            .gap_1()
            .px_1p5()
            .pb_1p5()
            .child(
                h_flex()
                    .min_w_0()
                    .flex_wrap()
                    .gap_0p5()
                    .child(self.render_add_context_button(cx))
                    .child(self.render_claude_code_slash_button(cx))
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
                    .child(self.render_claude_code_send_button(cx)),
            );

        let prompt_box = v_flex()
            .id("claude-code-prompt-box")
            .debug_selector(|| "claude-code-prompt-box".into())
            .w_full()
            .min_w_0()
            .when(editor_expanded, |this| this.h(vh(0.8, window)))
            .rounded(PROMPT_BOX_RADIUS)
            .border_1()
            .border_color(border_color)
            .bg(editor_background)
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .when(editor_expanded, |this| this.flex_1().min_h_0())
                    .px_3()
                    .pt_2p5()
                    .pb_1()
                    .child(self.message_editor.clone()),
            )
            .child(footer);

        h_flex()
            .w_full()
            .flex_none()
            .justify_center()
            .px_3()
            .pt_1()
            .pb_3()
            .bg(editor_background)
            .on_action(cx.listener(Self::handle_message_editor_move_up))
            .on_action(cx.listener(Self::expand_message_editor))
            .child(
                div()
                    .when_some(max_content_width, |this, max_width| {
                        this.flex_basis(max_width)
                    })
                    .when(max_content_width.is_none(), |this| this.w_full())
                    .min_w_0()
                    .flex_shrink_1()
                    .flex_grow_0()
                    .child(prompt_box),
            )
            .into_any_element()
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
        let max_content_width = AgentSettings::get_global(cx).max_content_width;
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

        Some(
            h_flex()
                .w_full()
                .flex_none()
                .justify_center()
                .px_3()
                .pt_1()
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
                        .child(activity_box),
                )
                .into_any_element(),
        )
    }

    fn claude_code_config_options(&self, cx: &App) -> Option<ConfigOptionsHandle> {
        let view = self.config_options_view.as_ref()?.read(cx);
        Some(ConfigOptionsHandle {
            config_options: view.config_options.clone(),
            agent_server: view.agent_server.clone(),
            fs: view.fs.clone(),
        })
    }

    /// Claude Code shows "Queue another message…" while a turn runs. The idle
    /// placeholder isn't documented, so a short neutral one is used. Zed's own
    /// placeholder is remembered so switching styles back restores it.
    fn sync_claude_code_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let is_running = matches!(
            self.thread.read(cx).status(),
            ThreadStatus::Generating | ThreadStatus::Unconfirmed
        );
        let placeholder = if is_running {
            QUEUE_PLACEHOLDER.to_string()
        } else {
            idle_placeholder(&self.agent_display_name)
        };
        let editor = self.message_editor.read(cx).editor().clone();
        let current = editor.update(cx, |editor, cx| editor.placeholder_text(cx));
        if current.as_deref() == Some(placeholder.as_str()) {
            return;
        }
        if let Some(current) = current.filter(|current| !is_claude_code_placeholder(current)) {
            remember_zed_placeholder(cx.entity_id(), current, cx);
        }
        self.message_editor.update(cx, |message_editor, cx| {
            message_editor.set_placeholder_text(&placeholder, window, cx);
        });
    }

    fn render_claude_code_slash_button(&self, cx: &Context<Self>) -> impl IntoElement {
        IconButton::new("claude-code-slash-commands", IconName::Slash)
            .icon_size(IconSize::Small)
            .icon_color(Color::Muted)
            .tooltip(Tooltip::text("Commands"))
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_claude_code_slash_menu(window, cx);
            }))
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

    fn render_claude_code_model_chip(
        &self,
        config_options: &ConfigOptionsHandle,
    ) -> Option<impl IntoElement> {
        let options = config_options.options();
        let label = model_chip_label(&options)?;
        let is_read_only = config_options.config_options.is_read_only();
        let trigger = Button::new("claude-code-model-chip", label)
            .label_size(LabelSize::Small)
            .color(Color::Muted)
            .disabled(is_read_only);
        let config_options = config_options.clone();
        Some(
            PopoverMenu::new("claude-code-model-menu")
                .trigger_with_tooltip(trigger, Tooltip::text("Switch model"))
                .anchor(gpui::Anchor::BottomLeft)
                .offset(gpui::Point {
                    x: px(0.0),
                    y: px(-2.0),
                })
                .menu(move |window, cx| {
                    let config_options = config_options.clone();
                    Some(ContextMenu::build(window, cx, move |menu, _, cx| {
                        build_model_menu(menu, &config_options, documentation_aside_side(cx))
                    }))
                }),
        )
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

    fn render_claude_code_send_button(&self, cx: &Context<Self>) -> AnyElement {
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

        let is_disabled = is_editor_empty && !is_generating;
        IconButton::new("claude-code-send", IconName::ArrowUp)
            .icon_size(IconSize::Small)
            .style(ButtonStyle::Filled)
            .map(|this| {
                if is_disabled {
                    this.disabled(true).icon_color(Color::Muted)
                } else {
                    this.icon_color(Color::Accent)
                }
            })
            .tooltip(move |_window, cx| {
                let label = if is_generating {
                    "Queue message"
                } else {
                    "Send message"
                };
                Tooltip::for_action(label, &Chat, cx)
            })
            .on_click(cx.listener(|this, _, window, cx| {
                this.send(window, cx);
            }))
            .into_any_element()
    }
}

/// The model chip's menu: the models, then the Effort levels, then the
/// model-related switches (Fast mode, Ultracode), like Claude Code's model
/// picker. Remaining options without a known place get their own section so
/// nothing the adapter offers becomes unreachable.
fn build_model_menu(
    mut menu: ContextMenu,
    config_options: &ConfigOptionsHandle,
    aside_side: DocumentationSide,
) -> ContextMenu {
    let options = config_options.options();
    let is_read_only = config_options.config_options.is_read_only();
    let mut sections = 0;

    let model = first_select_option(&options, &acp_v2::SessionConfigOptionCategory::Model);
    let effort = first_select_option(&options, &acp_v2::SessionConfigOptionCategory::ThoughtLevel);

    for (option, title) in [(model, "Model"), (effort, "Effort")] {
        let Some(option) = option else {
            continue;
        };
        let acp_v2::SessionConfigKind::Select(select) = &option.kind else {
            continue;
        };
        if sections > 0 {
            menu = menu.separator();
        }
        sections += 1;
        menu = menu.header(title);
        let is_effort = title == "Effort";
        for entry in select_entries(select) {
            let is_selected = entry.value == select.current_value;
            let label = if is_effort {
                effort_display_name(&entry)
            } else {
                SharedString::from(entry.name.clone())
            };
            let config_options = config_options.clone();
            let config_id = option.config_id.clone();
            let value = entry.value.clone();
            menu = menu.item(
                ContextMenuEntry::new(label)
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
    }

    let switches = options
        .iter()
        .filter(|option| {
            matches!(
                option.category,
                Some(acp_v2::SessionConfigOptionCategory::ModelConfig)
                    | Some(acp_v2::SessionConfigOptionCategory::ThoughtLevel)
            )
        })
        .filter_map(|option| Some((option, toggle_option_state(option)?)))
        .collect::<Vec<_>>();
    if !switches.is_empty() {
        if sections > 0 {
            menu = menu.separator();
        }
        sections += 1;
        for (option, enabled) in switches {
            let config_options = config_options.clone();
            let toggled = option.clone();
            menu = menu.item(
                ContextMenuEntry::new(option.name.clone())
                    .toggleable(IconPosition::End, enabled)
                    .disabled(is_read_only)
                    .handler(move |_, cx| {
                        if let Some(value) = toggled_option_value(&toggled, !enabled) {
                            config_options.set(toggled.config_id.clone(), value, cx);
                        }
                    }),
            );
        }
    }

    for option in options
        .iter()
        .filter(|option| option.category.is_none() && toggle_option_state(option).is_none())
    {
        let acp_v2::SessionConfigKind::Select(select) = &option.kind else {
            continue;
        };
        if sections > 0 {
            menu = menu.separator();
        }
        sections += 1;
        menu = menu.header(option.name.clone());
        for entry in select_entries(select) {
            let is_selected = entry.value == select.current_value;
            let config_options = config_options.clone();
            let config_id = option.config_id.clone();
            let value = entry.value.clone();
            menu = menu.item(
                ContextMenuEntry::new(entry.name.clone())
                    .toggleable(IconPosition::End, is_selected)
                    .disabled(is_read_only)
                    .handler(move |_, cx| {
                        config_options.set(
                            config_id.clone(),
                            acp_v2::SessionConfigOptionValue::id(value.clone()),
                            cx,
                        );
                    }),
            );
        }
    }

    menu
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
