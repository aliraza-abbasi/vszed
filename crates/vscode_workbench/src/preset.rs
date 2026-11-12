use std::sync::Arc;

use anyhow::{Context as _, Result};
use db::kvp::KeyValueStore;
use extension_host::ExtensionStore;
use fs::Fs;
use futures::channel::oneshot;
use gpui::{App, AsyncApp, Context, Entity, Task, WeakEntity, Window, actions};
use serde::{Deserialize, Serialize};
use settings::{
    BaseKeymapContent, BottomDockLayout, DockPosition, DockSide, EncodingDisplayOptions,
    FontFamilyName, FontSize, IconThemeName, IconThemeSelection, MinimapThumb, SettingsContent,
    SettingsStore, ShowDiagnostics, ShowMinimap, TerminalDockPosition, ThemeAppearanceMode,
    ThemeName, ThemeSelection, update_settings_file_with_completion,
};
use util::ResultExt as _;
use workspace::{Toast, Workspace, notifications::NotificationId};

actions!(
    workbench,
    [
        /// Switches to a VS Code like layout: the VS Code keymap, docks, tabs, fonts,
        /// theme and file icons. The settings it replaces are remembered.
        UseVsCodeLayout,
        /// Restores the settings that were replaced by the VS Code layout.
        RestorePreviousLayout,
    ]
);

const PREVIOUS_LAYOUT_KEY: &str = "vscode_workbench_previous_layout";
const THEME_EXTENSION_ID: &str = "vscode-classic-themes";
const DARK_THEME: &str = "Dark 2026 (VS Code)";
const LIGHT_THEME: &str = "Light 2026 (VS Code)";
const ICON_THEME_EXTENSION_ID: &str = "seti-icons";
const ICON_THEME: &str = "Seti Icon Theme";

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace
            .register_action(use_vscode_layout)
            .register_action(restore_previous_layout);
    })
    .detach();
}

fn use_vscode_layout(
    workspace: &mut Workspace,
    _: &UseVsCodeLayout,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let fs = workspace.app_state().fs.clone();
    let apply_layout = apply_vscode_layout(fs.clone(), cx);
    cx.spawn(async move |workspace, cx| {
        let result = async {
            apply_layout
                .await
                .context("Failed to apply the VS Code layout")?;
            select_vscode_themes(fs, cx)
                .await
                .context("Applied the VS Code layout, but could not set up its theme")
        }
        .await;
        if let Err(error) = result {
            log::error!("{error:#}");
            show_toast::<UseVsCodeLayout>(&workspace, format!("{error:#}"), cx);
        }
    })
    .detach();
}

fn restore_previous_layout(
    workspace: &mut Workspace,
    _: &RestorePreviousLayout,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let restore = restore_layout(workspace.app_state().fs.clone(), cx);
    cx.spawn(async move |workspace, cx| {
        let message = match restore.await {
            Ok(true) => return,
            Ok(false) => "There is no previous layout to restore.".to_string(),
            Err(error) => {
                log::error!("failed to restore the previous layout: {error:#}");
                format!("Failed to restore the previous layout: {error:#}")
            }
        };
        show_toast::<RestorePreviousLayout>(&workspace, message, cx);
    })
    .detach();
}

fn show_toast<Source: 'static>(
    workspace: &WeakEntity<Workspace>,
    message: String,
    cx: &mut AsyncApp,
) {
    workspace
        .update(cx, |workspace, cx| {
            workspace.show_toast(Toast::new(NotificationId::unique::<Source>(), message), cx);
        })
        .log_err();
}

/// Whether `workbench: restore previous layout` has anything to restore.
pub(crate) fn has_previous_layout(cx: &App) -> bool {
    KeyValueStore::global(cx)
        .read_kvp(PREVIOUS_LAYOUT_KEY)
        .log_err()
        .flatten()
        .is_some()
}

