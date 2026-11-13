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
    /// Whether to show the command center in the middle of the title bar: back
    /// and forward buttons and a search box that opens the file finder.
    ///
    /// Default: false
    pub command_center: Option<bool>,
    /// Whether to show buttons at the end of the title bar that toggle the side
    /// bars and the panel, next to a menu for customizing the layout.
    ///
    /// Default: false
    pub layout_controls: Option<bool>,
    /// Whether to show VS Code's status bar items (a remote indicator, the git
    /// branch, problem counts and a notifications bell) and to order the editor
    /// items like VS Code does.
    ///
    /// Default: false
    pub status_bar: Option<bool>,
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
