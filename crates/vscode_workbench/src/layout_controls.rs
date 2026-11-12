use std::sync::Arc;

use fs::Fs;
use gpui::{Action, Anchor, AnyView, ClickEvent, Empty, WeakEntity, actions};
use settings::{BottomDockLayout, Settings, SettingsStore, update_settings_file};
use title_bar::TitleBar;
use ui::{ButtonLike, ContextMenu, IconPosition, PopoverMenu, Tooltip, prelude::*};
use workspace::{Workspace, WorkspaceSettings, dock::DockPosition};

use crate::{
    Codicon, RestorePreviousLayout, UseVsCodeLayout, WorkbenchSettings, action_button,
    preset::has_previous_layout,
};

actions!(
    workbench,
    [
        /// Shows or hides the activity bar.
        ToggleActivityBar,
    ]
);

/// The dock toggles in VS Code's order, with the icons for an open and a
/// closed dock.
const DOCK_TOGGLES: [(DockPosition, &str, Codicon, Codicon); 3] = [
    (
        DockPosition::Left,
        "Primary Side Bar",
        Codicon::LayoutSidebarLeft,
        Codicon::LayoutSidebarLeftOff,
    ),
    (
        DockPosition::Bottom,
        "Panel",
        Codicon::LayoutPanel,
        Codicon::LayoutPanelOff,
    ),
    (
        DockPosition::Right,
        "Secondary Side Bar",
        Codicon::LayoutSidebarRight,
        Codicon::LayoutSidebarRightOff,
    ),
];

const BOTTOM_DOCK_LAYOUTS: [(BottomDockLayout, &str); 4] = [
    (BottomDockLayout::Contained, "Contained"),
    (BottomDockLayout::Full, "Full"),
    (BottomDockLayout::LeftAligned, "Left Aligned"),
    (BottomDockLayout::RightAligned, "Right Aligned"),
];

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(toggle_activity_bar);
    })
    .detach();
    cx.observe_new(|title_bar: &mut TitleBar, window, cx| {
        let Some(window) = window else {
            return;
        };
        apply_layout_controls(title_bar, window, cx);
        cx.observe_global_in::<SettingsStore>(window, apply_layout_controls)
            .detach();
    })
    .detach();
}

fn toggle_activity_bar(
    workspace: &mut Workspace,
    _: &ToggleActivityBar,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    set_activity_bar_visible(workspace.app_state().fs.clone(), cx);
}

fn set_activity_bar_visible(fs: Arc<dyn Fs>, cx: &App) {
    let visible = !WorkbenchSettings::get_global(cx).activity_bar.visible;
    update_settings_file(fs, cx, move |settings, _cx| {
        settings
            .workbench
            .get_or_insert_default()
            .activity_bar
            .get_or_insert_default()
            .visible = Some(visible);
    });
}

fn apply_layout_controls(
    title_bar: &mut TitleBar,
    _window: &mut Window,
    cx: &mut Context<TitleBar>,
) {
    let enabled = WorkbenchSettings::get_global(cx).layout_controls;
    let installed = title_bar
        .trailing_item()
        .is_some_and(|item| item.clone().downcast::<LayoutControls>().is_ok());
    if enabled == installed {
        return;
    }

    let layout_controls = enabled.then(|| {
        let workspace = title_bar.workspace().clone();
        AnyView::from(cx.new(|_| LayoutControls { workspace }))
    });
    title_bar.set_trailing_item(layout_controls, cx);
}

/// VS Code's layout controls: a menu to customize the layout and buttons that
/// toggle the primary side bar, the panel and the secondary side bar. They work
/// without the status bar's dock buttons, which the activity bar hides.
pub struct LayoutControls {
    workspace: WeakEntity<Workspace>,
}

