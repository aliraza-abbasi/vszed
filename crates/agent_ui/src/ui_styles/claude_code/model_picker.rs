use std::rc::Rc;
use std::sync::Arc;

use acp_thread::AgentSessionConfigOptions;
use agent_client_protocol::v2::schema as acp_v2;
use agent_servers::AgentServer;
use fs::Fs;
use gpui::{
    App, BoxShadow, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, Hsla, MouseButton,
    Stateful, Task, point,
};
use ui::{Switch, ToggleState, Tooltip, prelude::*};
use util::ResultExt as _;

use super::{
    ULTRACODE_CONFIG_ID, effort_slider, first_select_option, model_menu_rows, toggle_option_state,
    toggled_option_value,
};
use crate::config_options::set_config_option_and_default;

/// What the prompt box chips need to read and change the session's config options.
#[derive(Clone)]
pub(crate) struct ConfigOptionsHandle {
    pub config_options: Rc<dyn AgentSessionConfigOptions>,
    pub agent_server: Rc<dyn AgentServer>,
    pub fs: Arc<dyn Fs>,
}

impl ConfigOptionsHandle {
    pub(crate) fn options(&self) -> Vec<acp_v2::SessionConfigOption> {
        self.config_options.config_options()
    }

    pub(crate) fn is_read_only(&self) -> bool {
        self.config_options.is_read_only()
    }

