use gpui::{
    Action, Anchor, AnyView, ClickEvent, Empty, Focusable as _, MouseButton, Role, TaskExt as _,
    WeakEntity, relative,
};
use settings::{Settings, SettingsStore};
use title_bar::TitleBar;
use ui::{ContextMenu, PopoverMenu, Tooltip, prelude::*};
use workspace::{GoBack, GoForward, ToggleFileFinder, ToggleProjectSymbols, Workspace};

use crate::{Codicon, WorkbenchSettings, action_button};

/// The search box is `38vw` wide in VS Code, capped at 600px
/// (`.command-center-center` in `titlebarpart.css`).
const SEARCH_BOX_WIDTH: f32 = 0.38;
const SEARCH_BOX_MAX_WIDTH: Pixels = px(600.);
const SEARCH_BOX_HEIGHT: Pixels = px(22.);
const SEARCH_BOX_MARGIN: Pixels = px(6.);
const SEARCH_BOX_RADIUS: Pixels = px(6.);
const GO_TO_LINE_ACTION: &str = "go_to_line::Toggle";

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|title_bar: &mut TitleBar, window, cx| {
        let Some(window) = window else {
            return;
        };
        apply_command_center(title_bar, window, cx);
        cx.observe_global_in::<SettingsStore>(window, apply_command_center)
            .detach();
    })
    .detach();
}

fn apply_command_center(
    title_bar: &mut TitleBar,
    _window: &mut Window,
    cx: &mut Context<TitleBar>,
) {
    let enabled = WorkbenchSettings::get_global(cx).command_center;
    let installed = title_bar
        .center_item()
        .is_some_and(|item| item.clone().downcast::<CommandCenter>().is_ok());
    if enabled == installed {
        return;
    }

    let command_center = enabled.then(|| {
        let workspace = title_bar.workspace().clone();
        AnyView::from(cx.new(|_| CommandCenter { workspace }))
    });
    title_bar.set_center_item(command_center, cx);
}

/// VS Code's command center: back and forward buttons next to a search box named
/// after the project, which opens the file finder.
pub struct CommandCenter {
    workspace: WeakEntity<Workspace>,
}

#[derive(Clone, Copy)]
enum NavigationDirection {
    Back,
    Forward,
}

impl CommandCenter {
    fn navigate(
        &mut self,
        direction: NavigationDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            let pane = workspace.active_pane().downgrade();
            let navigation = match direction {
                NavigationDirection::Back => workspace.go_back(pane, window, cx),
                NavigationDirection::Forward => workspace.go_forward(pane, window, cx),
            };
            navigation.detach_and_log_err(cx);
        });
    }

    fn render_navigation_button(
        &self,
        direction: NavigationDirection,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (id, codicon, label, action): (_, _, _, Box<dyn Action>) = match direction {
            NavigationDirection::Back => (
                "command-center-back",
                Codicon::ArrowLeft,
                "Go Back",
                GoBack.boxed_clone(),
            ),
            NavigationDirection::Forward => (
                "command-center-forward",
                Codicon::ArrowRight,
                "Go Forward",
                GoForward.boxed_clone(),
            ),
        };
        let icon_color = if enabled {
            Color::Default
        } else {
            Color::Disabled
        };

        action_button(id, codicon.icon().color(icon_color))
            .disabled(!enabled)
            .tab_index(0isize)
            .aria_label(label)
            .tooltip(move |_window, cx| Tooltip::for_action(label, &*action, cx))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.navigate(direction, window, cx);
            }))
    }

    fn render_search_box(&self, title: SharedString, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let hover_background = colors.element_hover;
        let hover_border = colors.border;

        h_flex()
            .id("command-center-search")
            .debug_selector(|| "command-center-search".to_string())
            .role(Role::Button)
            .aria_label(format!("Search {title}"))
            .w(relative(SEARCH_BOX_WIDTH))
            .max_w(SEARCH_BOX_MAX_WIDTH)
            .h(SEARCH_BOX_HEIGHT)
            .ml(SEARCH_BOX_MARGIN)
            .overflow_hidden()
            .rounded(SEARCH_BOX_RADIUS)
            .border_1()
            .border_color(colors.border_variant)
            .bg(colors.element_background)
            .cursor_pointer()
            .hover(move |style| style.bg(hover_background).border_color(hover_border))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .tooltip(|_window, cx| {
                Tooltip::for_action("Go to File", &ToggleFileFinder::default(), cx)
            })
            .on_click(|_: &ClickEvent, window, cx| {
                window.dispatch_action(ToggleFileFinder::default().boxed_clone(), cx);
            })
            // Equal space on both sides keeps the title centered next to the menu.
            .child(div().flex_1())
            .child(
                h_flex()
                    .flex_shrink(1.)
                    .min_w_0()
                    .gap_1()
                    .child(
                        Codicon::Search
                            .icon()
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(title)
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .justify_end()
                    .child(self.render_quick_access_menu()),
            )
    }

    fn render_quick_access_menu(&self) -> impl IntoElement {
        let workspace = self.workspace.clone();
        PopoverMenu::new("command-center-menu")
            .anchor(Anchor::TopRight)
            .trigger_with_tooltip(
                action_button(
                    "command-center-menu-button",
                    Codicon::ChevronDown.icon().color(Color::Muted),
                )
                .height(px(20.).into())
                .tab_index(0isize)
                .aria_label("Quick Access"),
                Tooltip::text("Quick Access"),
            )
            .menu(move |window, cx| {
                // Go to Line only reaches the editor from inside of it, and the
                // other actions bubble up from there to the workspace.
                let context = workspace.upgrade().map(|workspace| {
                    let workspace = workspace.read(cx);
                    workspace
                        .active_item(cx)
                        .map(|item| item.item_focus_handle(cx))
                        .unwrap_or_else(|| workspace.focus_handle(cx))
                });
                let go_to_line = match cx.build_action(GO_TO_LINE_ACTION, None) {
                    Ok(action) => Some(action),
                    Err(error) => {
                        log::error!("failed to build the {GO_TO_LINE_ACTION} action: {error}");
                        None
                    }
                };
                Some(ContextMenu::build(window, cx, move |menu, _window, _cx| {
                    menu.when_some(context, |menu, context| menu.context(context))
                        .action("Go to File", ToggleFileFinder::default().boxed_clone())
                        .action(
                            "Show and Run Commands",
                            zed_actions::command_palette::Toggle.boxed_clone(),
                        )
                        .action(
                            "Go to Symbol in Workspace",
                            ToggleProjectSymbols.boxed_clone(),
                        )
                        .when_some(go_to_line, |menu, go_to_line| {
                            menu.action("Go to Line/Column", go_to_line)
                        })
                }))
            })
    }
}

