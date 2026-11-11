use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings_macros::MergeFrom;

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
