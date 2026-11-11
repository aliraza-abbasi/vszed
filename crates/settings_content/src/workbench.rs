use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings_macros::{MergeFrom, with_fallible_options};

/// Settings for the optional VS Code style workbench.
#[with_fallible_options]
#[derive(Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, MergeFrom, Debug)]
pub struct WorkbenchSettingsContent {
    /// Settings for the activity bar shown on the left edge of the workspace.
    pub activity_bar: Option<ActivityBarSettingsContent>,
}

#[with_fallible_options]
#[derive(Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, MergeFrom, Debug)]
pub struct ActivityBarSettingsContent {
    /// Whether to show the activity bar. When shown, the panel buttons are
    /// removed from the status bar.
    ///
    /// Default: false
    pub visible: Option<bool>,
}
