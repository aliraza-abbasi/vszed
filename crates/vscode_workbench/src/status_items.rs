use std::time::Duration;

use diagnostics::items::DiagnosticIndicator;
use encoding_selector::ActiveBufferEncoding;
use gpui::{Action, Anchor, Empty, Entity, Subscription, Task, WeakEntity};
use language_selector::ActiveBufferLanguage;
use line_ending_selector::LineEndingIndicator;
use project::{DiagnosticSummary, Project, git_store::GitStoreEvent};
use remote::RemoteConnectionOptions;
use settings::{Settings, SettingsStore};
use toolchain_selector::ActiveToolchain;
use ui::{ButtonLike, ContextMenu, PopoverMenu, Tooltip, prelude::*};
use util::ResultExt as _;
use workspace::{
    ClearAllNotifications, HideStatusItem, ItemHandle, StatusItemView, Workspace,
    dock::PanelButtons,
};

use crate::{Codicon, WorkbenchSettings};

const MAX_BRANCH_NAME_LENGTH: usize = 40;
const SHORT_SHA_LENGTH: usize = 8;

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|_workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        // `zed::initialize_workspace` adds the editor items, and it observes new
        // workspaces after this crate does.
        cx.defer_in(window, apply_status_bar_items);
        cx.observe_global_in::<SettingsStore>(window, apply_status_bar_items)
            .detach();
    })
    .detach();
}

