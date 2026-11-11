use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, Hsla, SharedString, Window, hsla,
    linear_color_stop, linear_gradient, px,
};
use itertools::Itertools as _;
use ui::prelude::*;

use super::{StepStatus, TIMELINE_GUTTER, TimelineDot, timeline_row};

const IO_LABEL_WIDTH: Pixels = px(36.);
const COLLAPSED_OUTPUT_LINES: usize = 3;
const CLAMPED_OUTPUT_MAX_HEIGHT: Pixels = px(64.);
/// Caps how much output an expanded row lays out; the full output stays
/// available to the agent and in the thread's markdown export.
const MAX_EXPANDED_OUTPUT_LINES: usize = 500;

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A compact tool call step: a status dot, the tool name in bold followed by
/// a muted description, and an optional bordered IN/OUT block whose output is
/// clamped to a few lines until expanded.
///
/// It only takes plain data so it can render tool calls from any agent.
#[derive(IntoElement)]
pub(crate) struct ToolRow {
    id: ElementId,
    name: SharedString,
    description: Option<SharedString>,
    status: StepStatus,
    header_suffix: Option<AnyElement>,
    on_header_click: Option<ClickHandler>,
    input: Option<SharedString>,
    output: Option<SharedString>,
    output_expanded: bool,
    on_toggle_output: Option<ClickHandler>,
    body: Option<AnyElement>,
    footer: Vec<AnyElement>,
}

impl ToolRow {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        name: impl Into<SharedString>,
        status: StepStatus,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: None,
            status,
            header_suffix: None,
            on_header_click: None,
            input: None,
            output: None,
            output_expanded: false,
            on_toggle_output: None,
            body: None,
            footer: Vec::new(),
        }
    }

    pub(crate) fn description(mut self, description: Option<SharedString>) -> Self {
        self.description = description;
        self
    }

    pub(crate) fn header_suffix(mut self, suffix: impl IntoElement) -> Self {
        self.header_suffix = Some(suffix.into_any_element());
        self
    }

    pub(crate) fn on_header_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_header_click = Some(Rc::new(handler));
        self
    }

    pub(crate) fn input(mut self, input: Option<SharedString>) -> Self {
        self.input = input;
        self
    }

    pub(crate) fn output(mut self, output: Option<SharedString>) -> Self {
        self.output = output;
        self
    }

    pub(crate) fn output_expanded(mut self, expanded: bool) -> Self {
        self.output_expanded = expanded;
        self
    }

    pub(crate) fn on_toggle_output(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle_output = Some(Rc::new(handler));
        self
    }

    /// Extra content shown under the header, such as a diff.
    pub(crate) fn body(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_any_element());
        self
    }

    /// Content shown below everything else, such as permission buttons.
    pub(crate) fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer.push(footer.into_any_element());
        self
    }
}

fn io_label(label: &'static str) -> impl IntoElement {
    div().flex_none().w(IO_LABEL_WIDTH).child(
        Label::new(label)
            .size(LabelSize::XSmall)
            .color(Color::Muted),
    )
}

fn io_text(text: SharedString, cx: &App) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
        .font_buffer(cx)
        .text_xs()
        .text_color(cx.theme().colors().text)
        .child(text)
}

fn output_background(cx: &App) -> Hsla {
    cx.theme()
        .colors()
        .editor_background
        .blend(hsla(0., 0., 0., 0.12))
}

impl RenderOnce for ToolRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let border_color = cx.theme().colors().border_variant;

        let header = h_flex()
            .id("header")
            .min_w_0()
            .gap_1p5()
            .child(Label::new(self.name).weight(FontWeight::BOLD).single_line())
            .when_some(self.description, |this, description| {
                this.child(
                    div().min_w_0().overflow_hidden().child(
                        Label::new(description)
                            .color(Color::Muted)
                            .single_line()
                            .truncate(),
                    ),
                )
            })
            .children(self.header_suffix)
            .when_some(self.on_header_click, |this, handler| {
                this.cursor_pointer()
                    .on_click(move |event, window, cx| handler(event, window, cx))
            });

        let output_line_count = self
            .output
            .as_ref()
            .map(|output| output.lines().count())
            .unwrap_or(0);
        let is_collapsible = output_line_count > COLLAPSED_OUTPUT_LINES;
        let is_clamped = is_collapsible && !self.output_expanded;
        let output_background = output_background(cx);

        let output_row = self.output.map(|output| {
            let visible_output: SharedString = if is_clamped {
                output
                    .lines()
                    .take(COLLAPSED_OUTPUT_LINES)
                    .join("\n")
                    .into()
            } else if output_line_count > MAX_EXPANDED_OUTPUT_LINES {
                format!(
                    "{}\n… {} more lines",
                    output.lines().take(MAX_EXPANDED_OUTPUT_LINES).join("\n"),
                    output_line_count - MAX_EXPANDED_OUTPUT_LINES
                )
                .into()
            } else {
                output
            };

            h_flex()
                .id("output")
                .relative()
                .items_start()
                .px_2p5()
                .py_1p5()
                .bg(output_background)
                .child(io_label("OUT"))
                .child(io_text(visible_output, cx))
                .when(is_clamped, |this| {
                    // Long lines wrap, so clamping by line count alone can still overflow.
                    this.max_h(CLAMPED_OUTPUT_MAX_HEIGHT)
                        .overflow_hidden()
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(px(18.))
                                .bg(linear_gradient(
                                    180.,
                                    linear_color_stop(output_background.opacity(0.), 0.),
                                    linear_color_stop(output_background, 1.),
                                )),
                        )
                })
                .when(is_collapsible, |this| {
                    this.cursor_pointer().when_some(
                        self.on_toggle_output.clone(),
                        |this, handler| {
                            this.on_click(move |event, window, cx| handler(event, window, cx))
                        },
                    )
                })
        });

        let has_io_block = self.input.is_some() || output_row.is_some();
        let has_input = self.input.is_some();

        v_flex()
            .id(self.id.clone())
            .w_full()
            .gap_1()
            .child(timeline_row(
                TimelineDot::new(self.id, self.status),
                header,
                window,
            ))
            .when(has_io_block, |this| {
                this.child(
                    div().pl(TIMELINE_GUTTER).child(
                        v_flex()
                            .w_full()
                            .overflow_hidden()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(border_color)
                            .bg(cx.theme().colors().editor_background)
                            .when_some(self.input, |this, input| {
                                this.child(
                                    h_flex()
                                        .items_start()
                                        .px_2p5()
                                        .py_1p5()
                                        .child(io_label("IN"))
                                        .child(io_text(input, cx)),
                                )
                            })
                            .when_some(output_row, |this, output_row| {
                                this.child(
                                    output_row
                                        .when(has_input, |row| row.border_t_1())
                                        .border_color(border_color),
                                )
                            }),
                    ),
                )
            })
            .when_some(self.body, |this, body| {
                this.child(div().pl(TIMELINE_GUTTER).child(body))
            })
            .children(self.footer)
    }
}
