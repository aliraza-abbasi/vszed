// Declared from `thread.rs` (via a `#[path]` attribute), so this module is a
// descendant of `thread_view` and can reuse its code highlighting.

use std::ops::Range;

use gpui::{Bounds, ContentMask, PathBuilder, TextRun, point};
use language::{DiffOptions, line_diff, word_diff_ranges};

use super::*;
use crate::ui_styles::claude_code::{DiffLayout, set_diff_overflows};

/// Claude Code's diff box is at most 200px tall: its content plus 20px.
const DIFF_MAX_HEIGHT: Pixels = px(200.);
const DIFF_BOTTOM_SPACE: Pixels = px(20.);
const DIFF_FADE_HEIGHT: Pixels = px(30.);
const DIFF_GUTTER_WIDTH: Pixels = px(16.);
const HATCH_SPACING: f32 = 6.;
/// Word changes are only looked for in hunks this small, as a diff editor would
/// otherwise give up on them too.
const MAX_WORD_DIFF_LINES: usize = 16;
/// Edits larger than this are laid out without syntax and word highlights.
const MAX_HIGHLIGHTED_DIFF_BYTES: usize = 64 * 1024;

/// A run of lines that is the same on both sides, or a changed run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DiffSegment {
    Unchanged {
        old_start: usize,
        new_start: usize,
        len: usize,
    },
    Changed {
        old: Range<usize>,
        new: Range<usize>,
    },
}

/// Splits two texts' lines into unchanged and changed runs, from the hunks of a
/// line diff. Lines past the last hunk are paired up as unchanged and any lines
/// left over on one side count as changed.
pub(super) fn diff_segments(
    old_line_count: usize,
    new_line_count: usize,
    hunks: &[(Range<u32>, Range<u32>)],
) -> Vec<DiffSegment> {
    let mut segments = Vec::new();
    let mut old_cursor = 0;
    let mut new_cursor = 0;
    let push_unchanged =
        |segments: &mut Vec<DiffSegment>, old_start: usize, new_start: usize, len: usize| {
            if len > 0 {
                segments.push(DiffSegment::Unchanged {
                    old_start,
                    new_start,
                    len,
                });
            }
        };
    for (old_rows, new_rows) in hunks {
        let old_start = (old_rows.start as usize).clamp(old_cursor, old_line_count);
        let old_end = (old_rows.end as usize).clamp(old_start, old_line_count);
        let new_start = (new_rows.start as usize).clamp(new_cursor, new_line_count);
        let new_end = (new_rows.end as usize).clamp(new_start, new_line_count);
        let unchanged = (old_start - old_cursor).min(new_start - new_cursor);
        push_unchanged(&mut segments, old_cursor, new_cursor, unchanged);
        let changed_old = old_cursor + unchanged..old_end;
        let changed_new = new_cursor + unchanged..new_end;
        if !changed_old.is_empty() || !changed_new.is_empty() {
            segments.push(DiffSegment::Changed {
                old: changed_old,
                new: changed_new,
            });
        }
        old_cursor = old_end;
        new_cursor = new_end;
    }
    let unchanged = (old_line_count - old_cursor).min(new_line_count - new_cursor);
    push_unchanged(&mut segments, old_cursor, new_cursor, unchanged);
    let leftover_old = old_cursor + unchanged..old_line_count;
    let leftover_new = new_cursor + unchanged..new_line_count;
    if !leftover_old.is_empty() || !leftover_new.is_empty() {
        segments.push(DiffSegment::Changed {
            old: leftover_old,
            new: leftover_new,
        });
    }
    segments
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DiffLine {
    Unchanged { old: usize, new: usize },
    Removed(usize),
    Added(usize),
}

/// The inline layout: each changed run's removed lines, then its added lines.
pub(super) fn inline_diff_lines(segments: &[DiffSegment]) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    for segment in segments {
        match segment {
            DiffSegment::Unchanged {
                old_start,
                new_start,
                len,
            } => lines.extend((0..*len).map(|offset| DiffLine::Unchanged {
                old: old_start + offset,
                new: new_start + offset,
            })),
            DiffSegment::Changed { old, new } => {
                lines.extend(old.clone().map(DiffLine::Removed));
                lines.extend(new.clone().map(DiffLine::Added));
            }
        }
    }
    lines
}

/// One row of the side-by-side layout. A changed run pairs its removed and added
/// lines; the shorter side is filled in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SplitDiffRow {
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub changed: bool,
}

