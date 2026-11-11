use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings_macros::{MergeFrom, with_fallible_options};

/// Settings for the optional VS Code style workbench.
#[with_fallible_options]
#[derive(Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, MergeFrom, Debug)]
pub struct WorkbenchSettingsContent {
    /// Settings for the activity bar shown on the left edge of the workspace.
    pub activity_bar: Option<ActivityBarSettingsContent>,
    /// Whether to show VS Code style headers above the docks: a view title for
    /// the left dock, an icon tab strip for the right dock and text tabs for the
    /// bottom dock.
    ///
    /// Default: false
    pub dock_headers: Option<bool>,
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
