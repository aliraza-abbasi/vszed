use std::sync::Arc;

use client::{SignIn, SignOut, UserStore};
use gpui::{Action, Anchor, ClickEvent, Empty, Entity, Subscription, WeakEntity};
use settings::{Settings, SettingsStore};
use ui::{ContextMenu, CountBadge, IconButtonShape, PopoverMenu, Tooltip, prelude::*};
use workspace::{
    DeploySearch, HideStatusItem, ItemHandle, StatusItemView, Workspace,
    dock::{Dock, PanelButtons, PanelHandle},
};

use crate::{
    DEBUG_PANEL_KEY, GIT_PANEL_KEY, PROJECT_PANEL_KEY, UseVsCodeLayout, WorkbenchSettings,
    is_active_panel, reveal_panel,
};

const ACTIVITY_BAR_WIDTH: Pixels = px(48.);
const ACTIVE_INDICATOR_WIDTH: Pixels = px(2.);

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        apply_activity_bar_visibility(workspace, window, cx);
        cx.observe_global_in::<SettingsStore>(window, apply_activity_bar_visibility)
            .detach();
    })
    .detach();
}

fn apply_activity_bar_visibility(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let visible = WorkbenchSettings::get_global(cx).activity_bar.visible;
    let installed = workspace
        .leading_item()
        .is_some_and(|item| item.downcast::<ActivityBar>().is_ok());
    if visible == installed {
        return;
    }

    if visible {
        let activity_bar = cx.new(|cx| ActivityBar::new(workspace, cx));
        workspace.set_leading_item(Some(activity_bar.into()), cx);
        hide_panel_buttons(workspace, window, cx);
    } else {
        workspace.set_leading_item(None, cx);
        restore_panel_buttons(workspace, window, cx);
    }
}

/// Holds a dock's panel buttons while the activity bar replaces them. The status
/// bar can only insert an item after an existing one, so keeping a placeholder in
/// the buttons' slot is what lets them return to their original position.
struct HiddenPanelButtons {
    panel_buttons: Entity<PanelButtons>,
}

impl Render for HiddenPanelButtons {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

impl StatusItemView for HiddenPanelButtons {
    fn set_active_pane_item(
        &mut self,
        _active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn hide_setting(&self, _cx: &App) -> Option<HideStatusItem> {
        None
    }
}

fn hide_panel_buttons(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    workspace.status_bar().update(cx, |status_bar, cx| {
        while let Some(position) = status_bar.position_of_item::<PanelButtons>()
            && let Some(panel_buttons) = status_bar.item_of_type::<PanelButtons>()
        {
            let placeholder = cx.new(|_| HiddenPanelButtons { panel_buttons });
            status_bar.insert_item_after(position, placeholder, window, cx);
            status_bar.remove_item_at(position, cx);
        }
    });
}

fn restore_panel_buttons(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace.status_bar().update(cx, |status_bar, cx| {
        while let Some(position) = status_bar.position_of_item::<HiddenPanelButtons>()
            && let Some(placeholder) = status_bar.item_of_type::<HiddenPanelButtons>()
        {
            let panel_buttons = placeholder.read(cx).panel_buttons.clone();
            status_bar.insert_item_after(position, panel_buttons, window, cx);
            status_bar.remove_item_at(position, cx);
        }
    });
}

enum ActivityBarEntry {
    Panel(Arc<dyn PanelHandle>),
    Search,
    Extensions,
}

#[derive(Clone, Copy)]
enum PrimarySideBarSlot {
    Panel(&'static str),
    Search,
    Extensions,
}

/// The fixed order of VS Code's built-in view containers. Left dock panels without
/// a slot follow in activation priority order.
const PRIMARY_SIDE_BAR_ORDER: [PrimarySideBarSlot; 5] = [
    PrimarySideBarSlot::Panel(PROJECT_PANEL_KEY),
    PrimarySideBarSlot::Search,
    PrimarySideBarSlot::Panel(GIT_PANEL_KEY),
    PrimarySideBarSlot::Panel(DEBUG_PANEL_KEY),
    PrimarySideBarSlot::Extensions,
];

fn has_fixed_slot(panel: &Arc<dyn PanelHandle>) -> bool {
    PRIMARY_SIDE_BAR_ORDER.iter().any(|slot| {
        matches!(slot, PrimarySideBarSlot::Panel(panel_key) if *panel_key == panel.panel_key())
    })
}

/// Switches the primary side bar, which is the left dock. Panels in the right and
/// bottom docks are reached through the dock headers instead, as in VS Code.
pub struct ActivityBar {
    workspace: WeakEntity<Workspace>,
    left_dock: Entity<Dock>,
    user_store: Entity<UserStore>,
    _subscriptions: Vec<Subscription>,
}

impl ActivityBar {
    fn new(workspace: &Workspace, cx: &mut Context<Self>) -> Self {
        let left_dock = workspace.left_dock().clone();
        let user_store = workspace.user_store().clone();

        let subscriptions = vec![
            cx.observe(&left_dock, |_, _, cx| cx.notify()),
            cx.observe(&user_store, |_, _, cx| cx.notify()),
            cx.observe_global::<SettingsStore>(|_, cx| cx.notify()),
        ];

        Self {
            workspace: workspace.weak_handle(),
            left_dock,
            user_store,
            _subscriptions: subscriptions,
        }
    }

