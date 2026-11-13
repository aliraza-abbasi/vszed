use std::sync::Arc;

use gpui::{
    Action, Anchor, AnyView, ClickEvent, Entity, FocusHandle, FontWeight, Subscription, WeakEntity,
    transparent_black,
};
use settings::{Settings, SettingsStore};
use ui::{ContextMenu, Icon, PopoverMenu, Tooltip, prelude::*};
use workspace::{
    ToggleZoom, Workspace,
    dock::{Dock, DockPosition, PanelHandle},
};

use crate::{
    Codicon, PROJECT_PANEL_KEY, WorkbenchSettings, action_button, is_active_panel, panel_icon_path,
    panel_title, reveal_panel,
};

/// Height of VS Code's side bar and panel titles (`.part > .title` in `part.css`).
const TITLE_HEIGHT: Pixels = px(35.);
const TITLE_FONT_SIZE: Pixels = px(11.);
const TITLE_PADDING: Pixels = px(8.);
/// The side bar title label adds 12px of its own to the part title's 8px.
const TITLE_LABEL_PADDING: Pixels = px(20.);
const TEXT_TAB_PADDING: Pixels = px(10.);
const TEXT_TAB_HEIGHT: Pixels = px(27.);
const ICON_TAB_PADDING: Pixels = px(3.);

const EXPLORER_ACTIONS: [(&str, &str); 3] = [
    ("New File...", "project_panel::NewFile"),
    ("New Folder...", "project_panel::NewDirectory"),
    (
        "Collapse Folders in Explorer",
        "project_panel::CollapseAllEntries",
    ),
];

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        apply_dock_headers(workspace, window, cx);
        cx.observe_global_in::<SettingsStore>(window, apply_dock_headers)
            .detach();
    })
    .detach();
}

fn apply_dock_headers(
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let enabled = WorkbenchSettings::get_global(cx).dock_headers;
    let workspace_handle = workspace.weak_handle();
    for dock in workspace.all_docks().map(Clone::clone) {
        let installed = dock
            .read(cx)
            .header()
            .is_some_and(|header| header.clone().downcast::<DockHeader>().is_ok());
        if enabled == installed {
            continue;
        }

        let header = enabled.then(|| {
            AnyView::from(cx.new(|cx| DockHeader::new(workspace_handle.clone(), dock.clone(), cx)))
        });
        dock.update(cx, |dock, cx| dock.set_header(header, cx));
    }
}

/// The title row VS Code shows above each part: a view title for the primary side
/// bar (left dock), an icon tab strip for the secondary side bar (right dock) and
/// text tabs for the panel (bottom dock).
pub struct DockHeader {
    workspace: WeakEntity<Workspace>,
    dock: Entity<Dock>,
    _dock_subscription: Subscription,
}

impl DockHeader {
    fn new(workspace: WeakEntity<Workspace>, dock: Entity<Dock>, cx: &mut Context<Self>) -> Self {
        let dock_subscription = cx.observe(&dock, |_, _, cx| cx.notify());
        Self {
            workspace,
            dock,
            _dock_subscription: dock_subscription,
        }
    }

    fn activate_tab(
        &mut self,
        panel: &Arc<dyn PanelHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !is_active_panel(self.dock.read(cx), panel) {
            reveal_panel(&self.dock, panel, window, cx);
        }
    }

