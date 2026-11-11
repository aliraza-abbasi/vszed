//! An optional VS Code style workbench layered on top of the Zed workspace.

mod activity_bar;
mod workbench_settings;

pub use activity_bar::ActivityBar;
pub use workbench_settings::{ActivityBarSettings, WorkbenchSettings};

use gpui::App;

pub fn init(cx: &mut App) {
    activity_bar::init(cx);
}