pub(super) fn split_diff_rows(segments: &[DiffSegment]) -> Vec<SplitDiffRow> {
    let mut rows = Vec::new();
    for segment in segments {
        match segment {
            DiffSegment::Unchanged {
                old_start,
                new_start,
                len,
            } => rows.extend((0..*len).map(|offset| SplitDiffRow {
                old: Some(old_start + offset),
                new: Some(new_start + offset),
                changed: false,
            })),
            DiffSegment::Changed { old, new } => {
                let row_count = old.len().max(new.len());
                rows.extend((0..row_count).map(|offset| SplitDiffRow {
                    old: (offset < old.len()).then(|| old.start + offset),
                    new: (offset < new.len()).then(|| new.start + offset),
                    changed: true,
                }));
            }
        }
    }
    rows
}

/// The rows that differ between two texts' lines, compared without their line
/// endings: the line diff counts a missing final line ending as a change.
fn changed_rows(old_text: &str, new_text: &str) -> Vec<(Range<u32>, Range<u32>)> {
    let with_final_newline = |text: &str| {
        if text.is_empty() || text.ends_with('\n') {
            text.to_string()
        } else {
            format!("{text}\n")
        }
    };
    line_diff(&with_final_newline(old_text), &with_final_newline(new_text))
}

/// The byte range of each line of `text`, without its line ending. Like the line
/// diff, a final line ending doesn't start another line.
fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let end = start + line.len();
        let content = line.trim_end_matches('\n').trim_end_matches('\r');
        ranges.push(start..start + content.len());
        start = end;
    }
    ranges
}

/// The changed words of every changed run, as byte ranges of the whole old and new
/// texts.
fn word_changes(
    old_text: &str,
    new_text: &str,
    old_lines: &[Range<usize>],
    new_lines: &[Range<usize>],
    segments: &[DiffSegment],
) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let mut old_changes = Vec::new();
    let mut new_changes = Vec::new();
    for segment in segments {
        let DiffSegment::Changed { old, new } = segment else {
            continue;
        };
        if old.is_empty()
            || new.is_empty()
            || old.len() > MAX_WORD_DIFF_LINES
            || new.len() > MAX_WORD_DIFF_LINES
        {
            continue;
        }
        let (Some(old_first), Some(old_last), Some(new_first), Some(new_last)) = (
            old_lines.get(old.start),
            old_lines.get(old.end - 1),
            new_lines.get(new.start),
            new_lines.get(new.end - 1),
        ) else {
            continue;
        };
        let old_span = old_first.start..old_last.end;
        let new_span = new_first.start..new_last.end;
        let (Some(old_hunk), Some(new_hunk)) = (
            old_text.get(old_span.clone()),
            new_text.get(new_span.clone()),
        ) else {
            continue;
        };
        let (old_ranges, new_ranges) = word_diff_ranges(old_hunk, new_hunk, DiffOptions::default());
        old_changes.extend(
            old_ranges
                .into_iter()
                .map(|range| range.start + old_span.start..range.end + old_span.start),
        );
        new_changes.extend(
            new_ranges
                .into_iter()
                .map(|range| range.start + new_span.start..range.end + new_span.start),
        );
    }
    (old_changes, new_changes)
}

/// The runs of `runs` (which cover a whole text) that fall in `range`, with a
/// background on the bytes in `highlights`.
fn line_runs(
    runs: &[TextRun],
    range: &Range<usize>,
    highlights: &[Range<usize>],
    highlight_color: Hsla,
) -> Vec<TextRun> {
    let mut boundaries = vec![range.start, range.end];
    let mut run_start = 0;
    for run in runs {
        boundaries.push(run_start);
        run_start += run.len;
    }
    for highlight in highlights {
        boundaries.push(highlight.start);
        boundaries.push(highlight.end);
    }
    boundaries.retain(|boundary| (range.start..=range.end).contains(boundary));
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut line_runs = Vec::new();
    let mut run_index = 0;
    let mut run_start = 0;
    for window in boundaries.windows(2) {
        let [start, end] = [window[0], window[1]];
        while let Some(run) = runs.get(run_index)
            && run_start + run.len <= start
        {
            run_start += run.len;
            run_index += 1;
        }
        let Some(run) = runs.get(run_index) else {
            break;
        };
        let mut piece = run.clone();
        piece.len = end - start;
        if highlights
            .iter()
            .any(|highlight| highlight.start <= start && end <= highlight.end)
        {
            piece.background_color = Some(highlight_color);
        }
        line_runs.push(piece);
    }
    line_runs
}