    fn hide_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (position, is_open) = {
            let dock = self.dock.read(cx);
            (dock.position(), dock.is_open())
        };
        if !is_open {
            return;
        }
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.toggle_dock(position, window, cx);
            });
        }
    }

    fn toggle_maximized(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.dock.read(cx).active_panel().cloned() else {
            return;
        };
        // Each panel implements zooming itself (usually through the pane inside it),
        // so the action has to start inside the panel, like its keybinding does.
        window.focus(&panel.activation_focus_handle(cx), cx);
        window.dispatch_action(ToggleZoom.boxed_clone(), cx);
    }

    fn move_panels_to(
        &mut self,
        position: DockPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panels = self.dock.read(cx).panels().cloned().collect::<Vec<_>>();
        for panel in panels {
            if panel.position_is_valid(position, cx) {
                panel.set_position(position, window, cx);
            }
        }
    }

    fn render_title(
        &self,
        active_panel: Option<&Arc<dyn PanelHandle>>,
        position: DockPosition,
        cx: &App,
    ) -> impl IntoElement {
        let title = active_panel
            .map(|panel| panel_title(panel.as_ref(), position))
            .unwrap_or_default();
        div()
            .min_w_0()
            .truncate()
            .text_size(TITLE_FONT_SIZE)
            .font_weight(FontWeight::NORMAL)
            .text_color(cx.theme().colors().text)
            .child(title)
    }

    fn render_icon_tab(
        &self,
        panel: Arc<dyn PanelHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let icon_path = panel_icon_path(panel.as_ref(), window, cx)?;
        let label = panel
            .icon_tooltip(window, cx)
            .unwrap_or_else(|| panel.persistent_name());
        let is_active = is_active_panel(self.dock.read(cx), &panel);
        let toggle_action = panel.toggle_action(window, cx);
        let border_color = if is_active {
            cx.theme().colors().text_accent
        } else {
            transparent_black()
        };

        Some(
            div()
                .flex()
                .flex_none()
                .h_full()
                .px(ICON_TAB_PADDING)
                .items_center()
                .border_b_1()
                .border_color(border_color)
                .child(
                    action_button(
                        ("dock-header-icon-tab", panel.panel_id().as_u64()),
                        Icon::from_path(icon_path).color(if is_active {
                            Color::Default
                        } else {
                            Color::Muted
                        }),
                    )
                    .aria_label(label)
                    .tooltip(move |_window, cx| Tooltip::for_action(label, &*toggle_action, cx))
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            this.activate_tab(&panel, window, cx);
                        },
                    )),
                )
                .into_any_element(),
        )
    }

    fn render_text_tab(
        &self,
        panel: Arc<dyn PanelHandle>,
        position: DockPosition,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let is_active = is_active_panel(self.dock.read(cx), &panel);
        let (text_color, border_color) = if is_active {
            (colors.text, colors.text_accent)
        } else {
            (colors.text_muted, transparent_black())
        };
        let hover_text_color = colors.text;

        h_flex()
            .id(("dock-header-text-tab", panel.panel_id().as_u64()))
            .flex_none()
            .h_full()
            .px(TEXT_TAB_PADDING)
            .cursor_pointer()
            .text_size(TITLE_FONT_SIZE)
            .text_color(text_color)
            .hover(move |style| style.text_color(hover_text_color))
            .child(
                h_flex()
                    .h(TEXT_TAB_HEIGHT)
                    .border_b_1()
                    .border_color(border_color)
                    .child(panel_title(panel.as_ref(), position)),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.activate_tab(&panel, window, cx);
            }))
            .into_any_element()
    }

    fn render_more_menu(
        &self,
        position: DockPosition,
        active_panel: Option<Arc<dyn PanelHandle>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let this = cx.weak_entity();
        let hide_action = self.dock.read(cx).toggle_action();
        let (move_label, move_position, hide_label) = match position {
            DockPosition::Left => (
                "Move Primary Side Bar Right",
                DockPosition::Right,
                "Hide Primary Side Bar",
            ),
            DockPosition::Right | DockPosition::Bottom => (
                "Move Secondary Side Bar Left",
                DockPosition::Left,
                "Hide Secondary Side Bar",
            ),
        };

        PopoverMenu::new("dock-header-more-menu")
            .anchor(Anchor::TopRight)
            .trigger_with_tooltip(
                action_button("dock-header-more", Codicon::Ellipsis.icon())
                    .aria_label("Views and More Actions..."),
                Tooltip::text("Views and More Actions..."),
            )
            .menu(move |window, cx| {
                let this = this.clone();
                let hide_action = hide_action.boxed_clone();
                let panel_actions = active_panel
                    .as_ref()
                    .and_then(|panel| panel_menu_actions(panel, cx));
                Some(ContextMenu::build(window, cx, move |menu, _window, _cx| {
                    let menu = match panel_actions {
                        Some((panel_focus_handle, actions)) => actions
                            .into_iter()
                            .fold(menu.context(panel_focus_handle), |menu, (label, action)| {
                                menu.action(label, action)
                            })
                            .separator(),
                        None => menu,
                    };
                    menu.entry(move_label, None, {
                        let this = this.clone();
                        move |window, cx| {
                            if let Some(this) = this.upgrade() {
                                this.update(cx, |this, cx| {
                                    this.move_panels_to(move_position, window, cx);
                                });
                            }
                        }
                    })
                    .entry(
                        hide_label,
                        Some(hide_action),
                        move |window, cx| {
                            if let Some(this) = this.upgrade() {
                                this.update(cx, |this, cx| this.hide_dock(window, cx));
                            }
                        },
                    )
                }))
            })
    }

    fn render_maximize_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        action_button("dock-header-maximize", Codicon::ChevronUp.icon())
            .aria_label("Maximize Panel")
            .tooltip(|_window, cx| Tooltip::for_action("Maximize Panel", &ToggleZoom, cx))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.toggle_maximized(window, cx);
            }))
    }

    fn render_close_button(
        &self,
        position: DockPosition,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label = match position {
            DockPosition::Left => "Hide Primary Side Bar",
            DockPosition::Right => "Hide Secondary Side Bar",
            DockPosition::Bottom => "Hide Panel",
        };
        let hide_action = self.dock.read(cx).toggle_action();

        action_button("dock-header-close", Codicon::Close.icon())
            .aria_label(label)
            .tooltip(move |_window, cx| Tooltip::for_action(label, &*hide_action, cx))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.hide_dock(window, cx);
            }))
    }
}