macro_rules! impl_status_item_view {
    ($item:ty) => {
        impl StatusItemView for $item {
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
    };
}

// The workbench items come and go with the `workbench.status_bar` setting, so
// they have no hide setting of their own.
impl_status_item_view!(RemoteIndicator);
impl_status_item_view!(GitBranchItem);
impl_status_item_view!(ProblemsItem);
impl_status_item_view!(NotificationBell);

fn apply_status_bar_items(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let enabled = WorkbenchSettings::get_global(cx).status_bar;
    let installed = workspace
        .status_bar()
        .read(cx)
        .item_of_type::<NotificationBell>()
        .is_some();
    if enabled == installed {
        return;
    }

    if enabled {
        let remote_indicator = cx.new(|_| RemoteIndicator::new(workspace));
        let git_branch = cx.new(|cx| GitBranchItem::new(workspace, cx));
        let problems = cx.new(|cx| ProblemsItem::new(workspace, cx));
        let notification_bell = cx.new(|_| NotificationBell::new(workspace));
        hide_status_items::<DiagnosticIndicator>(workspace, window, cx);
        let left_dock_buttons_position = left_dock_buttons_position(workspace, cx);
        workspace.status_bar().update(cx, |status_bar, cx| {
            match left_dock_buttons_position {
                // Inserted in reverse so that they end up in VS Code's order.
                Some(position) => {
                    status_bar.insert_item_after(position, problems, window, cx);
                    status_bar.insert_item_after(position, git_branch, window, cx);
                    status_bar.insert_item_after(position, remote_indicator, window, cx);
                }
                None => {
                    status_bar.add_left_item(remote_indicator, window, cx);
                    status_bar.add_left_item(git_branch, window, cx);
                    status_bar.add_left_item(problems, window, cx);
                }
            }
        });
        // Right side items are drawn in reverse order, so an item inserted after
        // another one shows up to its left.
        move_status_item_after::<ActiveBufferEncoding, LineEndingIndicator>(workspace, window, cx);
        move_status_item_after::<ActiveBufferLanguage, ActiveToolchain>(workspace, window, cx);
        workspace.status_bar().update(cx, |status_bar, cx| {
            status_bar.add_rightmost_item(notification_bell, window, cx);
            // VS Code starts the status bar with the remote indicator. The threads
            // sidebar stays reachable from the layout controls and its keybinding.
            status_bar.set_show_sidebar_toggle(false, cx);
        });
    } else {
        workspace.status_bar().update(cx, |status_bar, cx| {
            status_bar.set_show_sidebar_toggle(true, cx);
        });
        remove_status_items::<NotificationBell>(workspace, cx);
        remove_status_items::<ProblemsItem>(workspace, cx);
        remove_status_items::<GitBranchItem>(workspace, cx);
        remove_status_items::<RemoteIndicator>(workspace, cx);
        return_moved_status_item::<ActiveBufferLanguage>(workspace, window, cx);
        return_moved_status_item::<ActiveBufferEncoding>(workspace, window, cx);
        restore_status_items::<DiagnosticIndicator>(workspace, window, cx);
    }
}

/// Holds an item's slot in the status bar while the item is hidden or shown
/// elsewhere. The status bar can only insert an item after an existing one, so
/// keeping a placeholder in the slot is what lets the item return to its
/// original position.
pub(crate) struct StatusItemPlaceholder<T> {
    item: Entity<T>,
}

impl<T: 'static> Render for StatusItemPlaceholder<T> {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

impl<T: 'static> StatusItemView for StatusItemPlaceholder<T> {
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

/// Replaces every status bar item of type `T` with a placeholder in the same
/// slot.
pub(crate) fn hide_status_items<T: StatusItemView>(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut App,
) {
    workspace.status_bar().update(cx, |status_bar, cx| {
        while let Some(position) = status_bar.position_of_item::<T>()
            && let Some(item) = status_bar.item_of_type::<T>()
        {
            let placeholder = cx.new(|_| StatusItemPlaceholder { item });
            status_bar.insert_item_after(position, placeholder, window, cx);
            status_bar.remove_item_at(position, cx);
        }
    });
}

/// Puts the items hidden by [`hide_status_items`] back into their slots.
pub(crate) fn restore_status_items<T: StatusItemView>(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut App,
) {
    workspace.status_bar().update(cx, |status_bar, cx| {
        while let Some(position) = status_bar.position_of_item::<StatusItemPlaceholder<T>>()
            && let Some(placeholder) = status_bar.item_of_type::<StatusItemPlaceholder<T>>()
        {
            let item = placeholder.read(cx).item.clone();
            status_bar.insert_item_after(position, item, window, cx);
            status_bar.remove_item_at(position, cx);
        }
    });
}

/// Moves the status bar item of type `T` right after the item of type `Anchor`,
/// leaving a placeholder in its original slot.
fn move_status_item_after<T: StatusItemView, Anchor: StatusItemView>(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(item) = workspace.status_bar().read(cx).item_of_type::<T>() else {
        return;
    };
    if workspace
        .status_bar()
        .read(cx)
        .position_of_item::<Anchor>()
        .is_none()
    {
        return;
    }
    hide_status_items::<T>(workspace, window, cx);
    workspace.status_bar().update(cx, |status_bar, cx| {
        if let Some(anchor_position) = status_bar.position_of_item::<Anchor>() {
            status_bar.insert_item_after(anchor_position, item, window, cx);
        }
    });
}

/// Undoes [`move_status_item_after`].
fn return_moved_status_item<T: StatusItemView>(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut App,
) {
    let was_moved = workspace
        .status_bar()
        .read(cx)
        .position_of_item::<StatusItemPlaceholder<T>>()
        .is_some();
    if was_moved {
        remove_status_items::<T>(workspace, cx);
        restore_status_items::<T>(workspace, window, cx);
    }
}

fn remove_status_items<T: StatusItemView>(workspace: &Workspace, cx: &mut App) {
    workspace.status_bar().update(cx, |status_bar, cx| {
        while let Some(position) = status_bar.position_of_item::<T>() {
            status_bar.remove_item_at(position, cx);
        }
    });
}

/// The left dock's panel buttons are the first status bar item, or their
/// placeholder while the activity bar replaces them.
fn left_dock_buttons_position(workspace: &Workspace, cx: &App) -> Option<usize> {
    let status_bar = workspace.status_bar().read(cx);
    [
        status_bar.position_of_item::<PanelButtons>(),
        status_bar.position_of_item::<StatusItemPlaceholder<PanelButtons>>(),
    ]
    .into_iter()
    .flatten()
    .min()
}

/// VS Code's remote indicator: an accent colored button at the start of the
/// status bar that names the remote host and offers ways to open remote projects.
pub struct RemoteIndicator {
    project: Entity<Project>,
}

impl RemoteIndicator {
    fn new(workspace: &Workspace) -> Self {
        Self {
            project: workspace.project().clone(),
        }
    }

    fn remote_name(&self, cx: &App) -> Option<SharedString> {
        let options = self.project.read(cx).remote_connection_options(cx)?;
        let kind = if matches!(options, RemoteConnectionOptions::Ssh(_)) {
            "SSH"
        } else if matches!(options, RemoteConnectionOptions::Wsl(_)) {
            "WSL"
        } else if matches!(options, RemoteConnectionOptions::Docker(_)) {
            "Dev Container"
        } else {
            "Remote"
        };
        Some(format!("{kind}: {}", options.display_name()).into())
    }
}

impl Render for RemoteIndicator {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let remote_name = self.remote_name(cx);
        let is_local = self.project.read(cx).is_local();
        let colors = cx.theme().colors();
        let background = colors.text_accent;
        let foreground = Color::Custom(colors.status_bar_background);
        let tooltip = remote_name
            .clone()
            .unwrap_or_else(|| "Open a Remote Window".into());

        div()
            .id("vscode-remote-indicator")
            .rounded_sm()
            .bg(background)
            .hover(move |style| style.bg(background.opacity(0.85)))
            .child(
                PopoverMenu::new("vscode-remote-menu")
                    .anchor(Anchor::BottomLeft)
                    .attach(Anchor::TopLeft)
                    .trigger_with_tooltip(
                        ButtonLike::new("vscode-remote-indicator-button")
                            .style(ButtonStyle::Transparent)
                            .tab_index(0isize)
                            .aria_label(tooltip.clone())
                            .child(
                                h_flex()
                                    .gap_1()
                                    .px_1()
                                    .child(
                                        Codicon::Remote
                                            .icon()
                                            .size(IconSize::Small)
                                            .color(foreground),
                                    )
                                    .children(remote_name.clone().map(|remote_name| {
                                        Label::new(remote_name)
                                            .size(LabelSize::Small)
                                            .color(foreground)
                                    })),
                            ),
                        Tooltip::text(tooltip),
                    )
                    .menu(move |window, cx| {
                        let remote_name = remote_name.clone();
                        Some(ContextMenu::build(window, cx, move |menu, _window, _cx| {
                            let menu = match remote_name {
                                Some(remote_name) => menu.header(remote_name).separator(),
                                None => menu,
                            };
                            let menu = menu.action(
                                "Open Remote Project…",
                                zed_actions::OpenRemote::default().boxed_clone(),
                            );
                            // Zed only opens dev containers from local projects.
                            let menu = menu.when(is_local, |menu| {
                                menu.action(
                                    "Open Dev Container…",
                                    zed_actions::OpenDevContainer.boxed_clone(),
                                )
                            });
                            #[cfg(target_os = "windows")]
                            let menu = menu.action(
                                "Connect to WSL…",
                                zed_actions::wsl_actions::OpenWsl::default().boxed_clone(),
                            );
                            menu
                        }))
                    }),
            )
    }
}

