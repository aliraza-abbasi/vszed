//! An optional VS Code style workbench layered on top of the Zed workspace.

mod activity_bar;
mod command_center;
mod dock_header;
mod editor_status;
mod layout_controls;
mod preset;
mod status_items;
mod workbench_settings;

pub use activity_bar::ActivityBar;
pub use command_center::CommandCenter;
pub use dock_header::DockHeader;
pub use editor_status::{CursorPositionItem, IndentationItem, LanguageStatusItem};
pub use layout_controls::{LayoutControls, ToggleActivityBar};
pub use preset::{RestorePreviousLayout, UseVsCodeLayout};
pub use status_items::{GitBranchItem, NotificationBell, ProblemsItem, RemoteIndicator};
pub use workbench_settings::{ActivityBarSettings, WorkbenchSettings};

use std::sync::Arc;

use gpui::{App, Entity, Focusable as _, SharedString, Window, px};
use ui::{ButtonLike, Icon, IconSize, prelude::*};
use workspace::dock::{Dock, DockPosition, PanelHandle};

const PROJECT_PANEL_KEY: &str = "ProjectPanel";
const GIT_PANEL_KEY: &str = "GitPanel";
const DEBUG_PANEL_KEY: &str = "DebugPanel";
const OUTLINE_PANEL_KEY: &str = "OutlinePanel";
const COLLABORATION_PANEL_KEY: &str = "CollaborationPanel";
const AGENT_PANEL_KEY: &str = "agent_panel";
const TERMINAL_PANEL_KEY: &str = "TerminalPanel";

/// VS Code's toolbar action size (`.monaco-action-bar .action-label`), used for
/// the title bar and part title buttons.
const ACTION_BUTTON_SIZE: Pixels = px(22.);

pub fn init(cx: &mut App) {
    activity_bar::init(cx);
    dock_header::init(cx);
    preset::init(cx);
    command_center::init(cx);
    layout_controls::init(cx);
    status_items::init(cx);
}

/// VS Code's UI icons, embedded from `assets/icons/codicons`.
#[cfg_attr(test, derive(strum::EnumIter))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codicon {
    Account,
    ArrowLeft,
    ArrowRight,
    Bell,
    BellDot,
    ChatSparkle,
    ChevronUp,
    Close,
    CloudUpload,
    DebugAlt,
    Ellipsis,
    Error,
    Extensions,
    Files,
    GitBranch,
    GitCommit,
    Json,
    Layout,
    LayoutPanel,
    LayoutPanelOff,
    LayoutSidebarLeft,
    LayoutSidebarLeftOff,
    LayoutSidebarRight,
    LayoutSidebarRightOff,
    LiveShare,
    Loading,
    Remote,
    Search,
    SettingsGear,
    SourceControl,
    SymbolClass,
    Sync,
    Terminal,
    Warning,
}

impl Codicon {
    fn path(self) -> &'static str {
        match self {
            Self::Account => "icons/codicons/account.svg",
            Self::ArrowLeft => "icons/codicons/arrow-left.svg",
            Self::ArrowRight => "icons/codicons/arrow-right.svg",
            Self::Bell => "icons/codicons/bell.svg",
            Self::BellDot => "icons/codicons/bell-dot.svg",
            Self::ChatSparkle => "icons/codicons/chat-sparkle.svg",
            Self::ChevronUp => "icons/codicons/chevron-up.svg",
            Self::Close => "icons/codicons/close.svg",
            Self::CloudUpload => "icons/codicons/cloud-upload.svg",
            Self::DebugAlt => "icons/codicons/debug-alt.svg",
            Self::Ellipsis => "icons/codicons/ellipsis.svg",
            Self::Error => "icons/codicons/error.svg",
            Self::Extensions => "icons/codicons/extensions.svg",
            Self::Files => "icons/codicons/files.svg",
            Self::GitBranch => "icons/codicons/git-branch.svg",
            Self::GitCommit => "icons/codicons/git-commit.svg",
            Self::Json => "icons/codicons/json.svg",
            Self::Layout => "icons/codicons/layout.svg",
            Self::LayoutPanel => "icons/codicons/layout-panel.svg",
            Self::LayoutPanelOff => "icons/codicons/layout-panel-off.svg",
            Self::LayoutSidebarLeft => "icons/codicons/layout-sidebar-left.svg",
            Self::LayoutSidebarLeftOff => "icons/codicons/layout-sidebar-left-off.svg",
            Self::LayoutSidebarRight => "icons/codicons/layout-sidebar-right.svg",
            Self::LayoutSidebarRightOff => "icons/codicons/layout-sidebar-right-off.svg",
            Self::LiveShare => "icons/codicons/live-share.svg",
            Self::Loading => "icons/codicons/loading.svg",
            Self::Remote => "icons/codicons/remote.svg",
            Self::Search => "icons/codicons/search.svg",
            Self::SettingsGear => "icons/codicons/settings-gear.svg",
            Self::SourceControl => "icons/codicons/source-control.svg",
            Self::SymbolClass => "icons/codicons/symbol-class.svg",
            Self::Sync => "icons/codicons/sync.svg",
            Self::Terminal => "icons/codicons/terminal.svg",
            Self::Warning => "icons/codicons/warning.svg",
        }
    }

    fn icon(self) -> Icon {
        Icon::from_path(self.path())
    }
}