/// Actions of the active view that VS Code offers in its title, dispatched to the
/// panel itself.
fn panel_menu_actions(
    panel: &Arc<dyn PanelHandle>,
    cx: &App,
) -> Option<(FocusHandle, Vec<(&'static str, Box<dyn Action>)>)> {
    if panel.panel_key() != PROJECT_PANEL_KEY {
        return None;
    }
    let actions = EXPLORER_ACTIONS
        .into_iter()
        .filter_map(
            |(label, action_name)| match cx.build_action(action_name, None) {
                Ok(action) => Some((label, action)),
                Err(error) => {
                    log::error!("failed to build the {action_name} action: {error}");
                    None
                }
            },
        )
        .collect::<Vec<_>>();
    if actions.is_empty() {
        return None;
    }
    Some((panel.panel_focus_handle(cx), actions))
}

impl Render for DockHeader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (position, panels, active_panel) = {
            let dock = self.dock.read(cx);
            (
                dock.position(),
                dock.panels().cloned().collect::<Vec<_>>(),
                dock.active_panel().cloned(),
            )
        };

        let header = h_flex()
            .id("dock-header")
            .debug_selector(|| format!("dock-header-{position:?}"))
            .flex_none()
            .w_full()
            .h(TITLE_HEIGHT)
            .gap_1()
            .justify_between()
            .overflow_hidden();

        match position {
            DockPosition::Left => header
                .pl(TITLE_LABEL_PADDING)
                .pr(TITLE_PADDING)
                .child(self.render_title(active_panel.as_ref(), position, cx))
                .child(self.render_more_menu(position, active_panel, cx)),
            DockPosition::Right => {
                let tabs = panels
                    .into_iter()
                    .filter_map(|panel| self.render_icon_tab(panel, window, cx))
                    .collect::<Vec<_>>();
                header
                    .px(TITLE_PADDING)
                    .child(h_flex().h_full().min_w_0().overflow_hidden().children(tabs))
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_0p5()
                            .child(self.render_more_menu(position, active_panel, cx))
                            .child(self.render_close_button(position, cx)),
                    )
            }
            DockPosition::Bottom => {
                let tabs = panels
                    .into_iter()
                    .map(|panel| self.render_text_tab(panel, position, cx))
                    .collect::<Vec<_>>();
                header
                    .px(TITLE_PADDING)
                    .child(h_flex().h_full().min_w_0().overflow_hidden().children(tabs))
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_0p5()
                            .child(self.render_maximize_button(cx))
                            .child(self.render_close_button(position, cx)),
                    )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    use crate::test_support::{
        AgentTestPanel, DebugTestPanel, ExplorerTestPanel, SourceControlTestPanel,
        TerminalTestPanel, init_test, open_workspace, update_workbench_settings,
    };

    fn set_dock_headers(enabled: bool, cx: &mut VisualTestContext) {
        cx.update(|_window, cx| {
            update_workbench_settings(cx, |workbench| {
                workbench.dock_headers = Some(enabled);
            });
        });
        cx.run_until_parked();
    }

    fn installed_header(
        dock: &Entity<Dock>,
        cx: &mut VisualTestContext,
    ) -> Option<Entity<DockHeader>> {
        dock.read_with(cx, |dock, _| {
            dock.header()
                .and_then(|header| header.clone().downcast::<DockHeader>().ok())
        })
    }

    fn panel_handle(
        dock: &Entity<Dock>,
        panel_id: gpui::EntityId,
        cx: &mut VisualTestContext,
    ) -> Arc<dyn PanelHandle> {
        dock.read_with(cx, |dock, _| {
            dock.panels()
                .find(|panel| panel.panel_id() == panel_id)
                .cloned()
        })
        .expect("panel should be in the dock")
    }

    fn active_panel_id(dock: &Entity<Dock>, cx: &mut VisualTestContext) -> Option<gpui::EntityId> {
        dock.read_with(cx, |dock, _| {
            dock.is_open()
                .then(|| dock.active_panel().map(|panel| panel.panel_id()))
                .flatten()
        })
    }

    #[gpui::test]
    async fn test_dock_headers_follow_setting(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace(cx).await;
        let docks = workspace.read_with(cx, |workspace, _| workspace.all_docks().map(Clone::clone));

        for dock in &docks {
            assert!(installed_header(dock, cx).is_none());
        }

        set_dock_headers(true, cx);
        for dock in &docks {
            assert!(installed_header(dock, cx).is_some());
        }

        set_dock_headers(false, cx);
        for dock in &docks {
            assert!(installed_header(dock, cx).is_none());
        }
    }

    #[gpui::test]
    async fn test_dock_header_sits_above_panel(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace(cx).await;
        ExplorerTestPanel::add(&workspace, DockPosition::Left, 1, cx);
        AgentTestPanel::add(&workspace, DockPosition::Right, 2, cx);
        TerminalTestPanel::add(&workspace, DockPosition::Bottom, 3, cx);
        workspace.update_in(cx, |workspace, window, cx| {
            for position in [
                DockPosition::Left,
                DockPosition::Right,
                DockPosition::Bottom,
            ] {
                workspace.toggle_dock(position, window, cx);
            }
        });
        cx.run_until_parked();

        let panel_selectors = [
            ("ExplorerTestPanel", "dock-header-Left"),
            ("AgentTestPanel", "dock-header-Right"),
            ("TerminalTestPanel", "dock-header-Bottom"),
        ];
        let panel_bounds_without_headers = panel_selectors.map(|(panel_selector, _)| {
            cx.debug_bounds(panel_selector)
                .expect("panel should be rendered")
        });

        set_dock_headers(true, cx);
        for ((panel_selector, header_selector), bounds_without_header) in panel_selectors
            .into_iter()
            .zip(panel_bounds_without_headers)
        {
            let header_bounds = cx
                .debug_bounds(header_selector)
                .expect("header should be rendered");
            let panel_bounds = cx
                .debug_bounds(panel_selector)
                .expect("panel should be rendered");
            assert_eq!(header_bounds.size.height, TITLE_HEIGHT);
            assert_eq!(header_bounds.origin, bounds_without_header.origin);
            assert_eq!(header_bounds.size.width, bounds_without_header.size.width);
            assert_eq!(panel_bounds.origin.y, header_bounds.bottom());
            assert_eq!(
                panel_bounds.size.height,
                bounds_without_header.size.height - TITLE_HEIGHT,
                "{panel_selector} should shrink to make room for its header"
            );
        }
    }

    #[gpui::test]
    async fn test_panel_titles(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace(cx).await;
        let explorer = ExplorerTestPanel::add(&workspace, DockPosition::Left, 1, cx);
        let debug = DebugTestPanel::add(&workspace, DockPosition::Left, 2, cx);
        let terminal = TerminalTestPanel::add(&workspace, DockPosition::Bottom, 3, cx);
        let unknown = workspace.update_in(cx, |workspace, window, cx| {
            let panel =
                cx.new(|cx| workspace::dock::test::TestPanel::new(DockPosition::Left, 4, cx));
            workspace.add_panel(panel.clone(), window, cx);
            panel
        });
        cx.run_until_parked();

        let (left_dock, bottom_dock) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.left_dock().clone(),
                workspace.bottom_dock().clone(),
            )
        });
        let title = |dock: &Entity<Dock>, panel_id, position, cx: &mut VisualTestContext| {
            panel_title(panel_handle(dock, panel_id, cx).as_ref(), position)
        };
        assert_eq!(
            title(&left_dock, explorer.entity_id(), DockPosition::Left, cx),
            "EXPLORER"
        );
        assert_eq!(
            title(&left_dock, debug.entity_id(), DockPosition::Left, cx),
            "RUN AND DEBUG"
        );
        assert_eq!(
            title(&left_dock, debug.entity_id(), DockPosition::Bottom, cx),
            "DEBUG CONSOLE"
        );
        assert_eq!(
            title(&bottom_dock, terminal.entity_id(), DockPosition::Bottom, cx),
            "TERMINAL"
        );
        assert_eq!(
            title(&left_dock, unknown.entity_id(), DockPosition::Left, cx),
            "TESTPANEL",
            "panels without a VS Code name use their persistent name"
        );
    }

    #[gpui::test]
    async fn test_dock_header_tabs_activate_panels(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace(cx).await;
        set_dock_headers(true, cx);

        let terminal = TerminalTestPanel::add(&workspace, DockPosition::Bottom, 1, cx);
        let debug = DebugTestPanel::add(&workspace, DockPosition::Bottom, 2, cx);
        cx.run_until_parked();

        let bottom_dock = workspace.read_with(cx, |workspace, _| workspace.bottom_dock().clone());
        let header = installed_header(&bottom_dock, cx).expect("header should be installed");
        let terminal_handle = panel_handle(&bottom_dock, terminal.entity_id(), cx);
        let debug_handle = panel_handle(&bottom_dock, debug.entity_id(), cx);

        header.update_in(cx, |header, window, cx| {
            header.activate_tab(&terminal_handle, window, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            active_panel_id(&bottom_dock, cx),
            Some(terminal.entity_id())
        );

        header.update_in(cx, |header, window, cx| {
            header.activate_tab(&debug_handle, window, cx);
        });
        cx.run_until_parked();
        assert_eq!(active_panel_id(&bottom_dock, cx), Some(debug.entity_id()));

        header.update_in(cx, |header, window, cx| {
            header.activate_tab(&debug_handle, window, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            active_panel_id(&bottom_dock, cx),
            Some(debug.entity_id()),
            "clicking the active tab keeps the panel open"
        );

        header.update_in(cx, |header, window, cx| header.hide_dock(window, cx));
        cx.run_until_parked();
        assert_eq!(active_panel_id(&bottom_dock, cx), None);
    }

    #[gpui::test]
    async fn test_move_primary_side_bar_right(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace(cx).await;
        set_dock_headers(true, cx);

        let explorer = ExplorerTestPanel::add(&workspace, DockPosition::Left, 1, cx);
        let source_control = SourceControlTestPanel::add(&workspace, DockPosition::Left, 2, cx);
        let agent = AgentTestPanel::add(&workspace, DockPosition::Right, 3, cx);
        cx.run_until_parked();

        let (left_dock, right_dock) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.left_dock().clone(),
                workspace.right_dock().clone(),
            )
        });
        let header = installed_header(&left_dock, cx).expect("header should be installed");
        header.update_in(cx, |header, window, cx| {
            header.move_panels_to(DockPosition::Right, window, cx);
        });
        cx.run_until_parked();

        assert_eq!(left_dock.read_with(cx, |dock, _| dock.panels_len()), 0);
        let right_panels = right_dock.read_with(cx, |dock, _| {
            dock.panels()
                .map(|panel| panel.panel_id())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            right_panels,
            vec![
                explorer.entity_id(),
                source_control.entity_id(),
                agent.entity_id()
            ]
        );
    }
}
