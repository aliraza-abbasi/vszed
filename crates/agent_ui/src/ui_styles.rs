pub(crate) mod claude_code;
mod preset;
pub(crate) mod thread_item;

use gpui::{App, TextStyle};
use settings::{AgentThreadLocation, AgentUiStyle, RegisterSetting, Settings, SettingsContent};

/// Resolved `agent.ui_style` and `agent.thread_location` settings.
///
/// These live outside of `AgentSettings` while the alternative styles are
/// incubating, so that the settings hook stays confined to
/// `AgentSettingsContent` and no exhaustive `AgentSettings` literal needs to
/// change.
#[derive(Clone, Copy, Debug, RegisterSetting)]
pub(crate) struct AgentUiStyleSettings {
    pub ui_style: AgentUiStyle,
    pub thread_location: AgentThreadLocation,
}

impl Settings for AgentUiStyleSettings {
    fn from_settings(content: &SettingsContent) -> Self {
        let agent = content.agent.as_ref();
        Self {
            ui_style: agent.and_then(|agent| agent.ui_style).unwrap_or_default(),
            thread_location: agent
                .and_then(|agent| agent.thread_location)
                .unwrap_or_default(),
        }
    }
}

pub(crate) fn claude_code_style_enabled(cx: &App) -> bool {
    AgentUiStyleSettings::get_global(cx).ui_style == AgentUiStyle::ClaudeCode
}

/// The text style of agent message editors (the prompt and editable sent
/// messages): Zed's own style unless the active ui style draws them differently.
pub(crate) fn message_editor_text_style(zed_style: TextStyle, cx: &App) -> TextStyle {
    if claude_code_style_enabled(cx) {
        claude_code::ui_font_text_style(cx)
    } else {
        zed_style
    }
}

pub(crate) fn init(cx: &mut App) {
    claude_code::init(cx);
    preset::init(cx);
    thread_item::init(cx);
}
