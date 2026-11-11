use std::sync::Arc;

use client::{SignIn, SignOut, UserStore};
use gpui::{Action, Anchor, ClickEvent, Empty, Entity, Focusable, Subscription, WeakEntity};
use settings::{Settings, SettingsStore};
use ui::{ContextMenu, CountBadge, IconButtonShape, PopoverMenu, Tooltip, prelude::*};
use workspace::{
    DeploySearch, HideStatusItem, ItemHandle, StatusItemView, Workspace,
    dock::{Dock, PanelButtons, PanelHandle},
};

use crate::WorkbenchSettings;

const ACTIVITY_BAR_WIDTH: Pixels = px(48.);
const ACTIVE_INDICATOR_WIDTH: Pixels = px(2.);
const PROJECT_PANEL_KEY: &str = "ProjectPanel";

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
    Panel {
        dock: Entity<Dock>,
        panel: Arc<dyn PanelHandle>,
    },
    Search,
    Extensions,
}

pub struct ActivityBar {
    workspace: WeakEntity<Workspace>,
    left_dock: Entity<Dock>,
    right_dock: Entity<Dock>,
    bottom_dock: Entity<Dock>,
    user_store: Entity<UserStore>,
    _subscriptions: Vec<Subscription>,
}

impl ActivityBar {
    fn new(workspace: &Workspace, cx: &mut Context<Self>) -> Self {
        let left_dock = workspace.left_dock().clone();
        let right_dock = workspace.right_dock().clone();
        let bottom_dock = workspace.bottom_dock().clone();
        let user_store = workspace.user_store().clone();

        let mut subscriptions = [&left_dock, &right_dock, &bottom_dock]
            .into_iter()
            .map(|dock| cx.observe(dock, |_, _, cx| cx.notify()))
            .collect::<Vec<_>>();
        subscriptions.push(cx.observe(&user_store, |_, _, cx| cx.notify()));
        subscriptions.push(cx.observe_global::<SettingsStore>(|_, cx| cx.notify()));

        Self {
            workspace: workspace.weak_handle(),
            left_dock,
            right_dock,
            bottom_dock,
            user_store,
            _subscriptions: subscriptions,
        }
    }

    fn entries(&self, cx: &App) -> Vec<ActivityBarEntry> {
        let mut entries = Vec::new();
        let mut search_index = 0;
        for panel in self.left_dock.read(cx).panels() {
            entries.push(ActivityBarEntry::Panel {
                dock: self.left_dock.clone(),
                panel: panel.clone(),
            });
            if panel.panel_key() == PROJECT_PANEL_KEY {
                search_index = entries.len();
            }
        }
        entries.insert(search_index, ActivityBarEntry::Search);
        entries.push(ActivityBarEntry::Extensions);

        for dock in [&self.right_dock, &self.bottom_dock] {
            entries.extend(dock.read(cx).panels().map(|panel| ActivityBarEntry::Panel {
                dock: dock.clone(),
                panel: panel.clone(),
            }));
        }
        entries
    }

    fn toggle_panel(
        &mut self,
        dock: &Entity<Dock>,
        panel: &Arc<dyn PanelHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let (position, is_visible, focus_handle) = {
            let dock = dock.read(cx);
            let focus_handle = dock
                .zoomed_panel(window, cx)
                .map(|zoomed_panel| zoomed_panel.panel_focus_handle(cx))
                .unwrap_or_else(|| dock.focus_handle(cx));
            (dock.position(), is_active_panel(dock, panel), focus_handle)
        };

        if is_visible {
            workspace.update(cx, |workspace, cx| {
                workspace.toggle_dock(position, window, cx);
            });
        } else {
            // Opening goes through the panel's own toggle action, like the status bar
            // panel buttons do, so focus, zoom and serialization behave exactly as with
            // the panel's keybinding. Focusing the dock first makes the action reach
            // the workspace even when focus is currently outside of it.
            window.focus(&focus_handle, cx);
            window.dispatch_action(panel.toggle_action(window, cx), cx);
        }
    }