/// Writes the VS Code layout to the user settings file after remembering the
/// user's own values for every key it changes.
fn apply_vscode_layout(fs: Arc<dyn Fs>, cx: &App) -> Task<Result<()>> {
    let key_value_store = KeyValueStore::global(cx);
    let current_layout = cx
        .global::<SettingsStore>()
        .raw_user_settings()
        .map(|user_settings| LayoutSnapshot::read_from(&user_settings.content))
        .unwrap_or_default();

    cx.spawn(async move |cx| {
        // Applying the layout again must not replace the user's original values
        // with the layout's own, so an existing snapshot is kept until restored.
        if key_value_store.read_kvp(PREVIOUS_LAYOUT_KEY)?.is_none() {
            let snapshot = serde_json::to_string(&current_layout)?;
            key_value_store
                .write_kvp(PREVIOUS_LAYOUT_KEY.to_string(), snapshot)
                .await?;
        }

        cx.update(|cx| {
            update_settings_file_with_completion(fs, cx, |settings, _cx| {
                let mut layout = LayoutSnapshot::read_from(settings);
                layout.use_vscode_values();
                layout.write_to(settings);
            })
        })
        .await
        .context("The settings update was cancelled")?
    })
}

/// Writes the remembered values back, removing keys that were unset before.
/// Resolves to `false` when there is nothing to restore.
fn restore_layout(fs: Arc<dyn Fs>, cx: &App) -> Task<Result<bool>> {
    let key_value_store = KeyValueStore::global(cx);
    cx.spawn(async move |cx| {
        let Some(snapshot) = key_value_store.read_kvp(PREVIOUS_LAYOUT_KEY)? else {
            return Ok(false);
        };
        let snapshot = serde_json::from_str::<LayoutSnapshot>(&snapshot)
            .context("Failed to read the remembered layout")?;

        cx.update(|cx| {
            update_settings_file_with_completion(fs, cx, move |settings, _cx| {
                snapshot.write_to(settings);
            })
        })
        .await
        .context("The settings update was cancelled")??;

        key_value_store
            .delete_kvp(PREVIOUS_LAYOUT_KEY.to_string())
            .await?;
        Ok(true)
    })
}

/// Installs the theme and icon theme extensions if needed and selects them. Each
/// one is only selected once its extension is installed.
async fn select_vscode_themes(fs: Arc<dyn Fs>, cx: &mut AsyncApp) -> Result<()> {
    let extension_store = cx
        .update(|cx| ExtensionStore::try_global(cx))
        .context("Extensions are not available")?;
    let theme_installed = install_extension(&extension_store, THEME_EXTENSION_ID, cx).await;
    let icon_theme_installed =
        install_extension(&extension_store, ICON_THEME_EXTENSION_ID, cx).await;

    let select_theme = theme_installed.is_ok();
    let select_icon_theme = icon_theme_installed.is_ok();
    if select_theme || select_icon_theme {
        cx.update(|cx| {
            update_settings_file_with_completion(fs, cx, move |settings, _cx| {
                if select_theme {
                    settings.theme.theme = Some(ThemeSelection::Dynamic {
                        mode: ThemeAppearanceMode::System,
                        light: ThemeName(LIGHT_THEME.into()),
                        dark: ThemeName(DARK_THEME.into()),
                    });
                }
                if select_icon_theme {
                    settings.theme.icon_theme =
                        Some(IconThemeSelection::Static(IconThemeName(ICON_THEME.into())));
                }
            })
        })
        .await
        .context("The settings update was cancelled")??;
    }

    theme_installed.and(icon_theme_installed)
}