    pub(crate) fn set(
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

/// The rows of the picker that the arrow keys move between.
#[derive(Clone, Debug, PartialEq)]
enum PickerItem {
    Model(acp_v2::SessionConfigValueId),
    Effort,
    Ultracode,
}

/// Claude Code's model picker: "Select a model", one two-line row per model with a
/// check on the current one, then the Effort slider and the Ultracode switch. It opens
/// above the prompt box, at the box's width.
pub(crate) struct ClaudeCodeModelPicker {
    config_options: ConfigOptionsHandle,
    focus_handle: FocusHandle,
    highlighted: Option<PickerItem>,
    _refresh: Task<()>,
}

impl EventEmitter<DismissEvent> for ClaudeCodeModelPicker {}

impl Focusable for ClaudeCodeModelPicker {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ClaudeCodeModelPicker {
    pub(crate) fn new(config_options: ConfigOptionsHandle, cx: &mut Context<Self>) -> Self {
        let changes = config_options.config_options.watch(cx);
        let refresh = cx.spawn(async move |this, cx| {
            let Some(mut changes) = changes else {
                return;
            };
            while let Ok(()) = changes.recv().await {
                this.update(cx, |_, cx| cx.notify()).log_err();
            }
        });
        let mut picker = Self {
            config_options,
            focus_handle: cx.focus_handle(),
            highlighted: None,
            _refresh: refresh,
        };
        picker.highlighted = picker.items().into_iter().find(|item| match item {
            PickerItem::Model(value) => picker.current_model().as_ref() == Some(value),
            _ => false,
        });
        picker
    }

    fn current_model(&self) -> Option<acp_v2::SessionConfigValueId> {
        let options = self.config_options.options();
        let model = first_select_option(&options, &acp_v2::SessionConfigOptionCategory::Model)?;
        match &model.kind {
            acp_v2::SessionConfigKind::Select(select) => Some(select.current_value.clone()),
            _ => None,
        }
    }

    fn items(&self) -> Vec<PickerItem> {
        let options = self.config_options.options();
        let mut items = Vec::new();
        if let Some(model) =
            first_select_option(&options, &acp_v2::SessionConfigOptionCategory::Model)
            && let acp_v2::SessionConfigKind::Select(select) = &model.kind
        {
            items.extend(
                model_menu_rows(select)
                    .into_iter()
                    .map(|row| PickerItem::Model(row.value)),
            );
        }
        if first_select_option(&options, &acp_v2::SessionConfigOptionCategory::ThoughtLevel)
            .and_then(effort_slider)
            .is_some()
        {
            items.push(PickerItem::Effort);
        }
        if ultracode_option(&options).is_some() {
            items.push(PickerItem::Ultracode);
        }
        items
    }

    fn select_model(&mut self, value: acp_v2::SessionConfigValueId, cx: &mut Context<Self>) {
        let options = self.config_options.options();
        if let Some(model) =
            first_select_option(&options, &acp_v2::SessionConfigOptionCategory::Model)
            && !self.config_options.is_read_only()
        {
            self.config_options.set(
                model.config_id.clone(),
                acp_v2::SessionConfigOptionValue::id(value),
                cx,
            );
        }
        cx.emit(DismissEvent);
    }

    fn set_effort(&mut self, level: acp_v2::SessionConfigValueId, cx: &mut Context<Self>) {
        let options = self.config_options.options();
        if let Some(effort) =
            first_select_option(&options, &acp_v2::SessionConfigOptionCategory::ThoughtLevel)
            && !self.config_options.is_read_only()
        {
            self.config_options.set(
                effort.config_id.clone(),
                acp_v2::SessionConfigOptionValue::id(level),
                cx,
            );
        }
        self.highlighted = Some(PickerItem::Effort);
        cx.notify();
    }

    /// Clicking the Effort row moves to the next level, wrapping around, like Claude Code.
    fn cycle_effort(&mut self, cx: &mut Context<Self>) {
        let options = self.config_options.options();
        let Some(slider) =
            first_select_option(&options, &acp_v2::SessionConfigOptionCategory::ThoughtLevel)
                .and_then(effort_slider)
        else {
            return;
        };
        let next = slider
            .current
            .map_or(0, |current| (current + 1) % slider.levels.len());
        if let Some(level) = slider.levels.get(next) {
            self.set_effort(level.value.clone(), cx);
        }
    }

    fn toggle_ultracode(&mut self, cx: &mut Context<Self>) {
        let options = self.config_options.options();
        if let Some((option, enabled)) = ultracode_option(&options)
            && !self.config_options.is_read_only()
            && let Some(value) = toggled_option_value(option, !enabled)
        {
            self.config_options.set(option.config_id.clone(), value, cx);
        }
        self.highlighted = Some(PickerItem::Ultracode);
        cx.notify();
    }

    fn move_highlight(&mut self, step: isize, cx: &mut Context<Self>) {
        let items = self.items();
        if items.is_empty() {
            return;
        }
        let count = items.len() as isize;
        let next = match self
            .highlighted
            .as_ref()
            .and_then(|highlighted| items.iter().position(|item| item == highlighted))
        {
            Some(index) => (index as isize + step).rem_euclid(count),
            None if step > 0 => 0,
            None => count - 1,
        };
        self.highlighted = items.get(next as usize).cloned();
        cx.notify();
    }

    fn confirm(&mut self, _: &menu::Confirm, _window: &mut Window, cx: &mut Context<Self>) {
        match self.highlighted.clone() {
            Some(PickerItem::Model(value)) => self.select_model(value, cx),
            Some(PickerItem::Effort) => self.cycle_effort(cx),
            Some(PickerItem::Ultracode) => self.toggle_ultracode(cx),
            None => cx.emit(DismissEvent),
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn select_next(&mut self, _: &menu::SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_highlight(1, cx);
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_highlight(-1, cx);
    }

    fn row_background(&self, item: &PickerItem, cx: &App) -> Option<Hsla> {
        (self.highlighted.as_ref() == Some(item)).then(|| cx.theme().colors().element_hover)
    }
}

fn ultracode_option(
    options: &[acp_v2::SessionConfigOption],
) -> Option<(&acp_v2::SessionConfigOption, bool)> {
    let option = options
        .iter()
        .find(|option| option.config_id.0.as_ref() == ULTRACODE_CONFIG_ID)?;
    Some((option, toggle_option_state(option)?))
}

impl Render for ClaudeCodeModelPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let options = self.config_options.options();
        let colors = cx.theme().colors();
        let rows = first_select_option(&options, &acp_v2::SessionConfigOptionCategory::Model)
            .and_then(|model| match &model.kind {
                acp_v2::SessionConfigKind::Select(select) => Some(model_menu_rows(select)),
                _ => None,
            })
            .unwrap_or_default();
        let effort =
            first_select_option(&options, &acp_v2::SessionConfigOptionCategory::ThoughtLevel)
                .and_then(effort_slider);
        let ultracode = ultracode_option(&options).map(|(_, enabled)| enabled);

        let model_rows = rows.into_iter().enumerate().map(|(index, row)| {
            let item = PickerItem::Model(row.value.clone());
            let background = self.row_background(&item, cx);
            let value = row.value.clone();
            h_flex()
                .id(("claude-code-model-row", index))
                .w_full()
                .px_2()
                .py_1()
                .gap_2()
                .rounded_sm()
                .cursor_pointer()
                .when_some(background, |this, background| this.bg(background))
                .on_mouse_move(cx.listener({
                    let item = item.clone();
                    move |this, _, _, cx| {
                        if this.highlighted.as_ref() != Some(&item) {
                            this.highlighted = Some(item.clone());
                            cx.notify();
                        }
                    }
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_model(value.clone(), cx);
                }))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(Label::new(row.title).truncate())
                        .when_some(row.subtitle, |this, subtitle| {
                            this.child(
                                Label::new(subtitle)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .truncate(),
                            )
                        }),
                )
                .when(row.is_current, |this| {
                    this.child(
                        Icon::new(IconName::Check)
                            .size(IconSize::Small)
                            .color(Color::Default),
                    )
                })
        });

        let effort_row = effort.map(|slider| {
            let background = self.row_background(&PickerItem::Effort, cx);
            let levels = slider.levels.clone();
            let position = slider.current.unwrap_or(0);
            h_flex()
                .id("claude-code-effort-row")
                .w_full()
                .px_2()
                .py_1()
                .gap_2()
                .justify_between()
                .rounded_sm()
                .cursor_pointer()
                .tooltip(Tooltip::text("Set how hard the model tries"))
                .when_some(background, |this, background| this.bg(background))
                .on_mouse_move(cx.listener(|this, _, _, cx| {
                    if this.highlighted != Some(PickerItem::Effort) {
                        this.highlighted = Some(PickerItem::Effort);
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(|this, _, _, cx| this.cycle_effort(cx)))
                .child(
                    h_flex()
                        .gap_1()
                        .child(Label::new("Effort"))
                        .child(Label::new(format!("({})", slider.label)).color(Color::Muted)),
                )
                .child(
                    render_effort_slider(levels.len(), position, cx).child(
                        h_flex()
                            .absolute()
                            .inset_0()
                            .children(levels.into_iter().enumerate().map(|(index, level)| {
                                div()
                                    .id(("claude-code-effort-level", index))
                                    .flex_1()
                                    .h_full()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.set_effort(level.value.clone(), cx);
                                    }))
                            })),
                    ),
                )
        });

        let ultracode_row = ultracode.map(|enabled| {
            let background = self.row_background(&PickerItem::Ultracode, cx);
            h_flex()
                .id("claude-code-ultracode-row")
                .w_full()
                .px_2()
                .py_1()
                .gap_2()
                .justify_between()
                .rounded_sm()
                .cursor_pointer()
                .tooltip(Tooltip::text(
                    "Run dynamic workflows on every task (this session only)",
                ))
                .when_some(background, |this, background| this.bg(background))
                .on_mouse_move(cx.listener(|this, _, _, cx| {
                    if this.highlighted != Some(PickerItem::Ultracode) {
                        this.highlighted = Some(PickerItem::Ultracode);
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_ultracode(cx)))
                .child(Label::new("Ultracode"))
                .child(
                    Switch::new("claude-code-ultracode-switch", ToggleState::from(enabled))
                        .on_click(cx.listener(|this, _, _, cx| {
                            // The row would toggle it back.
                            cx.stop_propagation();
                            this.toggle_ultracode(cx);
                        })),
                )
        });

        v_flex()
            .id("claude-code-model-picker")
            .key_context("menu")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .w_full()
            .p_1()
            .rounded(px(8.))
            .border_1()
            .border_color(colors.border)
            .bg(colors.elevated_surface_background)
            .shadow(vec![BoxShadow {
                color: gpui::black().opacity(0.25),
                offset: point(px(0.), px(2.)),
                blur_radius: px(8.),
                spread_radius: px(0.),
                inset: false,
            }])
            .child(
                div().px_2().pt_0p5().pb_1().child(
                    Label::new("Select a model")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
            )
            .children(model_rows)
            .when(effort_row.is_some() || ultracode_row.is_some(), |this| {
                this.child(div().h(px(6.)))
            })
            .children(effort_row)
            .children(ultracode_row)
    }
}

const EFFORT_TRACK_WIDTH: Pixels = px(76.);
const EFFORT_TRACK_HEIGHT: Pixels = px(18.);
const EFFORT_THUMB_SIZE: Pixels = px(14.);
const EFFORT_THUMB_INSET: Pixels = px(2.);
const EFFORT_NOTCH_SIZE: Pixels = px(4.);

/// The left edge of the thumb at `fraction` (0 = first level, 1 = last level) of the
/// track, as the extension's effort slider places it.
fn effort_thumb_left(fraction: f32) -> Pixels {
    EFFORT_THUMB_INSET
        + (EFFORT_TRACK_WIDTH - EFFORT_THUMB_SIZE - EFFORT_THUMB_INSET * 2.) * fraction
}

fn effort_fraction(index: usize, level_count: usize) -> f32 {
    if level_count > 1 {
        index as f32 / (level_count - 1) as f32
    } else {
        0.
    }
}

/// The extension's effort slider: a rounded track with a dot per level, filled up to
/// the thumb at the current level.
fn render_effort_slider(level_count: usize, position: usize, cx: &App) -> Stateful<Div> {
    let colors = cx.theme().colors();
    let fraction = effort_fraction(position, level_count);
    let thumb_left = effort_thumb_left(fraction);
    div()
        .id("claude-code-effort-slider")
        .relative()
        .flex_none()
        .w(EFFORT_TRACK_WIDTH)
        .h(EFFORT_TRACK_HEIGHT)
        .rounded_full()
        .overflow_hidden()
        .bg(colors.border)
        .tooltip(Tooltip::text("Click or drag to set effort level"))
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(thumb_left + EFFORT_THUMB_SIZE + EFFORT_THUMB_INSET)
                .rounded_full()
                .bg(colors.border_focused),
        )
        .children((0..level_count).map(|index| {
            let center =
                effort_thumb_left(effort_fraction(index, level_count)) + EFFORT_THUMB_SIZE / 2.;
            div()
                .absolute()
                .top((EFFORT_TRACK_HEIGHT - EFFORT_NOTCH_SIZE) / 2.)
                .left(center - EFFORT_NOTCH_SIZE / 2.)
                .size(EFFORT_NOTCH_SIZE)
                .rounded_full()
                .bg(colors.text_muted.opacity(0.4))
        }))
        .child(
            div()
                .absolute()
                .top(EFFORT_THUMB_INSET)
                .left(thumb_left)
                .size(EFFORT_THUMB_SIZE)
                .rounded_full()
                .bg(colors.text)
                .shadow(vec![BoxShadow {
                    color: gpui::black().opacity(0.2),
                    offset: point(px(0.), px(1.)),
                    blur_radius: px(2.),
                    spread_radius: px(0.),
                    inset: false,
                }]),
        )
}