    fn render_panel_entry(
        dock: Entity<Dock>,
        panel: Arc<dyn PanelHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let icon = panel.icon(window, cx)?;
        let label = panel
            .icon_tooltip(window, cx)
            .unwrap_or_else(|| panel.persistent_name());
        let is_active = is_active_panel(dock.read(cx), &panel);
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
                this.toggle_panel(&dock, &panel, window, cx);
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
                ActivityBarEntry::Panel { dock, panel } => {
                    Self::render_panel_entry(dock, panel, window, cx)
                }
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

fn is_active_panel(dock: &Dock, panel: &Arc<dyn PanelHandle>) -> bool {
    dock.is_open()
        && dock
            .active_panel()
            .is_some_and(|active_panel| active_panel.panel_id() == panel.panel_id())
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
                .action("Check for Updates", auto_update::Check.boxed_clone())
            }))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use gpui::{EntityId, TestAppContext, VisualTestContext};
    use project::Project;
    use settings::{ActivityBarSettingsContent, WorkbenchSettingsContent};
    use workspace::{
        MultiWorkspace,
        dock::{
            DockPosition,
            test::{TestPanel, ToggleTestPanel},
        },
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

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            cx.set_global(db::AppDatabase::test_new());
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            crate::init(cx);
        });
    }

    async fn open_workspace(
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, &mut VisualTestContext) {
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
        (workspace, cx)
    }

    fn set_activity_bar_visible(visible: bool, cx: &mut VisualTestContext) {
        cx.update(|_window, cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.workbench = Some(WorkbenchSettingsContent {
                        activity_bar: Some(ActivityBarSettingsContent {
                            visible: Some(visible),
                        }),
                    });
                });
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
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.workbench = Some(WorkbenchSettingsContent {
                        activity_bar: Some(ActivityBarSettingsContent {
                            visible: Some(true),
                        }),
                    });
                });
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

        let panel = workspace.update_in(cx, |workspace, window, cx| {
            let panel = cx.new(|cx| TestPanel::new(DockPosition::Left, 100, cx));
            workspace.add_panel(panel.clone(), window, cx);
            workspace.register_action(|workspace, _: &ToggleTestPanel, window, cx| {
                workspace.toggle_panel_focus::<TestPanel>(window, cx);
            });
            panel
        });
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
            activity_bar.toggle_panel(&left_dock, &panel_handle, window, cx);
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
            activity_bar.toggle_panel(&left_dock, &panel_handle, window, cx);
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

        let (left_panel, right_panel, bottom_panel) =
            workspace.update_in(cx, |workspace, window, cx| {
                let left_panel = cx.new(|cx| TestPanel::new(DockPosition::Left, 100, cx));
                let right_panel = cx.new(|cx| TestPanel::new(DockPosition::Right, 101, cx));
                let bottom_panel = cx.new(|cx| TestPanel::new(DockPosition::Bottom, 102, cx));
                workspace.add_panel(bottom_panel.clone(), window, cx);
                workspace.add_panel(right_panel.clone(), window, cx);
                workspace.add_panel(left_panel.clone(), window, cx);
                (left_panel, right_panel, bottom_panel)
            });
        cx.run_until_parked();

        let activity_bar =
            installed_activity_bar(&workspace, cx).expect("activity bar should be installed");
        let entries = activity_bar.read_with(cx, |activity_bar, cx| {
            activity_bar
                .entries(cx)
                .into_iter()
                .map(|entry| match entry {
                    ActivityBarEntry::Panel { panel, .. } => EntryKind::Panel(panel.panel_id()),
                    ActivityBarEntry::Search => EntryKind::Search,
                    ActivityBarEntry::Extensions => EntryKind::Extensions,
                })
                .collect::<Vec<_>>()
        });
        assert_eq!(
            entries,
            vec![
                EntryKind::Search,
                EntryKind::Panel(left_panel.entity_id()),
                EntryKind::Extensions,
                EntryKind::Panel(right_panel.entity_id()),
                EntryKind::Panel(bottom_panel.entity_id()),
            ]
        );
    }
}