/// The current branch with its sync state, like VS Code's Git status items.
pub struct GitBranchItem {
    project: Entity<Project>,
    _git_store_subscription: Subscription,
}

impl GitBranchItem {
    fn new(workspace: &Workspace, cx: &mut Context<Self>) -> Self {
        let project = workspace.project().clone();
        let git_store = project.read(cx).git_store().clone();
        let git_store_subscription = cx.subscribe(&git_store, |_, _, event, cx| match event {
            GitStoreEvent::ActiveRepositoryChanged(_)
            | GitStoreEvent::RepositoryUpdated(_, _, true) => cx.notify(),
            _ => {}
        });
        Self {
            project,
            _git_store_subscription: git_store_subscription,
        }
    }
}

impl Render for GitBranchItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(repository) = self.project.read(cx).active_repository(cx) else {
            return Empty.into_any_element();
        };
        let repository = repository.read(cx);
        let branch_name = match &repository.branch {
            Some(branch) => util::truncate_and_trailoff(branch.name(), MAX_BRANCH_NAME_LENGTH),
            None => match &repository.head_commit {
                Some(commit) => commit
                    .sha
                    .get(..SHORT_SHA_LENGTH)
                    .unwrap_or(commit.sha.as_ref())
                    .to_string(),
                None => return Empty.into_any_element(),
            },
        };
        let tracking = repository
            .branch
            .as_ref()
            .and_then(|branch| branch.upstream.as_ref())
            .and_then(|upstream| upstream.tracking.status());
        let has_branch = repository.branch.is_some();

        let sync_counts = tracking
            .filter(|status| status.behind > 0 || status.ahead > 0)
            .map(|status| format!("{}↓ {}↑", status.behind, status.ahead));
        // VS Code syncs by pulling and then pushing. Zed has no single action for
        // that, so this runs whichever one the branch needs first.
        let (sync_icon, sync_label, sync_action): (_, &'static str, Box<dyn Action>) =
            match tracking {
                None => (
                    Codicon::CloudUpload,
                    "Publish Branch",
                    git::Push.boxed_clone(),
                ),
                Some(status) if status.behind > 0 => {
                    (Codicon::Sync, "Pull", git::Pull.boxed_clone())
                }
                Some(status) if status.ahead > 0 => {
                    (Codicon::Sync, "Push", git::Push.boxed_clone())
                }
                Some(_) => (Codicon::Sync, "Fetch", git::Fetch.boxed_clone()),
            };
        let sync_tooltip_action = sync_action.boxed_clone();

        h_flex()
            .gap_0p5()
            .child(
                ButtonLike::new("vscode-git-branch")
                    .tab_index(0isize)
                    .aria_label(format!("Branch {branch_name}"))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(Codicon::GitBranch.icon().size(IconSize::Small))
                            .child(Label::new(branch_name).size(LabelSize::Small)),
                    )
                    .tooltip(|_window, cx| {
                        Tooltip::for_action("Switch Branch", &zed_actions::git::Branch, cx)
                    })
                    .on_click(|_, window, cx| {
                        window.dispatch_action(zed_actions::git::Branch.boxed_clone(), cx);
                    }),
            )
            .when(has_branch, |this| {
                this.child(
                    ButtonLike::new("vscode-git-sync")
                        .tab_index(0isize)
                        .aria_label(sync_label)
                        .child(
                            h_flex()
                                .gap_1()
                                .child(sync_icon.icon().size(IconSize::Small))
                                .children(sync_counts.map(|sync_counts| {
                                    Label::new(sync_counts).size(LabelSize::Small)
                                })),
                        )
                        .tooltip(move |_window, cx| {
                            Tooltip::for_action(sync_label, &*sync_tooltip_action, cx)
                        })
                        .on_click(move |_, window, cx| {
                            window.dispatch_action(sync_action.boxed_clone(), cx);
                        }),
                )
            })
            .into_any_element()
    }
}