/// Installs the latest version of an extension unless it is already installed,
/// and waits for the installation to finish.
async fn install_extension(
    extension_store: &Entity<ExtensionStore>,
    extension_id: &'static str,
    cx: &mut AsyncApp,
) -> Result<()> {
    let (finished_tx, finished_rx) = oneshot::channel();
    let subscription = cx.update(|cx| {
        if extension_store
            .read(cx)
            .installed_extensions()
            .contains_key(extension_id)
        {
            return Ok(None);
        }

        // The extension store reports installation failures only to the log, so an
        // install is over once its pending operation is gone, and it failed if the
        // extension still isn't installed by then.
        let mut finished_tx = Some(finished_tx);
        let subscription = cx.observe(extension_store, move |extension_store, cx| {
            let extension_store = extension_store.read(cx);
            if extension_store
                .outstanding_operations()
                .contains_key(extension_id)
            {
                return;
            }
            if let Some(finished_tx) = finished_tx.take() {
                let installed = extension_store
                    .installed_extensions()
                    .contains_key(extension_id);
                if finished_tx.send(installed).is_err() {
                    log::debug!("stopped waiting for the {extension_id} extension to install");
                }
            }
        });
        extension_store.update(cx, |extension_store, cx| {
            extension_store.install_latest_extension(extension_id.into(), cx);
        });
        anyhow::ensure!(
            extension_store
                .read(cx)
                .outstanding_operations()
                .contains_key(extension_id),
            "Could not start installing the {extension_id} extension"
        );
        Ok(Some(subscription))
    })?;

    let Some(_subscription) = subscription else {
        return Ok(());
    };
    let installed = finished_rx
        .await
        .with_context(|| format!("Stopped waiting for the {extension_id} extension"))?;
    anyhow::ensure!(
        installed,
        "Failed to install the {extension_id} extension, see the log for details"
    );
    Ok(())
}

/// The user's own value of every setting the VS Code layout changes. `None`
/// means the key isn't in the user's settings file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct LayoutSnapshot {
    base_keymap: Option<BaseKeymapContent>,
    project_panel_dock: Option<DockSide>,
    outline_panel_dock: Option<DockSide>,
    git_panel_dock: Option<DockPosition>,
    collaboration_panel_dock: Option<DockPosition>,
    debugger_dock: Option<DockPosition>,
    agent_dock: Option<DockPosition>,
    terminal_dock: Option<TerminalDockPosition>,
    bottom_dock_layout: Option<BottomDockLayout>,
    activity_bar_visible: Option<bool>,
    dock_headers: Option<bool>,
    command_center: Option<bool>,
    layout_controls: Option<bool>,
    status_bar: Option<bool>,
    title_bar_project_items: Option<bool>,
    title_bar_branch_name: Option<bool>,
    title_bar_sign_in: Option<bool>,
    title_bar_user_menu: Option<bool>,
    search_button: Option<bool>,
    tab_bar_nav_history_buttons: Option<bool>,
    tab_file_icons: Option<bool>,
    tab_git_status: Option<bool>,
    tab_diagnostics: Option<ShowDiagnostics>,
    minimap: Option<ShowMinimap>,
    minimap_thumb: Option<MinimapThumb>,
    status_bar_encoding_button: Option<EncodingDisplayOptions>,
    status_bar_line_endings_button: Option<bool>,
    breadcrumbs: Option<bool>,
    buffer_font_family: Option<FontFamilyName>,
    buffer_font_size: Option<FontSize>,
    ui_font_size: Option<FontSize>,
    theme: Option<ThemeSelection>,
    icon_theme: Option<IconThemeSelection>,
}