/// The name VS Code shows in the command center: the project's folders, or
/// a prompt to search when no folder is open.
fn project_title(workspace: &Workspace, cx: &App) -> SharedString {
    let names = workspace
        .project()
        .read(cx)
        .visible_worktrees(cx)
        .map(|worktree| worktree.read(cx).root_name_str().to_string())
        .collect::<Vec<_>>();
    if names.is_empty() {
        "Search".into()
    } else {
        names.join(", ").into()
    }
}

impl Render for CommandCenter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(workspace) = self.workspace.upgrade() else {
            return Empty.into_any_element();
        };
        let (can_go_back, can_go_forward, title) = {
            let workspace = workspace.read(cx);
            let pane = workspace.active_pane().read(cx);
            (
                pane.can_navigate_backward(),
                pane.can_navigate_forward(),
                project_title(workspace, cx),
            )
        };

        // Spans the whole title bar so the search box can size itself relative to
        // it. Only the controls themselves take mouse events, so the rest of the
        // title bar still drags the window.
        h_flex()
            .w_full()
            .justify_center()
            .child(
                h_flex()
                    .gap_1()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(self.render_navigation_button(
                        NavigationDirection::Back,
                        can_go_back,
                        cx,
                    ))
                    .child(self.render_navigation_button(
                        NavigationDirection::Forward,
                        can_go_forward,
                        cx,
                    )),
            )
            .child(self.render_search_box(title, cx))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use settings::TitleBarVisibility;

    use crate::test_support::{
        init_test, open_workspace_with_title_bar, title_bar, update_workbench_settings,
    };

    fn set_command_center(enabled: bool, cx: &mut VisualTestContext) {
        cx.update(|_window, cx| {
            update_workbench_settings(cx, |workbench| workbench.command_center = Some(enabled));
        });
        cx.run_until_parked();
    }

    fn installed_command_center(
        title_bar: &Entity<TitleBar>,
        cx: &mut VisualTestContext,
    ) -> Option<Entity<CommandCenter>> {
        title_bar.read_with(cx, |title_bar, _| {
            title_bar
                .center_item()
                .and_then(|item| item.clone().downcast::<CommandCenter>().ok())
        })
    }

    #[gpui::test]
    async fn test_command_center_follows_setting(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace_with_title_bar(cx).await;
        let title_bar = title_bar(&workspace, cx);
        assert!(installed_command_center(&title_bar, cx).is_none());
        assert!(cx.debug_bounds("command-center-search").is_none());

        set_command_center(true, cx);
        assert!(installed_command_center(&title_bar, cx).is_some());
        let search_box = cx
            .debug_bounds("command-center-search")
            .expect("the search box should be rendered");
        let window_width = cx.update(|window, _| window.viewport_size().width);
        assert!(search_box.size.width > px(0.));
        assert!(search_box.size.width <= SEARCH_BOX_MAX_WIDTH);
        // Window controls and the back and forward buttons shift it a little.
        let offset_from_center = search_box.center().x - window_width / 2.;
        assert!(
            offset_from_center.abs() < px(100.),
            "the search box should be centered in the title bar, but is {offset_from_center:?} off"
        );

        set_command_center(false, cx);
        assert!(installed_command_center(&title_bar, cx).is_none());
        assert!(cx.debug_bounds("command-center-search").is_none());
    }

    #[gpui::test]
    async fn test_command_center_survives_title_bar_rebuild(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace_with_title_bar(cx).await;
        set_command_center(true, cx);
        let first_title_bar = title_bar(&workspace, cx);

        for visibility in [TitleBarVisibility::Never, TitleBarVisibility::Always] {
            cx.update(|_window, cx| {
                cx.update_global::<SettingsStore, _>(|store, cx| {
                    store.update_user_settings(cx, |settings| {
                        settings.title_bar.get_or_insert_default().show = Some(visibility);
                    });
                });
            });
            cx.run_until_parked();
        }

        let second_title_bar = title_bar(&workspace, cx);
        assert_ne!(first_title_bar.entity_id(), second_title_bar.entity_id());
        assert!(installed_command_center(&second_title_bar, cx).is_some());
    }
}