/// Error and warning counts that open the project diagnostics, shown even when
/// there are none, like VS Code's problems item.
pub struct ProblemsItem {
    summary: DiagnosticSummary,
    summary_update: Task<()>,
    _project_subscription: Subscription,
}

impl ProblemsItem {
    fn new(workspace: &Workspace, cx: &mut Context<Self>) -> Self {
        let project = workspace.project().clone();
        let project_subscription = cx.subscribe(&project, |this, project, event, cx| match event {
            project::Event::DiskBasedDiagnosticsFinished { .. }
            | project::Event::LanguageServerRemoved(_) => {
                this.summary = project.read(cx).diagnostic_summary(false, cx);
                cx.notify();
            }
            // Updates arrive in bursts while typing, so the summary, which walks
            // every path with diagnostics, is computed once they settle.
            project::Event::DiagnosticsUpdated { .. } => {
                this.summary_update = cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(30))
                        .await;
                    this.update(cx, |this, cx| {
                        this.summary = project.read(cx).diagnostic_summary(false, cx);
                        cx.notify();
                    })
                    .log_err();
                });
            }
            _ => {}
        });

        Self {
            summary: project.read(cx).diagnostic_summary(false, cx),
            summary_update: Task::ready(()),
            _project_subscription: project_subscription,
        }
    }
}

impl Render for ProblemsItem {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let error_count = self.summary.error_count;
        let warning_count = self.summary.warning_count;
        let label = format!("Errors: {error_count}, Warnings: {warning_count}");

        ButtonLike::new("vscode-problems")
            .tab_index(0isize)
            .aria_label(label.clone())
            .child(
                h_flex()
                    .gap_1()
                    .child(Codicon::Error.icon().size(IconSize::Small))
                    .child(Label::new(error_count.to_string()).size(LabelSize::Small))
                    .child(Codicon::Warning.icon().size(IconSize::Small))
                    .child(Label::new(warning_count.to_string()).size(LabelSize::Small)),
            )
            .tooltip(move |_window, cx| {
                Tooltip::for_action(label.clone(), &diagnostics::Deploy, cx)
            })
            .on_click(|_, window, cx| {
                window.dispatch_action(diagnostics::Deploy.boxed_clone(), cx);
            })
    }
}

/// VS Code's notifications bell. Zed has no notification center to open, so a
/// click dismisses the notifications that are shown.
pub struct NotificationBell {
    workspace: WeakEntity<Workspace>,
}

impl NotificationBell {
    fn new(workspace: &Workspace) -> Self {
        Self {
            workspace: workspace.weak_handle(),
        }
    }

    fn clear_notifications(&mut self, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| workspace.clear_all_notifications(cx));
        }
    }
}

