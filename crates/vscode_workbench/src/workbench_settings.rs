use settings::{RegisterSetting, Settings, SettingsContent};

#[derive(Clone, Copy, Debug, Default, PartialEq, RegisterSetting)]
pub struct WorkbenchSettings {
    pub activity_bar: ActivityBarSettings,
    pub dock_headers: bool,
    pub command_center: bool,
    pub layout_controls: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ActivityBarSettings {
    pub visible: bool,
}

impl Settings for WorkbenchSettings {
    // Every field falls back to its default so the workbench needs no entries in
    // the bundled default settings while it is incubated in this crate.
    fn from_settings(content: &SettingsContent) -> Self {
        let workbench = content.workbench.as_ref();
        let activity_bar = workbench.and_then(|workbench| workbench.activity_bar.as_ref());

        Self {
            activity_bar: ActivityBarSettings {
                visible: activity_bar
                    .and_then(|activity_bar| activity_bar.visible)
                    .unwrap_or_default(),
            },
            dock_headers: workbench
                .and_then(|workbench| workbench.dock_headers)
                .unwrap_or_default(),
            command_center: workbench
                .and_then(|workbench| workbench.command_center)
                .unwrap_or_default(),
            layout_controls: workbench
                .and_then(|workbench| workbench.layout_controls)
                .unwrap_or_default(),
        }
    }
}