impl LayoutSnapshot {
    fn read_from(content: &SettingsContent) -> Self {
        let workbench = content.workbench.as_ref();
        let title_bar = content.title_bar.as_ref();
        let tab_bar = content.tab_bar.as_ref();
        let tabs = content.tabs.as_ref();
        let minimap = content.editor.minimap.as_ref();
        let status_bar = content.status_bar.as_ref();
        Self {
            base_keymap: content.base_keymap,
            project_panel_dock: content.project_panel.as_ref().and_then(|panel| panel.dock),
            outline_panel_dock: content.outline_panel.as_ref().and_then(|panel| panel.dock),
            git_panel_dock: content.git_panel.as_ref().and_then(|panel| panel.dock),
            collaboration_panel_dock: content
                .collaboration_panel
                .as_ref()
                .and_then(|panel| panel.dock),
            debugger_dock: content.debugger.as_ref().and_then(|debugger| debugger.dock),
            agent_dock: content.agent.as_ref().and_then(|agent| agent.dock),
            terminal_dock: content.terminal.as_ref().and_then(|terminal| terminal.dock),
            bottom_dock_layout: content.workspace.bottom_dock_layout,
            activity_bar_visible: workbench
                .and_then(|workbench| workbench.activity_bar.as_ref())
                .and_then(|activity_bar| activity_bar.visible),
            dock_headers: workbench.and_then(|workbench| workbench.dock_headers),
            command_center: workbench.and_then(|workbench| workbench.command_center),
            layout_controls: workbench.and_then(|workbench| workbench.layout_controls),
            status_bar: workbench.and_then(|workbench| workbench.status_bar),
            title_bar_project_items: title_bar.and_then(|title_bar| title_bar.show_project_items),
            title_bar_branch_name: title_bar.and_then(|title_bar| title_bar.show_branch_name),
            title_bar_sign_in: title_bar.and_then(|title_bar| title_bar.show_sign_in),
            title_bar_user_menu: title_bar.and_then(|title_bar| title_bar.show_user_menu),
            search_button: content
                .editor
                .search
                .as_ref()
                .and_then(|search| search.button),
            tab_bar_nav_history_buttons: tab_bar
                .and_then(|tab_bar| tab_bar.show_nav_history_buttons),
            tab_file_icons: tabs.and_then(|tabs| tabs.file_icons),
            tab_git_status: tabs.and_then(|tabs| tabs.git_status),
            tab_diagnostics: tabs.and_then(|tabs| tabs.show_diagnostics),
            minimap: minimap.and_then(|minimap| minimap.show),
            minimap_thumb: minimap.and_then(|minimap| minimap.thumb),
            status_bar_encoding_button: status_bar
                .and_then(|status_bar| status_bar.active_encoding_button),
            status_bar_line_endings_button: status_bar
                .and_then(|status_bar| status_bar.line_endings_button),
            breadcrumbs: content
                .editor
                .toolbar
                .as_ref()
                .and_then(|toolbar| toolbar.breadcrumbs),
            buffer_font_family: content.theme.buffer_font_family.clone(),
            buffer_font_size: content.theme.buffer_font_size,
            ui_font_size: content.theme.ui_font_size,
            theme: content.theme.theme.clone(),
            icon_theme: content.theme.icon_theme.clone(),
        }
    }

    fn write_to(&self, content: &mut SettingsContent) {
        content.base_keymap = self.base_keymap;
        update_nested(&mut content.project_panel, |panel| {
            panel.dock = self.project_panel_dock;
        });
        update_nested(&mut content.outline_panel, |panel| {
            panel.dock = self.outline_panel_dock;
        });
        update_nested(&mut content.git_panel, |panel| {
            panel.dock = self.git_panel_dock;
        });
        update_nested(&mut content.collaboration_panel, |panel| {
            panel.dock = self.collaboration_panel_dock;
        });
        update_nested(&mut content.debugger, |debugger| {
            debugger.dock = self.debugger_dock;
        });
        update_nested(&mut content.agent, |agent| agent.dock = self.agent_dock);
        update_nested(&mut content.terminal, |terminal| {
            terminal.dock = self.terminal_dock;
        });
        content.workspace.bottom_dock_layout = self.bottom_dock_layout;
        update_nested(&mut content.workbench, |workbench| {
            update_nested(&mut workbench.activity_bar, |activity_bar| {
                activity_bar.visible = self.activity_bar_visible;
            });
            workbench.dock_headers = self.dock_headers;
            workbench.command_center = self.command_center;
            workbench.layout_controls = self.layout_controls;
            workbench.status_bar = self.status_bar;
        });
        update_nested(&mut content.title_bar, |title_bar| {
            title_bar.show_project_items = self.title_bar_project_items;
            title_bar.show_branch_name = self.title_bar_branch_name;
            title_bar.show_sign_in = self.title_bar_sign_in;
            title_bar.show_user_menu = self.title_bar_user_menu;
        });
        update_nested(&mut content.editor.search, |search| {
            search.button = self.search_button;
        });
        update_nested(&mut content.tab_bar, |tab_bar| {
            tab_bar.show_nav_history_buttons = self.tab_bar_nav_history_buttons;
        });
        update_nested(&mut content.tabs, |tabs| {
            tabs.file_icons = self.tab_file_icons;
            tabs.git_status = self.tab_git_status;
            tabs.show_diagnostics = self.tab_diagnostics;
        });
        update_nested(&mut content.editor.minimap, |minimap| {
            minimap.show = self.minimap;
            minimap.thumb = self.minimap_thumb;
        });
        update_nested(&mut content.status_bar, |status_bar| {
            status_bar.active_encoding_button = self.status_bar_encoding_button;
            status_bar.line_endings_button = self.status_bar_line_endings_button;
        });
        update_nested(&mut content.editor.toolbar, |toolbar| {
            toolbar.breadcrumbs = self.breadcrumbs;
        });
        content.theme.buffer_font_family = self.buffer_font_family.clone();
        content.theme.buffer_font_size = self.buffer_font_size;
        content.theme.ui_font_size = self.ui_font_size;
        content.theme.theme = self.theme.clone();
        content.theme.icon_theme = self.icon_theme.clone();
    }