impl Render for NotificationBell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_notifications = self
            .workspace
            .upgrade()
            .is_some_and(|workspace| workspace.read(cx).has_notifications());
        let (icon, label) = if has_notifications {
            (Codicon::BellDot, "Clear All Notifications")
        } else {
            (Codicon::Bell, "No Notifications")
        };

        // The state is part of the id so the cached tooltip is rebuilt when it
        // changes.
        ButtonLike::new(("vscode-notifications", has_notifications as u64))
            .tab_index(0isize)
            .aria_label(label)
            .child(icon.icon().size(IconSize::Small))
            .tooltip(move |_window, cx| {
                if has_notifications {
                    Tooltip::for_action(label, &ClearAllNotifications, cx)
                } else {
                    Tooltip::simple(label, cx)
                }
            })
            .on_click(cx.listener(|this, _, _window, cx| this.clear_notifications(cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use workspace::{Toast, notifications::NotificationId};

    use crate::test_support::{init_test, open_workspace, update_workbench_settings};

    struct OtherStatusItem;

    impl Render for OtherStatusItem {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            Empty
        }
    }

    impl_status_item_view!(OtherStatusItem);

    /// Adds the items whose order the workbench changes the way
    /// `zed::initialize_workspace` does, around the dock buttons that the
    /// workspace adds itself.
    fn add_editor_items(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        workspace.update_in(cx, |workspace, window, cx| {
            let diagnostics = cx.new(|cx| DiagnosticIndicator::new(workspace, cx));
            let other_item = cx.new(|_| OtherStatusItem);
            let encoding = cx.new(|_| ActiveBufferEncoding::new(workspace));
            let language = cx.new(|_| ActiveBufferLanguage::new(workspace));
            let toolchain = cx.new(|cx| ActiveToolchain::new(workspace, window, cx));
            let line_ending = cx.new(|_| LineEndingIndicator::default());
            workspace.status_bar().update(cx, |status_bar, cx| {
                status_bar.add_left_item(diagnostics, window, cx);
                status_bar.add_left_item(other_item, window, cx);
                status_bar.add_right_item(encoding, window, cx);
                status_bar.add_right_item(language, window, cx);
                status_bar.add_right_item(toolchain, window, cx);
                status_bar.add_right_item(line_ending, window, cx);
            });
        });
    }

    #[derive(Debug, PartialEq)]
    struct ItemPositions {
        left_dock_buttons: Option<usize>,
        hidden_left_dock_buttons: Option<usize>,
        remote_indicator: Option<usize>,
        git_branch: Option<usize>,
        problems: Option<usize>,
        diagnostics: Option<usize>,
        hidden_diagnostics: Option<usize>,
        other_item: Option<usize>,
        notification_bell: Option<usize>,
        encoding: Option<usize>,
        language: Option<usize>,
        toolchain: Option<usize>,
        line_ending: Option<usize>,
    }

    fn item_positions(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> ItemPositions {
        workspace.read_with(cx, |workspace, cx| {
            let status_bar = workspace.status_bar().read(cx);
            ItemPositions {
                left_dock_buttons: status_bar.position_of_item::<PanelButtons>(),
                hidden_left_dock_buttons: status_bar
                    .position_of_item::<StatusItemPlaceholder<PanelButtons>>(),
                remote_indicator: status_bar.position_of_item::<RemoteIndicator>(),
                git_branch: status_bar.position_of_item::<GitBranchItem>(),
                problems: status_bar.position_of_item::<ProblemsItem>(),
                diagnostics: status_bar.position_of_item::<DiagnosticIndicator>(),
                hidden_diagnostics: status_bar
                    .position_of_item::<StatusItemPlaceholder<DiagnosticIndicator>>(),
                other_item: status_bar.position_of_item::<OtherStatusItem>(),
                notification_bell: status_bar.position_of_item::<NotificationBell>(),
                encoding: status_bar.position_of_item::<ActiveBufferEncoding>(),
                language: status_bar.position_of_item::<ActiveBufferLanguage>(),
                toolchain: status_bar.position_of_item::<ActiveToolchain>(),
                line_ending: status_bar.position_of_item::<LineEndingIndicator>(),
            }
        })
    }

    fn set_status_bar(enabled: bool, cx: &mut VisualTestContext) {
        cx.update(|_window, cx| {
            update_workbench_settings(cx, |workbench| workbench.status_bar = Some(enabled));
        });
        cx.run_until_parked();
    }

    fn set_activity_bar(visible: bool, cx: &mut VisualTestContext) {
        cx.update(|_window, cx| {
            update_workbench_settings(cx, |workbench| {
                workbench.activity_bar.get_or_insert_default().visible = Some(visible);
            });
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn test_status_bar_items_follow_setting(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace(cx).await;
        add_editor_items(&workspace, cx);
        let original_positions = item_positions(&workspace, cx);
        // Left: dock buttons, diagnostics, other item. Right, drawn in reverse:
        // right and bottom dock buttons, encoding, language, toolchain, line ending.
        assert_eq!(
            original_positions,
            ItemPositions {
                left_dock_buttons: Some(0),
                hidden_left_dock_buttons: None,
                remote_indicator: None,
                git_branch: None,
                problems: None,
                diagnostics: Some(1),
                hidden_diagnostics: None,
                other_item: Some(2),
                notification_bell: None,
                encoding: Some(5),
                language: Some(6),
                toolchain: Some(7),
                line_ending: Some(8),
            }
        );

        set_status_bar(true, cx);
        assert_eq!(
            item_positions(&workspace, cx),
            ItemPositions {
                left_dock_buttons: Some(0),
                hidden_left_dock_buttons: None,
                remote_indicator: Some(1),
                git_branch: Some(2),
                problems: Some(3),
                diagnostics: None,
                hidden_diagnostics: Some(4),
                other_item: Some(5),
                // The first right side item, so it is drawn last, after the right
                // and bottom dock buttons (7 and 8).
                notification_bell: Some(6),
                // Placeholders for encoding and language hold 9 and 10. Drawn from
                // left to right: encoding, line ending, language, toolchain.
                toolchain: Some(11),
                language: Some(12),
                line_ending: Some(13),
                encoding: Some(14),
            }
        );

        set_status_bar(false, cx);
        assert_eq!(item_positions(&workspace, cx), original_positions);
    }

    #[gpui::test]
    async fn test_status_bar_hides_sidebar_toggle(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace(cx).await;
        let shows_sidebar_toggle = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace.status_bar().read(cx).show_sidebar_toggle()
            })
        };
        assert!(shows_sidebar_toggle(cx));

        set_status_bar(true, cx);
        assert!(!shows_sidebar_toggle(cx));

        set_status_bar(false, cx);
        assert!(shows_sidebar_toggle(cx));
    }

    #[gpui::test]
    async fn test_status_bar_items_with_activity_bar(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace(cx).await;
        add_editor_items(&workspace, cx);
        let original_positions = item_positions(&workspace, cx);

        set_activity_bar(true, cx);
        set_status_bar(true, cx);
        let positions = item_positions(&workspace, cx);
        assert_eq!(positions.left_dock_buttons, None);
        assert_eq!(positions.hidden_left_dock_buttons, Some(0));
        assert_eq!(positions.remote_indicator, Some(1));
        assert_eq!(positions.git_branch, Some(2));
        assert_eq!(positions.problems, Some(3));
        assert_eq!(positions.notification_bell, Some(6));

        // Turned off in the order they were turned on in, which leaves each one
        // restoring items while the other one's are still in place.
        set_activity_bar(false, cx);
        set_status_bar(false, cx);
        assert_eq!(item_positions(&workspace, cx), original_positions);
    }

    #[gpui::test]
    async fn test_status_bar_items_installed_for_new_workspace(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| {
            update_workbench_settings(cx, |workbench| workbench.status_bar = Some(true));
        });
        let (workspace, cx) = open_workspace(cx).await;
        let positions = item_positions(&workspace, cx);
        assert_eq!(positions.remote_indicator, Some(1));
        assert!(positions.notification_bell.is_some());
    }

    #[gpui::test]
    async fn test_notification_bell_clears_notifications(cx: &mut TestAppContext) {
        init_test(cx);
        let (workspace, cx) = open_workspace(cx).await;
        set_status_bar(true, cx);
        let notification_bell = workspace
            .read_with(cx, |workspace, cx| {
                workspace
                    .status_bar()
                    .read(cx)
                    .item_of_type::<NotificationBell>()
            })
            .expect("the bell should be installed");

        workspace.update(cx, |workspace, cx| {
            workspace.show_toast(
                Toast::new(NotificationId::unique::<NotificationBell>(), "Hello"),
                cx,
            );
        });
        assert!(workspace.read_with(cx, |workspace, _| workspace.has_notifications()));

        notification_bell.update(cx, |notification_bell, cx| {
            notification_bell.clear_notifications(cx)
        });
        assert!(!workspace.read_with(cx, |workspace, _| workspace.has_notifications()));
    }
}