    fn entries(&self, cx: &App) -> Vec<ActivityBarEntry> {
        let left_panels = self
            .left_dock
            .read(cx)
            .panels()
            .cloned()
            .collect::<Vec<_>>();

        let mut entries = Vec::with_capacity(left_panels.len() + 2);
        for slot in PRIMARY_SIDE_BAR_ORDER {
            match slot {
                PrimarySideBarSlot::Panel(panel_key) => entries.extend(
                    left_panels
                        .iter()
                        .find(|panel| panel.panel_key() == panel_key)
                        .cloned()
                        .map(ActivityBarEntry::Panel),
                ),
                PrimarySideBarSlot::Search => entries.push(ActivityBarEntry::Search),
                PrimarySideBarSlot::Extensions => entries.push(ActivityBarEntry::Extensions),
            }
        }
        entries.extend(
            left_panels
                .into_iter()
                .filter(|panel| !has_fixed_slot(panel))
                .map(ActivityBarEntry::Panel),
        );
        entries
    }

    fn toggle_panel(
        &mut self,
        panel: &Arc<dyn PanelHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let (position, is_visible) = {
            let dock = self.left_dock.read(cx);
            (dock.position(), is_active_panel(dock, panel))
        };

        if is_visible {
            workspace.update(cx, |workspace, cx| {
                workspace.toggle_dock(position, window, cx);
            });
        } else {
            reveal_panel(&self.left_dock, panel, window, cx);
        }
    }

    fn render_panel_entry(
        &self,
        panel: Arc<dyn PanelHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let icon = panel.icon(window, cx)?;
        let label = panel
            .icon_tooltip(window, cx)
            .unwrap_or_else(|| panel.persistent_name());
        let is_active = is_active_panel(self.left_dock.read(cx), &panel);
        let badge = panel
            .icon_label(window, cx)
            .filter(|_| !is_active)
            .and_then(|icon_label| icon_label.parse::<usize>().ok());
        let toggle_action = panel.toggle_action(window, cx);

        // The active state is part of the id so the cached tooltip is rebuilt when the
        // panel is toggled from elsewhere, for example with a keybinding.
        let button = activity_bar_button((panel.panel_key(), is_active as u64), icon, is_active)
            .aria_label(label)
            .tooltip(move |_window, cx| Tooltip::for_action(label, &*toggle_action, cx))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.toggle_panel(&panel, window, cx);
            }));

        Some(render_entry_cell(is_active, button, badge, cx))
    }

    fn render_accounts_menu(&self, cx: &App) -> impl IntoElement {
        let username = self
            .user_store
            .read(cx)
            .current_user()
            .map(|user| user.username.clone());

        PopoverMenu::new("activity-bar-accounts-menu")
            .anchor(Anchor::BottomLeft)
            .attach(Anchor::BottomRight)
            .trigger_with_tooltip(
                activity_bar_button("activity-bar-accounts", IconName::Person, false)
                    .aria_label("Accounts"),
                Tooltip::text("Accounts"),
            )
            .menu(move |window, cx| {
                let username = username.clone();
                Some(ContextMenu::build(
                    window,
                    cx,
                    move |menu, _window, _cx| match username {
                        Some(username) => menu
                            .header(username)
                            .action("Sign Out", SignOut.boxed_clone()),
                        None => menu.action("Sign In", SignIn.boxed_clone()),
                    },
                ))
            })
    }
}