/// Diagonal hatching where one side of a side-by-side diff has no line, lined up
/// across rows.
fn hatch(color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window, _| {
            let left = f32::from(bounds.left());
            let right = f32::from(bounds.right());
            let top = f32::from(bounds.top());
            let bottom = f32::from(bounds.bottom());
            if !(right > left && bottom > top) {
                return;
            }
            let mut builder = PathBuilder::stroke(px(1.));
            let mut sum = ((left + top) / HATCH_SPACING).floor() * HATCH_SPACING;
            while sum <= right + bottom {
                builder.move_to(point(px(sum - bottom), px(bottom)));
                builder.line_to(point(px(sum - top), px(top)));
                sum += HATCH_SPACING;
            }
            match builder.build() {
                Ok(path) => window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    window.paint_path(path, color)
                }),
                Err(error) => log::debug!("failed to build diff hatching: {error}"),
            }
        },
    )
    .flex_1()
    .min_w_0()
}

struct DiffSide {
    text: SharedString,
    lines: Vec<Range<usize>>,
    runs: Vec<TextRun>,
    word_changes: Vec<Range<usize>>,
}

impl DiffSide {
    fn line(&self, index: usize, highlight_color: Option<Hsla>) -> AnyElement {
        let Some(range) = self.lines.get(index) else {
            return Empty.into_any_element();
        };
        let Some(text) = self.text.get(range.clone()) else {
            return Empty.into_any_element();
        };
        if text.is_empty() {
            // An empty row would collapse to zero height.
            return div().child(" ").into_any_element();
        }
        let highlights = if highlight_color.is_some() {
            self.word_changes.as_slice()
        } else {
            &[]
        };
        let runs = line_runs(
            &self.runs,
            range,
            highlights,
            highlight_color.unwrap_or_default(),
        );
        StyledText::new(SharedString::from(text.to_string()))
            .with_runs(runs)
            .into_any_element()
    }
}

fn gutter(marker: Option<&'static str>, cx: &App) -> impl IntoElement {
    div()
        .flex_none()
        .w(DIFF_GUTTER_WIDTH)
        .text_color(cx.theme().colors().text_muted)
        .child(marker.unwrap_or(" "))
}