    /// Everything except the theme and icon theme, which are selected once their
    /// extensions are installed.
    fn use_vscode_values(&mut self) {
        self.base_keymap = Some(BaseKeymapContent::VSCode);
        self.project_panel_dock = Some(DockSide::Left);
        self.outline_panel_dock = Some(DockSide::Left);
        self.git_panel_dock = Some(DockPosition::Left);
        self.collaboration_panel_dock = Some(DockPosition::Left);
        self.debugger_dock = Some(DockPosition::Left);
        self.agent_dock = Some(DockPosition::Right);
        self.terminal_dock = Some(TerminalDockPosition::Bottom);
        self.bottom_dock_layout = Some(BottomDockLayout::Contained);
        self.activity_bar_visible = Some(true);
        self.dock_headers = Some(true);
        self.command_center = Some(true);
        self.layout_controls = Some(true);
        self.status_bar = Some(true);
        // VS Code's title bar shows neither, the status bar has the branch.
        self.title_bar_project_items = Some(false);
        self.title_bar_branch_name = Some(false);
        // The activity bar's Accounts and Manage menus have their entries.
        self.title_bar_sign_in = Some(false);
        self.title_bar_user_menu = Some(false);
        // Search is in the activity bar.
        self.search_button = Some(false);
        // The command center has back and forward buttons.
        self.tab_bar_nav_history_buttons = Some(false);
        self.tab_file_icons = Some(true);
        self.tab_git_status = Some(true);
        self.tab_diagnostics = Some(ShowDiagnostics::Errors);
        self.minimap = Some(ShowMinimap::Always);
        // VS Code's `editor.minimap.showSlider` default.
        self.minimap_thumb = Some(MinimapThumb::Hover);
        // VS Code always shows the encoding and line ending of the active file.
        self.status_bar_encoding_button = Some(EncodingDisplayOptions::Enabled);
        self.status_bar_line_endings_button = Some(true);
        self.breadcrumbs = Some(true);
        // VS Code's per-platform editor defaults, from its `fontInfo.ts`.
        let (buffer_font_family, buffer_font_size) = if cfg!(target_os = "macos") {
            ("Menlo", 12.)
        } else if cfg!(target_os = "windows") {
            ("Consolas", 14.)
        } else {
            ("Droid Sans Mono", 14.)
        };
        self.buffer_font_family = Some(FontFamilyName(buffer_font_family.into()));
        self.buffer_font_size = Some(FontSize(buffer_font_size));
        // Zed renders most UI labels at 0.875 of `ui_font_size`, so 15 gives
        // VS Code's 13px workbench text.
        self.ui_font_size = Some(FontSize(15.));
    }
}