impl Render for ActivityBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let top_entries = self
            .entries(cx)
            .into_iter()
            .filter_map(|entry| match entry {
                ActivityBarEntry::Panel(panel) => self.render_panel_entry(panel, window, cx),
                ActivityBarEntry::Search => Some(render_action_entry(
                    "activity-bar-search",
                    IconName::MagnifyingGlass,
                    "Search",
                    DeploySearch::default().boxed_clone(),
                    cx,
                )),
                ActivityBarEntry::Extensions => Some(render_action_entry(
                    "activity-bar-extensions",
                    IconName::Blocks,
                    "Extensions",
                    zed_actions::Extensions::default().boxed_clone(),
                    cx,
                )),
            })
            .collect::<Vec<_>>();

        let colors = cx.theme().colors();

        v_flex()
            .id("activity-bar")
            .flex_none()
            .h_full()
            .w(ACTIVITY_BAR_WIDTH)
            .justify_between()
            .overflow_hidden()
            .bg(colors.title_bar_background)
            // Match the borders of the adjacent workspace element so the lines under
            // the title bar and above the status bar span the whole window.
            .border_t_1()
            .border_b_1()
            .border_r_1()
            .border_color(colors.border)
            .child(v_flex().min_h_0().overflow_hidden().children(top_entries))
            .child(
                v_flex()
                    .flex_none()
                    .child(render_entry_cell(
                        false,
                        self.render_accounts_menu(cx),
                        None,
                        cx,
                    ))
                    .child(render_entry_cell(false, render_manage_menu(), None, cx)),
            )
    }
}

fn activity_bar_button(id: impl Into<ElementId>, icon: IconName, is_active: bool) -> IconButton {
    IconButton::new(id, icon)
        .shape(IconButtonShape::Square)
        .style(ButtonStyle::Transparent)
        .icon_size(IconSize::Custom(rems_from_px(24_f32)))
        .icon_color(if is_active {
            Color::Default
        } else {
            Color::Muted
        })
        .hover_color(Color::Default)
}

fn render_entry_cell(
    is_active: bool,
    button: impl IntoElement,
    badge: Option<usize>,
    cx: &App,
) -> AnyElement {
    div()
        .relative()
        .flex()
        .flex_none()
        .w_full()
        .h(ACTIVITY_BAR_WIDTH)
        .items_center()
        .justify_center()
        .when(is_active, |this| {
            this.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .h_full()
                    .w(ACTIVE_INDICATOR_WIDTH)
                    .bg(cx.theme().colors().text_accent),
            )
        })
        .child(
            div()
                .relative()
                .child(button)
                .when_some(badge, |this, count| this.child(CountBadge::new(count))),
        )
        .into_any_element()
}

fn render_action_entry(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    action: Box<dyn Action>,
    cx: &App,
) -> AnyElement {
    let tooltip_action = action.boxed_clone();
    let button = activity_bar_button(id, icon, false)
        .aria_label(label)
        .tooltip(move |_window, cx| Tooltip::for_action(label, &*tooltip_action, cx))
        .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx));
    render_entry_cell(false, button, None, cx)
}

