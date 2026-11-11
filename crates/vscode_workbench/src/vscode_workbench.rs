//! An optional VS Code style workbench layered on top of the Zed workspace.

mod activity_bar;
mod workbench_settings;

pub use activity_bar::ActivityBar;
pub use workbench_settings::{ActivityBarSettings, WorkbenchSettings};

use std::sync::Arc;

use gpui::{App, Entity, Focusable as _, Window};
use workspace::dock::{Dock, PanelHandle};

const PROJECT_PANEL_KEY: &str = "ProjectPanel";
const GIT_PANEL_KEY: &str = "GitPanel";
const DEBUG_PANEL_KEY: &str = "DebugPanel";

pub fn init(cx: &mut App) {
    activity_bar::init(cx);
}

fn is_active_panel(dock: &Dock, panel: &Arc<dyn PanelHandle>) -> bool {
    dock.is_open()
        && dock
            .active_panel()
            .is_some_and(|active_panel| active_panel.panel_id() == panel.panel_id())
}

/// Opens a panel through its own toggle action, like the status bar panel buttons
/// do, so focus, zoom and serialization behave exactly as with the panel's
/// keybinding. Focusing the dock first makes the action reach the workspace even
/// when focus is currently outside of it.
fn reveal_panel(
    dock: &Entity<Dock>,
    panel: &Arc<dyn PanelHandle>,
    window: &mut Window,
    cx: &mut App,
) {
    let focus_handle = {
        let dock = dock.read(cx);
        dock.zoomed_panel(window, cx)
            .map(|zoomed_panel| zoomed_panel.panel_focus_handle(cx))
            .unwrap_or_else(|| dock.focus_handle(cx))
    };
    window.focus(&focus_handle, cx);
    window.dispatch_action(panel.toggle_action(window, cx), cx);
}

#[cfg(test)]
mod test_support {
    use fs::FakeFs;
    use gpui::{
        Action, App, AppContext as _, BorrowAppContext as _, Context, Entity, EventEmitter,
        FocusHandle, Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
        Render, Styled as _, TestAppContext, VisualTestContext, Window, actions, div, px,
    };
    use project::Project;
    use settings::{SettingsStore, WorkbenchSettingsContent};
    use ui::IconName;
    use workspace::{
        MultiWorkspace, Workspace,
        dock::{DockPosition, Panel, PanelEvent},
    };

    pub(crate) fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            cx.set_global(db::AppDatabase::test_new());
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            crate::init(cx);
        });
    }

    pub(crate) async fn open_workspace(
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

    pub(crate) fn update_workbench_settings(
        cx: &mut App,
        update: impl FnOnce(&mut WorkbenchSettingsContent),
    ) {
        cx.update_global::<SettingsStore, _>(|store, cx| {
            store.update_user_settings(cx, |settings| {
                update(settings.workbench.get_or_insert_default());
            });
        });
    }

    /// Defines a panel type whose `panel_key` matches a real Zed panel, so the
    /// workbench treats it like that panel. Each type gets its own toggle action
    /// because panels are toggled by type.
    macro_rules! keyed_test_panel {
        ($panel:ident, $toggle:ident, $panel_key:literal, $icon:expr) => {
            actions!(vscode_workbench_test, [$toggle]);

            pub(crate) struct $panel {
                pub(crate) position: DockPosition,
                focus_handle: FocusHandle,
                activation_priority: u32,
            }

            impl $panel {
                pub(crate) fn add(
                    workspace: &Entity<Workspace>,
                    position: DockPosition,
                    activation_priority: u32,
                    cx: &mut VisualTestContext,
                ) -> Entity<Self> {
                    workspace.update_in(cx, |workspace, window, cx| {
                        let panel = cx.new(|cx| Self {
                            position,
                            focus_handle: cx.focus_handle(),
                            activation_priority,
                        });
                        workspace.add_panel(panel.clone(), window, cx);
                        workspace.register_action(|workspace, _: &$toggle, window, cx| {
                            workspace.toggle_panel_focus::<$panel>(window, cx);
                        });
                        panel
                    })
                }
            }

            impl EventEmitter<PanelEvent> for $panel {}

            impl Focusable for $panel {
                fn focus_handle(&self, _cx: &App) -> FocusHandle {
                    self.focus_handle.clone()
                }
            }

            impl Render for $panel {
                fn render(
                    &mut self,
                    _window: &mut Window,
                    _cx: &mut Context<Self>,
                ) -> impl IntoElement {
                    div()
                        .size_full()
                        .track_focus(&self.focus_handle)
                        .debug_selector(|| stringify!($panel).to_string())
                        .child($panel_key)
                        // Content taller than any dock, like a long file tree.
                        .child(div().h(px(10_000.)))
                }
            }

            impl Panel for $panel {
                fn persistent_name() -> &'static str {
                    stringify!($panel)
                }

                fn panel_key() -> &'static str {
                    $panel_key
                }

                fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
                    self.position
                }

                fn position_is_valid(&self, _position: DockPosition) -> bool {
                    true
                }

                fn set_position(
                    &mut self,
                    position: DockPosition,
                    _window: &mut Window,
                    cx: &mut Context<Self>,
                ) {
                    self.position = position;
                    // Real panels persist their position to the settings file. The
                    // docks react to the resulting settings change, so trigger one.
                    cx.update_global::<SettingsStore, _>(|_, _| {});
                }

                fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
                    px(300.)
                }

                fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
                    Some($icon)
                }

                fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
                    Some(stringify!($panel))
                }

                fn toggle_action(&self) -> Box<dyn Action> {
                    $toggle.boxed_clone()
                }

                fn activation_priority(&self) -> u32 {
                    self.activation_priority
                }
            }
        };
    }

    keyed_test_panel!(
        ExplorerTestPanel,
        ToggleExplorerTestPanel,
        "ProjectPanel",
        IconName::FileTree
    );
    keyed_test_panel!(
        SourceControlTestPanel,
        ToggleSourceControlTestPanel,
        "GitPanel",
        IconName::GitBranch
    );
    keyed_test_panel!(
        DebugTestPanel,
        ToggleDebugTestPanel,
        "DebugPanel",
        IconName::Debug
    );
    keyed_test_panel!(
        OutlineTestPanel,
        ToggleOutlineTestPanel,
        "OutlinePanel",
        IconName::ListTree
    );
    keyed_test_panel!(
        AgentTestPanel,
        ToggleAgentTestPanel,
        "agent_panel",
        IconName::ZedAgent
    );
    keyed_test_panel!(
        TerminalTestPanel,
        ToggleTerminalTestPanel,
        "TerminalPanel",
        IconName::Terminal
    );
}