impl LayoutControls {
    fn toggle_dock(&mut self, position: DockPosition, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.toggle_dock(position, window, cx);
            });
        }
    }

    fn render_dock_toggle(
        &self,
        (position, name, open_icon, closed_icon): (DockPosition, &'static str, Codicon, Codicon),
        is_open: bool,
        toggle_action: Box<dyn Action>,
        cx: &mut Context<Self>,
    ) -> ButtonLike {
        let icon = if is_open { open_icon } else { closed_icon };
        let label: SharedString = format!("Toggle {name}").into();

        action_button(
            SharedString::from(format!("layout-controls-{position:?}")),
            icon.icon(),
        )
        .tab_index(0isize)
        .aria_label(label.clone())
        .tooltip(move |_window, cx| Tooltip::for_action(label.clone(), &*toggle_action, cx))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.toggle_dock(position, window, cx);
        }))
    }

    fn render_customize_layout_menu(&self) -> impl IntoElement {
        let workspace = self.workspace.clone();
        PopoverMenu::new("layout-controls-customize-menu")
            .anchor(Anchor::TopRight)
            .trigger_with_tooltip(
                action_button("layout-controls-customize", Codicon::Layout.icon())
                    .tab_index(0isize)
                    .aria_label("Customize Layout..."),
                Tooltip::text("Customize Layout..."),
            )
            .menu(move |window, cx| {
                let workspace = workspace.upgrade()?;
                let dock_states = DOCK_TOGGLES.map(|(position, name, _, _)| {
                    let dock = workspace.read(cx).dock_at_position(position).read(cx);
                    (position, name, dock.is_open(), dock.toggle_action())
                });
                let activity_bar_visible = WorkbenchSettings::get_global(cx).activity_bar.visible;
                let bottom_dock_layout = WorkspaceSettings::get_global(cx).bottom_dock_layout;
                let can_restore = has_previous_layout(cx);
                let fs = workspace.read(cx).app_state().fs.clone();
                let workspace = workspace.downgrade();

                Some(ContextMenu::build(window, cx, move |menu, _window, _cx| {
                    let menu = menu.header("Visibility").toggleable_entry(
                        "Activity Bar",
                        activity_bar_visible,
                        IconPosition::Start,
                        Some(ToggleActivityBar.boxed_clone()),
                        {
                            let fs = fs.clone();
                            move |_window, cx| set_activity_bar_visible(fs.clone(), cx)
                        },
                    );
                    let menu = dock_states.into_iter().fold(
                        menu,
                        |menu, (position, name, is_open, toggle_action)| {
                            let workspace = workspace.clone();
                            menu.toggleable_entry(
                                name,
                                is_open,
                                IconPosition::Start,
                                Some(toggle_action),
                                move |window, cx| {
                                    if let Some(workspace) = workspace.upgrade() {
                                        workspace.update(cx, |workspace, cx| {
                                            workspace.toggle_dock(position, window, cx);
                                        });
                                    }
                                },
                            )
                        },
                    );
                    let menu = BOTTOM_DOCK_LAYOUTS.into_iter().fold(
                        menu.separator().header("Bottom Panel Alignment"),
                        |menu, (layout, label)| {
                            let fs = fs.clone();
                            menu.toggleable_entry(
                                label,
                                layout == bottom_dock_layout,
                                IconPosition::Start,
                                None,
                                move |_window, cx| {
                                    update_settings_file(fs.clone(), cx, move |settings, _cx| {
                                        settings.workspace.bottom_dock_layout = Some(layout);
                                    });
                                },
                            )
                        },
                    );
                    menu.separator()
                        .action("Use VS Code Layout", UseVsCodeLayout.boxed_clone())
                        .action_disabled_when(
                            !can_restore,
                            "Restore Previous Layout",
                            RestorePreviousLayout.boxed_clone(),
                        )
                }))
            })
    }
}

impl Render for LayoutControls {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(workspace) = self.workspace.upgrade() else {
            return Empty.into_any_element();
        };
        let dock_states = DOCK_TOGGLES.map(|(position, ..)| {
            let dock = workspace.read(cx).dock_at_position(position).read(cx);
            (dock.is_open(), dock.toggle_action())
        });
        let dock_toggles = DOCK_TOGGLES
            .into_iter()
            .zip(dock_states)
            .map(|(dock_toggle, (is_open, toggle_action))| {
                self.render_dock_toggle(dock_toggle, is_open, toggle_action, cx)
            })
            .collect::<Vec<_>>();