impl ThreadView {
    /// The diff under a Claude Code Edit: its old and new text side by side
    /// when there is room, inline otherwise, syntax highlighted, at most 200px tall
    /// with a fade, and clicked open to its full height.
    pub(super) fn render_claude_code_edit_diff(
        &self,
        tool_call_id: &acp_v2::ToolCallId,
        file_path: Option<&str>,
        old_text: &str,
        new_text: &str,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let thread_view_id = cx.entity_id();
        let activity = thread_activity(thread_view_id, cx);
        let layout_cell = activity.map(|activity| activity.diff_layout());
        let layout = layout_cell
            .as_ref()
            .map(|cell| cell.get())
            .unwrap_or_default();
        let overflows = activity.is_some_and(|activity| activity.diff_overflows(tool_call_id));

        // Claude Code picks the layout from the width the diff gets, which is only
        // known once it is laid out.
        let measure = layout_cell.map(|layout_cell| {
            canvas(
                move |bounds, _, cx| {
                    let previous = layout_cell.get();
                    let layout = DiffLayout::for_width(bounds.size.width, previous);
                    if layout != previous {
                        layout_cell.set(layout);
                        cx.defer(move |cx| cx.notify(thread_view_id));
                    }
                },
                |_, _, _, _| {},
            )
            .w_full()
            .h_0()
        });
        if matches!(layout, DiffLayout::Hidden | DiffLayout::Unknown) {
            return div().w_full().children(measure).into_any_element();
        }

        let colors = cx.theme().colors();
        let markdown_style = transcript_markdown_style(window, cx);
        let mut code_style = markdown_style.base_text_style.clone();
        code_style.refine(&markdown_style.code_block.text);

        let is_small = old_text.len() + new_text.len() <= MAX_HIGHLIGHTED_DIFF_BYTES;
        let language = file_path
            .filter(|_| is_small)
            .and_then(|file_path| self.claude_code_diff_language(Path::new(file_path), cx));
        let old_lines = line_ranges(old_text);
        let new_lines = line_ranges(new_text);
        let segments = diff_segments(
            old_lines.len(),
            new_lines.len(),
            &changed_rows(old_text, new_text),
        );
        let (old_word_changes, new_word_changes) = if is_small {
            word_changes(old_text, new_text, &old_lines, &new_lines, &segments)
        } else {
            Default::default()
        };
        let side = |text: &str, lines: Vec<Range<usize>>, word_changes: Vec<Range<usize>>| {
            let runs = if is_small {
                highlight_code_runs(text, language.as_ref(), code_style.clone(), &markdown_style)
            } else {
                vec![code_style.to_run(text.len())]
            };
            DiffSide {
                text: SharedString::from(text.to_string()),
                lines,
                runs,
                word_changes,
            }
        };
        let old_side = side(old_text, old_lines, old_word_changes);
        let new_side = side(new_text, new_lines, new_word_changes);

        let removed_color = colors.version_control_deleted;
        let added_color = colors.version_control_added;
        let removed_background = removed_color.opacity(0.2);
        let added_background = added_color.opacity(0.2);
        let hatch_color = colors.border_variant;

        let rows: Vec<AnyElement> = match layout {
            DiffLayout::SideBySide => split_diff_rows(&segments)
                .into_iter()
                .map(|row| {
                    let half =
                        |line: Option<usize>,
                         diff_side: &DiffSide,
                         marker: &'static str,
                         background: Hsla| {
                            match line {
                                Some(line) => h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .items_start()
                                    .when(row.changed, |this| this.bg(background))
                                    .child(gutter(row.changed.then_some(marker), cx))
                                    .child(div().flex_1().min_w_0().child(
                                        diff_side.line(line, row.changed.then_some(background)),
                                    ))
                                    .into_any_element(),
                                None => hatch(hatch_color).into_any_element(),
                            }
                        };
                    h_flex()
                        .w_full()
                        .items_stretch()
                        .child(half(row.old, &old_side, "-", removed_background))
                        .child(div().flex_none().w_px().bg(colors.border_variant))
                        .child(half(row.new, &new_side, "+", added_background))
                        .into_any_element()
                })
                .collect(),
            _ => inline_diff_lines(&segments)
                .into_iter()
                .map(|line| {
                    let (diff_side, index, marker, background) = match line {
                        DiffLine::Unchanged { new, .. } => (&new_side, new, None, None),
                        DiffLine::Removed(old) => {
                            (&old_side, old, Some("-"), Some(removed_background))
                        }
                        DiffLine::Added(new) => (&new_side, new, Some("+"), Some(added_background)),
                    };
                    h_flex()
                        .w_full()
                        .items_start()
                        .when_some(background, |this, background| this.bg(background))
                        .child(gutter(marker, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(diff_side.line(index, background)),
                        )
                        .into_any_element()
                })
                .collect(),
        };

        let is_expanded = self
            .entry_view_state
            .read(cx)
            .is_tool_call_expanded(tool_call_id);
        let can_toggle = overflows || is_expanded;
        let diff_background = colors.editor_background;
        let overflow_id = tool_call_id.clone();
        let toggle_id = tool_call_id.clone();
        let hover_group = SharedString::from(format!("claude-code-diff-{tool_call_id}"));

        let content = v_flex()
            .w_full()
            .pb(DIFF_BOTTOM_SPACE)
            .font_family(code_style.font_family.clone())
            .text_size(code_style.font_size)
            .line_height(code_style.line_height)
            .text_color(colors.text)
            .children(rows);

        v_flex()
            .w_full()
            .children(measure)
            .child(
                div()
                    .id(hover_group.clone())
                    .debug_selector({
                        let hover_group = hover_group.clone();
                        move || hover_group.to_string()
                    })
                    .group(hover_group.clone())
                    .relative()
                    .w_full()
                    .overflow_hidden()
                    .rounded(px(5.))
                    .border_1()
                    .border_color(colors.border_variant)
                    .bg(diff_background)
                    .when(!is_expanded, |this| this.max_h(DIFF_MAX_HEIGHT))
                    .child(
                        div()
                            .w_full()
                            .on_children_prepainted(move |bounds, _window, cx| {
                                let height = bounds
                                    .first()
                                    .map(|bounds| bounds.size.height)
                                    .unwrap_or_default();
                                set_diff_overflows(
                                    thread_view_id,
                                    &overflow_id,
                                    height > DIFF_MAX_HEIGHT + DIFF_BOTTOM_SPACE,
                                    cx,
                                );
                            })
                            .child(content),
                    )
                    .when(overflows && !is_expanded, |this| {
                        this.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(DIFF_FADE_HEIGHT)
                                .bg(linear_gradient(
                                    180.,
                                    linear_color_stop(diff_background.opacity(0.), 0.),
                                    linear_color_stop(diff_background, 1.),
                                )),
                        )
                    })
                    .when(can_toggle, |this| {
                        let label = if is_expanded {
                            "Click to collapse"
                        } else {
                            "Click to expand"
                        };
                        this.cursor_pointer()
                            .child(
                                div().absolute().right_2().bottom_2().child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .rounded(px(4.))
                                        .bg(colors.elevated_surface_background)
                                        .text_ui_xs(cx)
                                        .text_color(colors.text)
                                        .child(label)
                                        .invisible()
                                        .group_hover(hover_group, |style| style.visible()),
                                ),
                            )
                            .on_click(cx.listener(move |this, _event: &ClickEvent, window, cx| {
                                this.entry_view_state.update(cx, |state, _cx| {
                                    state.toggle_tool_call_expansion(&toggle_id);
                                });
                                this.refresh_thread_search(window, cx);
                                cx.notify();
                            }))
                    }),
            )
            .into_any_element()
    }

    /// The language of an edited file, when it is loaded. Otherwise it starts
    /// loading and the thread view re-renders once it has.
    fn claude_code_diff_language(&self, path: &Path, cx: &Context<Self>) -> Option<Arc<Language>> {
        let project = self.project.upgrade()?;
        let registry = project.read(cx).languages().clone();
        let language_id = registry.language_for_file_path(path)?;
        let mut receiver = registry.load_language(language_id);
        match receiver.try_recv() {
            Ok(Some(Ok(language))) => Some(language),
            Ok(Some(Err(_))) | Err(_) => None,
            Ok(None) => {
                let pending = thread_activity(cx.entity_id(), cx)?.diff_language_pending();
                if !pending.replace(true) {
                    cx.spawn(async move |this, cx| {
                        receiver.await.ok();
                        pending.set(false);
                        this.update(cx, |_, cx| cx.notify()).ok();
                    })
                    .detach();
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_segments_pair_changed_runs() {
        let old_text = "a\nb\nc\nd";
        let new_text = "a\nB\nB2\nc\nd\ne";
        let segments = diff_segments(4, 6, &changed_rows(old_text, new_text));
        assert_eq!(
            inline_diff_lines(&segments),
            vec![
                DiffLine::Unchanged { old: 0, new: 0 },
                DiffLine::Removed(1),
                DiffLine::Added(1),
                DiffLine::Added(2),
                DiffLine::Unchanged { old: 2, new: 3 },
                DiffLine::Unchanged { old: 3, new: 4 },
                DiffLine::Added(5),
            ]
        );
        let rows = split_diff_rows(&segments);
        assert_eq!(rows.len(), 6);
        assert_eq!(
            rows[1],
            SplitDiffRow {
                old: Some(1),
                new: Some(1),
                changed: true
            }
        );
        assert_eq!(
            rows[2],
            SplitDiffRow {
                old: None,
                new: Some(2),
                changed: true
            }
        );
        assert_eq!(
            rows[5],
            SplitDiffRow {
                old: None,
                new: Some(5),
                changed: true
            }
        );
    }

    #[test]
    fn test_diff_segments_of_a_new_file() {
        let segments = diff_segments(0, 2, &changed_rows("", "one\ntwo"));
        assert_eq!(
            inline_diff_lines(&segments),
            vec![DiffLine::Added(0), DiffLine::Added(1)]
        );
        assert_eq!(line_ranges(""), Vec::<Range<usize>>::new());
    }

    #[test]
    fn test_line_runs_split_at_word_changes() {
        let style = TextStyle::default();
        let runs = vec![style.to_run(4), style.to_run(6)];
        let highlight = gpui::red();
        let pieces = line_runs(&runs, &(2..9), &[3..5], highlight);
        assert_eq!(
            pieces.iter().map(|run| run.len).collect::<Vec<_>>(),
            vec![1, 1, 1, 4]
        );
        assert_eq!(
            pieces
                .iter()
                .map(|run| run.background_color.is_some())
                .collect::<Vec<_>>(),
            vec![false, true, true, false]
        );
        assert_eq!(line_ranges("one\r\ntwo\n"), vec![0..3, 5..8]);
        assert_eq!(line_ranges("one\n\nthree"), vec![0..3, 4..4, 5..10]);
    }
}
