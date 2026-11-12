use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, Hsla, SharedString, Window, hsla,
    linear_color_stop, linear_gradient, px,
};
use ui::{CopyButton, Tooltip, prelude::*};

use super::{StepStatus, TIMELINE_GUTTER, TimelineDot, display_lines, timeline_row};

const IO_LABEL_WIDTH: Pixels = px(36.);
/// Caps how much input a row lays out.
const MAX_INPUT_LINES: usize = 500;
/// Claude Code clips OUT to 60px, fading out its last 10px.
const OUTPUT_MAX_HEIGHT: Pixels = px(60.);
const OUTPUT_FADE_HEIGHT: Pixels = px(10.);
/// More lines than fit in `OUTPUT_MAX_HEIGHT` at any font size Zed allows.
const MAX_PREVIEW_LINES: usize = 8;
/// OUT lines don't wrap, so laying out more than a screen's width of one is
/// wasted work.
const MAX_PREVIEW_CHARS: usize = 400;

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A compact tool call step: a status dot, the tool name in bold followed by
/// a muted description, and an optional bordered IN/OUT block. Like Claude
/// Code, OUT shows the first lines unwrapped and clipped; clicking it can open
/// the full output.
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
    copy_input: bool,
    output: Option<SharedString>,
    on_open_output: Option<ClickHandler>,
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
            copy_input: false,
            output: None,
            on_open_output: None,
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

    /// Shows a copy button on the IN row while it is hovered, as Claude Code
    /// does for Bash commands.
    pub(crate) fn copy_input(mut self, copy_input: bool) -> Self {
        self.copy_input = copy_input;
        self
    }

    pub(crate) fn output(mut self, output: Option<SharedString>) -> Self {
        self.output = output;
        self
    }

    pub(crate) fn on_open_output(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open_output = Some(Rc::new(handler));
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

/// One row per line, so that every line starts at the left edge however the
/// text was produced.
fn io_text(lines: Vec<SharedString>, cx: &App) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .font_buffer(cx)
        .text_xs()
        .text_color(cx.theme().colors().text)
        .children(lines.into_iter().map(|line| {
            // An empty row would collapse to zero height.
            let line = if line.is_empty() { " ".into() } else { line };
            div().w_full().min_w_0().child(line)
        }))
}

/// The lines of tool input that a row shows.
pub(crate) fn visible_io_lines(text: &str, max_lines: usize) -> Vec<SharedString> {
    let lines = display_lines(text);
    let line_count = lines.len();
    let mut visible = lines
        .into_iter()
        .take(max_lines)
        .map(SharedString::from)
        .collect::<Vec<_>>();
    if line_count > max_lines {
        visible.push(format!("… {} more lines", line_count - max_lines).into());
    }
    visible
}

/// The lines OUT lays out before clipping, each cut to what can be visible.
pub(crate) fn output_preview_lines(output: &str) -> Vec<SharedString> {
    display_lines(output)
        .into_iter()
        .take(MAX_PREVIEW_LINES)
        .map(|line| match line.char_indices().nth(MAX_PREVIEW_CHARS) {
            Some((byte_index, _)) => line[..byte_index].to_string().into(),
            None => line.into(),
        })
        .collect()
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

        let output_background = output_background(cx);
        let output_selector = format!("{}-output", self.id);
        let input_hover_group = SharedString::from(format!("{}-input", self.id));
        let output_row = self.output.map(|output| {
            let lines = output_preview_lines(&output);
            let may_overflow = lines.len() > 3;
            h_flex()
                .id("output")
                .debug_selector(move || output_selector)
                .relative()
                .items_start()
                .max_h(OUTPUT_MAX_HEIGHT)
                .overflow_hidden()
                .px_2p5()
                .py_1p5()
                .bg(output_background)
                .child(io_label("OUT"))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .font_buffer(cx)
                        .text_xs()
                        .text_color(cx.theme().colors().text)
                        .children(lines.into_iter().map(|line| {
                            // An empty row would collapse to zero height.
                            let line = if line.is_empty() { " ".into() } else { line };
                            div().whitespace_nowrap().child(line)
                        })),
                )
                .when(may_overflow, |this| {
                    this.child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .h(OUTPUT_FADE_HEIGHT)
                            .bg(linear_gradient(
                                180.,
                                linear_color_stop(output_background.opacity(0.), 0.),
                                linear_color_stop(output_background, 1.),
                            )),
                    )
                })
                .when_some(self.on_open_output.clone(), |this, handler| {
                    let hover_background = cx.theme().colors().element_hover;
                    this.cursor_pointer()
                        .hover(move |style| style.bg(hover_background))
                        .tooltip(Tooltip::text("Open the full text in an editor tab"))
                        .on_click(move |event, window, cx| handler(event, window, cx))
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
                                let hover_group = input_hover_group;
                                let copy_button = self.copy_input.then(|| {
                                    div().absolute().top_1().right_1().child(
                                        CopyButton::new(
                                            SharedString::from(format!("{hover_group}-copy")),
                                            input.clone(),
                                        )
                                        .icon_size(IconSize::Small)
                                        .tooltip_label("Copy")
                                        .visible_on_hover(hover_group.clone()),
                                    )
                                });
                                this.child(
                                    h_flex()
                                        .group(hover_group.clone())
                                        .relative()
                                        .items_start()
                                        .px_2p5()
                                        .py_1p5()
                                        .child(io_label("IN"))
                                        .child(io_text(
                                            visible_io_lines(&input, MAX_INPUT_LINES),
                                            cx,
                                        ))
                                        .children(copy_button),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn as_strings(lines: Vec<SharedString>) -> Vec<String> {
        lines.into_iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn test_output_preview_lines_are_cut_not_wrapped() {
        let output = "    Finished `release` profile\r\n\r\n[exited with code 0]\r\n";
        assert_eq!(
            as_strings(output_preview_lines(output)),
            vec!["    Finished `release` profile", "", "[exited with code 0]"]
        );
        let long_output = (0..20)
            .map(|index| index.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(output_preview_lines(&long_output).len(), MAX_PREVIEW_LINES);
        let long_line = "é".repeat(MAX_PREVIEW_CHARS + 10);
        let lines = output_preview_lines(&long_line);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].chars().count(), MAX_PREVIEW_CHARS);
    }

    #[test]
    fn test_multi_line_input_rows() {
        assert_eq!(
            as_strings(visible_io_lines(
                "git status &&\n\tgit log -1",
                MAX_INPUT_LINES
            )),
            vec!["git status &&", "    git log -1"]
        );
        assert_eq!(
            as_strings(visible_io_lines("a\nb\nc\nd", 2)),
            vec!["a", "b", "… 2 more lines"]
        );
    }
}