fn render_manage_menu() -> impl IntoElement {
    PopoverMenu::new("activity-bar-manage-menu")
        .anchor(Anchor::BottomLeft)
        .attach(Anchor::BottomRight)
        .trigger_with_tooltip(
            activity_bar_button("activity-bar-manage", IconName::Settings, false)
                .aria_label("Manage"),
            Tooltip::text("Manage"),
        )
        .menu(|window, cx| {
            Some(ContextMenu::build(window, cx, |menu, _window, _cx| {
                menu.action(
                    "Command Palette",
                    zed_actions::command_palette::Toggle.boxed_clone(),
                )
                .separator()
                .action("Settings", zed_actions::OpenSettings.boxed_clone())
                .action("Keymap", zed_actions::OpenKeymap.boxed_clone())
                .action(
                    "Themes…",
                    zed_actions::theme_selector::Toggle::default().boxed_clone(),
                )
                .action(
                    "Extensions",
                    zed_actions::Extensions::default().boxed_clone(),
                )
                .separator()
                .action("Use VS Code Layout", UseVsCodeLayout.boxed_clone())
                .separator()
                .action("Check for Updates", auto_update::Check.boxed_clone())
            }))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{EntityId, TestAppContext, VisualTestContext};
    use workspace::dock::{DockPosition, Panel as _};

    use crate::test_support::{
        AgentTestPanel, DebugTestPanel, ExplorerTestPanel, OutlineTestPanel,
        SourceControlTestPanel, TerminalTestPanel, init_test, open_workspace,
        update_workbench_settings,
    };

    #[derive(Debug, PartialEq)]
    enum EntryKind {
        Panel(EntityId),
        Search,
        Extensions,
    }

    struct TestStatusItem;

    impl Render for TestStatusItem {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            Empty
        }
    }

    impl StatusItemView for TestStatusItem {
        fn set_active_pane_item(
            &mut self,
            _active_pane_item: Option<&dyn ItemHandle>,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) {
        }

        fn hide_setting(&self, _cx: &App) -> Option<HideStatusItem> {
            None
        }
    }

    fn set_activity_bar_visible(visible: bool, cx: &mut VisualTestContext) {
        cx.update(|_window, cx| {
            update_workbench_settings(cx, |workbench| {
                workbench.activity_bar.get_or_insert_default().visible = Some(visible);
            });
        });
        cx.run_until_parked();
    }

    fn installed_activity_bar(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Option<Entity<ActivityBar>> {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .leading_item()
                .and_then(|item| item.downcast::<ActivityBar>().ok())
        })
    }

    fn entry_kinds(
        activity_bar: &Entity<ActivityBar>,
        cx: &mut VisualTestContext,
    ) -> Vec<EntryKind> {
        activity_bar.read_with(cx, |activity_bar, cx| {
            activity_bar
                .entries(cx)
                .into_iter()
                .map(|entry| match entry {
                    ActivityBarEntry::Panel(panel) => EntryKind::Panel(panel.panel_id()),
                    ActivityBarEntry::Search => EntryKind::Search,
                    ActivityBarEntry::Extensions => EntryKind::Extensions,
                })
                .collect()
        })
    }

    #[gpui::test]
    async fn test_activity_bar_replaces_panel_buttons(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace(cx).await;

        workspace.update_in(cx, |workspace, window, cx| {
            let test_item = cx.new(|_| TestStatusItem);
            workspace.status_bar().update(cx, |status_bar, cx| {
                status_bar.add_left_item(test_item, window, cx);
            });
        });

        let (initial_buttons_position, initial_item_position, initial_buttons) = workspace
            .read_with(cx, |workspace, cx| {
                let status_bar = workspace.status_bar().read(cx);
                (
                    status_bar.position_of_item::<PanelButtons>(),
                    status_bar.position_of_item::<TestStatusItem>(),
                    status_bar
                        .item_of_type::<PanelButtons>()
                        .map(|item| item.entity_id()),
                )
            });
        assert_eq!(initial_buttons_position, Some(0));
        assert_eq!(initial_item_position, Some(1));
        assert!(installed_activity_bar(&workspace, cx).is_none());

        set_activity_bar_visible(true, cx);
        assert!(installed_activity_bar(&workspace, cx).is_some());
        workspace.read_with(cx, |workspace, cx| {
            let status_bar = workspace.status_bar().read(cx);
            assert!(status_bar.item_of_type::<PanelButtons>().is_none());
            assert_eq!(
                status_bar.position_of_item::<HiddenPanelButtons>(),
                initial_buttons_position
            );
            assert_eq!(
                status_bar.position_of_item::<TestStatusItem>(),
                initial_item_position
            );
        });

        set_activity_bar_visible(false, cx);
        assert!(installed_activity_bar(&workspace, cx).is_none());
        workspace.read_with(cx, |workspace, cx| {
            let status_bar = workspace.status_bar().read(cx);
            assert!(status_bar.item_of_type::<HiddenPanelButtons>().is_none());
            assert_eq!(
                status_bar.position_of_item::<PanelButtons>(),
                initial_buttons_position
            );
            assert_eq!(
                status_bar.position_of_item::<TestStatusItem>(),
                initial_item_position
            );
            assert_eq!(
                status_bar
                    .item_of_type::<PanelButtons>()
                    .map(|item| item.entity_id()),
                initial_buttons
            );
        });
    }

    #[gpui::test]
    async fn test_activity_bar_installed_for_new_workspace(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| {
            update_workbench_settings(cx, |workbench| {
                workbench.activity_bar.get_or_insert_default().visible = Some(true);
            });
        });
        let (workspace, cx) = open_workspace(cx).await;

        assert!(installed_activity_bar(&workspace, cx).is_some());
        workspace.read_with(cx, |workspace, cx| {
            let status_bar = workspace.status_bar().read(cx);
            assert!(status_bar.item_of_type::<PanelButtons>().is_none());
        });
    }

    #[gpui::test]
    async fn test_activity_bar_toggles_panel_dock(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace(cx).await;
        set_activity_bar_visible(true, cx);

        let panel = ExplorerTestPanel::add(&workspace, DockPosition::Left, 100, cx);
        cx.run_until_parked();

        let activity_bar =
            installed_activity_bar(&workspace, cx).expect("activity bar should be installed");
        let left_dock = workspace.read_with(cx, |workspace, _| workspace.left_dock().clone());
        let panel_handle = left_dock
            .read_with(cx, |dock, _| {
                dock.panels()
                    .find(|handle| handle.panel_id() == panel.entity_id())
                    .cloned()
            })
            .expect("panel should be in the left dock");
        assert!(!left_dock.read_with(cx, |dock, _| dock.is_open()));

        activity_bar.update_in(cx, |activity_bar, window, cx| {
            activity_bar.toggle_panel(&panel_handle, window, cx);
        });
        cx.run_until_parked();
        left_dock.read_with(cx, |dock, _| {
            assert!(dock.is_open(), "clicking a hidden panel opens its dock");
            assert_eq!(
                dock.active_panel()
                    .map(|active_panel| active_panel.panel_id()),
                Some(panel.entity_id())
            );
        });

        activity_bar.update_in(cx, |activity_bar, window, cx| {
            activity_bar.toggle_panel(&panel_handle, window, cx);
        });
        cx.run_until_parked();
        assert!(
            !left_dock.read_with(cx, |dock, _| dock.is_open()),
            "clicking the visible panel closes its dock"
        );
    }

    #[gpui::test]
    async fn test_activity_bar_entry_order(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace(cx).await;
        set_activity_bar_visible(true, cx);

        let agent = AgentTestPanel::add(&workspace, DockPosition::Left, 5, cx);
        let outline = OutlineTestPanel::add(&workspace, DockPosition::Left, 10, cx);
        let debug = DebugTestPanel::add(&workspace, DockPosition::Left, 20, cx);
        let explorer = ExplorerTestPanel::add(&workspace, DockPosition::Left, 30, cx);
        let source_control = SourceControlTestPanel::add(&workspace, DockPosition::Left, 40, cx);
        TerminalTestPanel::add(&workspace, DockPosition::Bottom, 1, cx);
        cx.run_until_parked();

        let activity_bar =
            installed_activity_bar(&workspace, cx).expect("activity bar should be installed");
        assert_eq!(
            entry_kinds(&activity_bar, cx),
            vec![
                EntryKind::Panel(explorer.entity_id()),
                EntryKind::Search,
                EntryKind::Panel(source_control.entity_id()),
                EntryKind::Panel(debug.entity_id()),
                EntryKind::Extensions,
                EntryKind::Panel(agent.entity_id()),
                EntryKind::Panel(outline.entity_id()),
            ],
            "VS Code's containers come first in their fixed order, then the remaining \
             left dock panels by activation priority; other docks are left out"
        );

        explorer.update_in(cx, |panel, window, cx| {
            panel.set_position(DockPosition::Right, window, cx);
        });
        debug.update_in(cx, |panel, window, cx| {
            panel.set_position(DockPosition::Right, window, cx);
        });
        agent.update_in(cx, |panel, window, cx| {
            panel.set_position(DockPosition::Right, window, cx);
        });
        cx.run_until_parked();

        assert_eq!(
            entry_kinds(&activity_bar, cx),
            vec![
                EntryKind::Search,
                EntryKind::Panel(source_control.entity_id()),
                EntryKind::Extensions,
                EntryKind::Panel(outline.entity_id()),
            ],
            "panels moved out of the left dock disappear, Search and Extensions stay"
        );
    }
}
