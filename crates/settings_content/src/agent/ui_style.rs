use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings_macros::{MergeFrom, with_fallible_options};

/// The visual style used to render agent threads.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentUiStyle {
    /// Zed's own agent thread style.
    #[default]
    Zed,
    /// A compact timeline modeled on the Claude Code VS Code extension.
    ClaudeCode,
}

/// Where agent threads are opened.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentThreadLocation {
    /// Open threads in the agent panel.
    #[default]
    Panel,
    /// Open threads as tabs in the editor area.
    Editor,
}

/// Settings of the Claude Code style that mirror the Claude Code extension's.
#[with_fallible_options]
#[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom, Debug, Default)]
pub struct ClaudeCodeStyleSettingsContent {
    /// Archive a session after this many days with no activity. Sessions that
    /// are open, running, waiting for input, unread, or in a group are never
    /// archived automatically. 0 never archives.
    ///
    /// Default: 14
    pub archive_inactive_sessions: Option<u32>,
}