/// Updates settings inside an optional settings object. An object left empty is
/// removed, so restoring previously unset keys leaves no `{}` behind.
fn update_nested<Parent: Default + PartialEq>(
    parent: &mut Option<Parent>,
    update: impl FnOnce(&mut Parent),
) {
    let parent_content = parent.get_or_insert_default();
    update(parent_content);
    if *parent_content == Parent::default() {
        *parent = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use gpui::{BorrowAppContext as _, TestAppContext};
    use serde_json::json;
    use settings::RootUserSettings as _;

    const ORIGINAL_SETTINGS: &str = r#"{
        "base_keymap": "JetBrains",
        "buffer_font_size": 15,
        "theme": "One Dark",
        "vim_mode": true,
        "project_panel": { "dock": "right", "default_width": 300 },
        "tabs": { "close_position": "left", "file_icons": false },
        "tab_bar": { "show": true, "show_nav_history_buttons": true },
        "search": { "button": true, "regex": true },
        "status_bar": { "line_endings_button": false, "cursor_position_button": true },
        "title_bar": { "show_branch_name": true, "show_user_picture": false }
    }"#;

    async fn init_settings(cx: &mut TestAppContext) -> Arc<FakeFs> {
        let fs = FakeFs::new(cx.executor());
        fs.save(
            paths::settings_file().as_path(),
            &ORIGINAL_SETTINGS.into(),
            Default::default(),
        )
        .await
        .expect("writing the settings file");
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            cx.set_global(db::AppDatabase::test_new());
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store
                    .set_user_settings(ORIGINAL_SETTINGS, cx)
                    .expect("parsing the settings file");
            });
        });
        fs
    }

    async fn load_settings_file(fs: &FakeFs) -> (serde_json::Value, SettingsContent) {
        let text = fs
            .load(paths::settings_file().as_path())
            .await
            .expect("reading the settings file");
        (
            settings::parse_json_with_comments::<serde_json::Value>(&text)
                .expect("settings file should be valid JSON"),
            SettingsContent::parse_json_with_comments(&text)
                .expect("settings file should be valid settings"),
        )
    }

    async fn apply(fs: &Arc<FakeFs>, cx: &mut TestAppContext) {
        let fs = fs.clone();
        cx.update(|cx| apply_vscode_layout(fs, cx))
            .await
            .expect("applying the VS Code layout");
        cx.run_until_parked();
    }

    async fn restore(fs: &Arc<FakeFs>, cx: &mut TestAppContext) -> bool {
        let fs = fs.clone();
        let restored = cx
            .update(|cx| restore_layout(fs, cx))
            .await
            .expect("restoring the previous layout");
        cx.run_until_parked();
        restored
    }

    #[gpui::test]
    async fn test_vscode_layout_apply_and_restore(cx: &mut TestAppContext) {
        let fs = init_settings(cx).await;
        let (_, original_settings) = load_settings_file(&fs).await;

        apply(&fs, cx).await;
        let (applied, _) = load_settings_file(&fs).await;
        assert_eq!(applied["base_keymap"], json!("VSCode"));
        assert_eq!(applied["project_panel"]["dock"], json!("left"));
        assert_eq!(applied["outline_panel"]["dock"], json!("left"));
        assert_eq!(applied["git_panel"]["dock"], json!("left"));
        assert_eq!(applied["collaboration_panel"]["dock"], json!("left"));
        assert_eq!(applied["debugger"]["dock"], json!("left"));
        assert_eq!(applied["agent"]["dock"], json!("right"));
        assert_eq!(applied["terminal"]["dock"], json!("bottom"));
        assert_eq!(applied["bottom_dock_layout"], json!("contained"));
        assert_eq!(applied["workbench"]["activity_bar"]["visible"], json!(true));
        assert_eq!(applied["workbench"]["dock_headers"], json!(true));
        assert_eq!(applied["workbench"]["command_center"], json!(true));
        assert_eq!(applied["workbench"]["layout_controls"], json!(true));
        assert_eq!(applied["workbench"]["status_bar"], json!(true));
        assert_eq!(applied["title_bar"]["show_project_items"], json!(false));
        assert_eq!(applied["title_bar"]["show_branch_name"], json!(false));
        assert_eq!(applied["title_bar"]["show_sign_in"], json!(false));
        assert_eq!(applied["title_bar"]["show_user_menu"], json!(false));
        assert_eq!(applied["search"]["button"], json!(false));
        assert_eq!(applied["tab_bar"]["show_nav_history_buttons"], json!(false));
        assert_eq!(applied["tabs"]["file_icons"], json!(true));
        assert_eq!(applied["tabs"]["git_status"], json!(true));
        assert_eq!(applied["tabs"]["show_diagnostics"], json!("errors"));
        assert_eq!(applied["minimap"]["show"], json!("always"));
        assert_eq!(applied["minimap"]["thumb"], json!("hover"));
        assert_eq!(
            applied["status_bar"]["active_encoding_button"],
            json!("enabled")
        );
        assert_eq!(applied["status_bar"]["line_endings_button"], json!(true));
        assert_eq!(applied["status_bar"]["cursor_position_button"], json!(true));
        assert_eq!(applied["toolbar"]["breadcrumbs"], json!(true));
        let (expected_font_family, expected_font_size) = if cfg!(target_os = "macos") {
            ("Menlo", 12.0)
        } else if cfg!(target_os = "windows") {
            ("Consolas", 14.0)
        } else {
            ("Droid Sans Mono", 14.0)
        };
        assert_eq!(applied["buffer_font_family"], json!(expected_font_family));
        assert_eq!(applied["buffer_font_size"], json!(expected_font_size));
        assert_eq!(applied["ui_font_size"], json!(15.0));
        assert_eq!(
            applied["theme"],
            json!("One Dark"),
            "the theme is only selected once its extension is installed"
        );
        assert_eq!(applied["vim_mode"], json!(true));
        assert_eq!(applied["project_panel"]["default_width"], json!(300));
        assert_eq!(applied["tabs"]["close_position"], json!("left"));
        assert_eq!(applied["title_bar"]["show_user_picture"], json!(false));
        assert_eq!(applied["search"]["regex"], json!(true));
        assert_eq!(applied["tab_bar"]["show"], json!(true));

        assert!(restore(&fs, cx).await);
        let (restored, restored_settings) = load_settings_file(&fs).await;
        assert_eq!(restored_settings, original_settings);
        assert_eq!(restored["search"]["button"], json!(true));
        assert_eq!(restored["tab_bar"]["show_nav_history_buttons"], json!(true));
        assert_eq!(restored["status_bar"]["line_endings_button"], json!(false));
        assert!(
            restored["status_bar"]
                .get("active_encoding_button")
                .is_none(),
            "status_bar.active_encoding_button was not set before the layout was applied: {restored:#}"
        );
        for key in ["show_sign_in", "show_user_menu"] {
            assert!(
                restored["title_bar"].get(key).is_none(),
                "title_bar.{key} was not set before the layout was applied: {restored:#}"
            );
        }
        for key in [
            "workbench",
            "minimap",
            "toolbar",
            "agent",
            "terminal",
            "debugger",
            "git_panel",
        ] {
            assert!(
                restored.get(key).is_none(),
                "{key} was not set before the layout was applied: {restored:#}"
            );
        }

        assert!(
            !restore(&fs, cx).await,
            "the remembered layout is used up by restoring it"
        );
    }

    #[gpui::test]
    async fn test_vscode_layout_keeps_first_snapshot(cx: &mut TestAppContext) {
        let fs = init_settings(cx).await;
        let (_, original_settings) = load_settings_file(&fs).await;

        apply(&fs, cx).await;
        {
            let fs = fs.clone();
            cx.update(|cx| {
                update_settings_file_with_completion(fs, cx, |settings, _cx| {
                    settings.base_keymap = Some(BaseKeymapContent::Atom);
                })
            })
            .await
            .expect("the settings update should finish")
            .expect("the settings file should be written");
        }
        apply(&fs, cx).await;

        assert!(restore(&fs, cx).await);
        let (_, restored_settings) = load_settings_file(&fs).await;
        assert_eq!(
            restored_settings, original_settings,
            "restoring after applying twice returns to the settings from before the first time"
        );
    }
}