/// The icon of a panel's button: the codicon of the VS Code view the panel
/// stands for, or else the panel's own icon. `None` when the panel hides its
/// button.
fn panel_icon_path(panel: &dyn PanelHandle, window: &Window, cx: &App) -> Option<SharedString> {
    let icon = panel.icon(window, cx)?;
    let codicon = match panel.panel_key() {
        PROJECT_PANEL_KEY => Codicon::Files,
        GIT_PANEL_KEY => Codicon::SourceControl,
        DEBUG_PANEL_KEY => Codicon::DebugAlt,
        OUTLINE_PANEL_KEY => Codicon::SymbolClass,
        COLLABORATION_PANEL_KEY => Codicon::LiveShare,
        AGENT_PANEL_KEY => Codicon::ChatSparkle,
        TERMINAL_PANEL_KEY => Codicon::Terminal,
        _ => return Some(icon.path().into()),
    };
    Some(codicon.path().into())
}

/// A square toolbar button with a 16px icon, for icons that aren't an
/// `IconName`.
fn action_button(id: impl Into<ElementId>, icon: Icon) -> ButtonLike {
    ButtonLike::new(id)
        .width(ACTION_BUTTON_SIZE)
        .height(ACTION_BUTTON_SIZE.into())
        .child(icon.size(IconSize::Medium))
}

/// The VS Code name of the view a panel stands for, used for side bar titles and
/// bottom panel tabs.
fn panel_title(panel: &dyn PanelHandle, position: DockPosition) -> SharedString {
    let title = match panel.panel_key() {
        PROJECT_PANEL_KEY => "EXPLORER",
        GIT_PANEL_KEY => "SOURCE CONTROL",
        // Zed has a single debugger panel. VS Code calls it Run and Debug in a side
        // bar and Debug Console in the bottom panel.
        DEBUG_PANEL_KEY if position == DockPosition::Bottom => "DEBUG CONSOLE",
        DEBUG_PANEL_KEY => "RUN AND DEBUG",
        OUTLINE_PANEL_KEY => "OUTLINE",
        COLLABORATION_PANEL_KEY => "COLLABORATION",
        AGENT_PANEL_KEY => "AGENT",
        TERMINAL_PANEL_KEY => "TERMINAL",
        _ => return panel.persistent_name().to_uppercase().into(),
    };
    title.into()
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
    use title_bar::TitleBar;
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
        cx.run_until_parked();
        (workspace, cx)
    }

    /// Opens a workspace with Zed's title bar, which needs calls to be set up.
    pub(crate) async fn open_workspace_with_title_bar(
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, &mut VisualTestContext) {
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        cx.update(|cx| {
            command_palette_hooks::init(cx);
            let project = project.read(cx);
            call::init(project.client(), project.user_store(), cx);
            title_bar::init(cx);
        });
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
        cx.run_until_parked();
        (workspace, cx)
    }

    pub(crate) fn title_bar(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Entity<TitleBar> {
        workspace
            .read_with(cx, |workspace, _| {
                workspace
                    .titlebar_item()
                    .and_then(|item| item.downcast::<TitleBar>().ok())
            })
            .expect("the title bar should be shown")
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use strum::IntoEnumIterator as _;

    use super::Codicon;

    fn assets_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets")
    }

    #[test]
    fn test_codicons_exist() {
        for codicon in Codicon::iter() {
            let path = assets_dir().join(codicon.path());
            assert!(path.exists(), "{codicon:?} does not exist at {path:?}");
        }
    }

    #[test]
    fn test_only_used_codicons_are_bundled() {
        let codicons_dir = assets_dir().join("icons/codicons");
        let entries = std::fs::read_dir(&codicons_dir).expect("reading the codicons directory");
        for entry in entries {
            let path = entry.expect("reading a codicons directory entry").path();
            if path.extension().is_none_or(|extension| extension != "svg") {
                continue;
            }
            let file_name = path
                .file_name()
                .and_then(|file_name| file_name.to_str())
                .expect("codicon file names are UTF-8");
            let asset_path = format!("icons/codicons/{file_name}");
            assert!(
                Codicon::iter().any(|codicon| codicon.path() == asset_path),
                "{asset_path} is bundled but not used"
            );
        }
    }
}