        h_flex()
            .gap_0p5()
            .child(self.render_customize_layout_menu())
            .children(dock_toggles)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use workspace::dock::{Dock, PanelButtons};

    use crate::test_support::{
        AgentTestPanel, ExplorerTestPanel, TerminalTestPanel, init_test, open_workspace,
        open_workspace_with_title_bar, title_bar, update_workbench_settings,
    };

    fn installed_layout_controls(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Option<Entity<LayoutControls>> {
        title_bar(workspace, cx).read_with(cx, |title_bar, _| {
            title_bar
                .trailing_item()
                .and_then(|item| item.clone().downcast::<LayoutControls>().ok())
        })
    }

    fn active_panel_id(dock: &Entity<Dock>, cx: &mut VisualTestContext) -> Option<gpui::EntityId> {
        dock.read_with(cx, |dock, _| {
            dock.is_open()
                .then(|| dock.active_panel().map(|panel| panel.panel_id()))
                .flatten()
        })
    }

    #[gpui::test]
    async fn test_layout_controls_follow_setting(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace_with_title_bar(cx).await;
        assert!(installed_layout_controls(&workspace, cx).is_none());

        for enabled in [true, false] {
            cx.update(|_window, cx| {
                update_workbench_settings(cx, |workbench| {
                    workbench.layout_controls = Some(enabled);
                });
            });
            cx.run_until_parked();
            assert_eq!(installed_layout_controls(&workspace, cx).is_some(), enabled);
        }
    }

    #[gpui::test]
    async fn test_layout_controls_toggle_docks_without_panel_buttons(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace_with_title_bar(cx).await;
        cx.update(|_window, cx| {
            update_workbench_settings(cx, |workbench| {
                workbench.activity_bar.get_or_insert_default().visible = Some(true);
                workbench.layout_controls = Some(true);
            });
        });
        let panels = [
            (
                DockPosition::Left,
                ExplorerTestPanel::add(&workspace, DockPosition::Left, 1, cx).entity_id(),
            ),
            (
                DockPosition::Bottom,
                TerminalTestPanel::add(&workspace, DockPosition::Bottom, 1, cx).entity_id(),
            ),
            (
                DockPosition::Right,
                AgentTestPanel::add(&workspace, DockPosition::Right, 1, cx).entity_id(),
            ),
        ];
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            assert!(
                workspace
                    .status_bar()
                    .read(cx)
                    .item_of_type::<PanelButtons>()
                    .is_none(),
                "the activity bar hides every dock's panel buttons"
            );
        });

        let layout_controls =
            installed_layout_controls(&workspace, cx).expect("layout controls should be installed");
        for (position, panel_id) in panels {
            let dock = workspace.read_with(cx, |workspace, _| {
                workspace.dock_at_position(position).clone()
            });
            assert_eq!(active_panel_id(&dock, cx), None);

            layout_controls.update_in(cx, |layout_controls, window, cx| {
                layout_controls.toggle_dock(position, window, cx);
            });
            cx.run_until_parked();
            assert_eq!(
                active_panel_id(&dock, cx),
                Some(panel_id),
                "the {position:?} dock opens with its panel"
            );

            layout_controls.update_in(cx, |layout_controls, window, cx| {
                layout_controls.toggle_dock(position, window, cx);
            });
            cx.run_until_parked();
            assert_eq!(active_panel_id(&dock, cx), None);
        }
    }

    #[gpui::test]
    async fn test_toggle_activity_bar(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace(cx).await;

        for expected_visibility in [true, false] {
            workspace.update_in(cx, |workspace, window, cx| {
                toggle_activity_bar(workspace, &ToggleActivityBar, window, cx);
            });
            cx.run_until_parked();
            let visible =
                cx.update(|_window, cx| WorkbenchSettings::get_global(cx).activity_bar.visible);
            assert_eq!(visible, expected_visibility);
        }
    }
}
