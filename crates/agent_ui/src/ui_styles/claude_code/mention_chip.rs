// Declared from `ui/mention_crease.rs` (via a `#[path]` attribute), so this module can
// draw a `MentionCrease` from its private fields and open it like Zed's crease does.

use gpui::{Animation, pulsating_between};

use super::*;
use crate::ui_styles::claude_code::ui_font_text_style;

impl MentionCrease {
    /// Claude Code shows a mention in its prompt as the typed "@name", in the
    /// prompt's own font, on a tinted chip in the theme's accent.
    pub(super) fn render_claude_code_chip(self, window: &mut Window, cx: &mut App) -> AnyElement {
        let text_style = ui_font_text_style(cx);
        let accent = cx.theme().colors().text_accent;
        let background = if self.is_toggled {
            accent.opacity(0.35)
        } else {
            accent.opacity(0.18)
        };
        let tooltip = self.tooltip;
        let image_preview = self.image_preview;
        let label = format!("@{}", self.label);
        let text = div().text_color(accent).child(label).map(|this| {
            if self.is_loading {
                this.with_animation(
                    "loading-context-chip",
                    Animation::new(Duration::from_secs(2))
                        .repeat()
                        .with_easing(pulsating_between(0.4, 0.8)),
                    |label, delta| label.opacity(delta),
                )
                .into_any_element()
            } else {
                this.into_any_element()
            }
        });

        h_flex()
            .id(self.id)
            .h(window.line_height())
            .px_1()
            .rounded(px(3.))
            .bg(background)
            .font_family(text_style.font_family)
            .text_size(text_style.font_size)
            .child(text)
            .when_some(
                self.mention_uri.zip(self.workspace),
                |this, (mention_uri, workspace)| {
                    this.cursor_pointer().on_click(move |_event, window, cx| {
                        open_mention_uri(mention_uri.clone(), &workspace, window, cx);
                    })
                },
            )
            .map(|chip| {
                if let Some(image_preview) = image_preview {
                    chip.hoverable_tooltip(image_preview)
                } else {
                    chip.when_some(tooltip, |this, tooltip_text| {
                        this.tooltip(Tooltip::text(tooltip_text))
                    })
                }
            })
            .into_any_element()
    }
}
