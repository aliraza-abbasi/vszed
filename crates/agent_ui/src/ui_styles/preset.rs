use std::sync::Arc;

use agent_servers::CLAUDE_AGENT_ID;
use anyhow::{Context as _, Result};
use db::kvp::KeyValueStore;
use fs::Fs;
use gpui::{App, AsyncApp, Context, Task, WeakEntity, Window, actions};
use serde::{Deserialize, Serialize};
use settings::{
    AgentThreadLocation, AgentUiStyle, CustomAgentServerSettings, SettingsContent, SettingsStore,
    update_settings_file_with_completion,
};
use util::ResultExt as _;
use workspace::{Toast, Workspace, notifications::NotificationId};

actions!(
    agent,
    [
        /// Switches agent threads to the Claude Code style, opened as editor tabs
        /// like the Claude Code extension. The settings it replaces are remembered.
        UseClaudeCodeStyle,
        /// Restores the agent settings that `agent: use Claude Code style` replaced.
        RestorePreviousUiStyle,
    ]
);

const PREVIOUS_STYLE_KEY: &str = "agent_ui_style_previous_settings";
/// The Claude Agent SDK labels the sessions claude-agent-acp starts `sdk-ts`
/// unless this is set, and the Claude Code extension's session list and
/// `claude --resume` hide SDK sessions. `cli` is the label of interactive
/// sessions outside VS Code, so sessions started here show up there too.
const ENTRYPOINT_VARIABLE: &str = "CLAUDE_CODE_ENTRYPOINT";
const ENTRYPOINT: &str = "cli";

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace
            .register_action(use_claude_code_style)
            .register_action(restore_previous_ui_style);
    })
    .detach();
}

fn use_claude_code_style(
    workspace: &mut Workspace,
    _: &UseClaudeCodeStyle,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let apply = apply_claude_code_style(workspace.app_state().fs.clone(), cx);
    cx.spawn(async move |workspace, cx| {
        if let Err(error) = apply.await {
            log::error!("failed to switch to the Claude Code style: {error:#}");
            show_toast::<UseClaudeCodeStyle>(
                &workspace,
                format!("Failed to switch to the Claude Code style: {error:#}"),
                cx,
            );
        }
    })
    .detach();
}

fn restore_previous_ui_style(
    workspace: &mut Workspace,
    _: &RestorePreviousUiStyle,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let restore = restore_ui_style(workspace.app_state().fs.clone(), cx);
    cx.spawn(async move |workspace, cx| {
        let message = match restore.await {
            Ok(true) => return,
            Ok(false) => "There is no previous agent style to restore.".to_string(),
            Err(error) => {
                log::error!("failed to restore the previous agent style: {error:#}");
                format!("Failed to restore the previous agent style: {error:#}")
            }
        };
        show_toast::<RestorePreviousUiStyle>(&workspace, message, cx);
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

/// Writes the Claude Code style to the user settings file after remembering the
/// user's own values for the keys it changes.
fn apply_claude_code_style(fs: Arc<dyn Fs>, cx: &App) -> Task<Result<()>> {
    let key_value_store = KeyValueStore::global(cx);
    let current_style = match cx.global::<SettingsStore>().raw_user_settings() {
        Some(user_settings) => StyleSnapshot::read_from(&user_settings.content),
        None => StyleSnapshot::read_from(&SettingsContent::default()),
    };

    cx.spawn(async move |cx| {
        // Applying the preset again must not replace the user's original values
        // with the preset's own, so an existing snapshot is kept until restored.
        if key_value_store.read_kvp(PREVIOUS_STYLE_KEY)?.is_none() {
            let snapshot = serde_json::to_string(&current_style)?;
            key_value_store
                .write_kvp(PREVIOUS_STYLE_KEY.to_string(), snapshot)
                .await?;
        }

        cx.update(|cx| {
            update_settings_file_with_completion(fs, cx, |settings, _cx| {
                StyleSnapshot::apply_claude_code_style(settings);
            })
        })
        .await
        .context("The settings update was cancelled")?
    })
}

/// Writes the remembered values back, removing keys that were unset before.
/// Resolves to `false` when there is nothing to restore.
fn restore_ui_style(fs: Arc<dyn Fs>, cx: &App) -> Task<Result<bool>> {
    let key_value_store = KeyValueStore::global(cx);
    cx.spawn(async move |cx| {
        let Some(snapshot) = key_value_store.read_kvp(PREVIOUS_STYLE_KEY)? else {
            return Ok(false);
        };
        let snapshot = serde_json::from_str::<StyleSnapshot>(&snapshot)
            .context("Failed to read the remembered agent style")?;

        cx.update(|cx| {
            update_settings_file_with_completion(fs, cx, move |settings, _cx| {
                snapshot.restore_to(settings);
            })
        })
        .await
        .context("The settings update was cancelled")??;

        key_value_store
            .delete_kvp(PREVIOUS_STYLE_KEY.to_string())
            .await?;
        Ok(true)
    })
}

/// The user's own value of every setting the preset changes. `None` means the
/// key isn't in the user's settings file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct StyleSnapshot {
    ui_style: Option<AgentUiStyle>,
    thread_location: Option<AgentThreadLocation>,
    /// `agent_servers.claude-acp.env.CLAUDE_CODE_ENTRYPOINT`. Snapshots from
    /// before the preset set it read as unset.
    claude_code_entrypoint: Option<String>,
    /// Whether `agent_servers.claude-acp` was in the settings file.
    claude_agent_server_configured: bool,
}

fn agent_server_env_mut(
    server: &mut CustomAgentServerSettings,
) -> &mut collections::HashMap<String, String> {
    match server {
        CustomAgentServerSettings::Custom { env, .. }
        | CustomAgentServerSettings::Registry { env, .. } => env,
    }
}

impl StyleSnapshot {
    fn read_from(content: &SettingsContent) -> Self {
        let agent = content.agent.as_ref();
        let claude_agent_server = content
            .agent_servers
            .as_ref()
            .and_then(|servers| servers.get(CLAUDE_AGENT_ID));
        Self {
            ui_style: agent.and_then(|agent| agent.ui_style),
            thread_location: agent.and_then(|agent| agent.thread_location),
            claude_code_entrypoint: claude_agent_server.and_then(|server| match server {
                CustomAgentServerSettings::Custom { env, .. }
                | CustomAgentServerSettings::Registry { env, .. } => {
                    env.get(ENTRYPOINT_VARIABLE).cloned()
                }
            }),
            claude_agent_server_configured: claude_agent_server.is_some(),
        }
    }

    fn apply_claude_code_style(content: &mut SettingsContent) {
        let agent = content.agent.get_or_insert_default();
        agent.ui_style = Some(AgentUiStyle::ClaudeCode);
        agent.thread_location = Some(AgentThreadLocation::Editor);

        let server = content
            .agent_servers
            .get_or_insert_default()
            .entry(CLAUDE_AGENT_ID.to_string())
            .or_insert_with(|| CustomAgentServerSettings::Registry {
                env: Default::default(),
                default_mode: None,
                default_config_options: Default::default(),
                favorite_config_option_values: Default::default(),
            });
        // A value the user chose for the variable stays.
        agent_server_env_mut(server)
            .entry(ENTRYPOINT_VARIABLE.to_string())
            .or_insert_with(|| ENTRYPOINT.to_string());
    }

    fn restore_to(&self, content: &mut SettingsContent) {
        let agent = content.agent.get_or_insert_default();
        agent.ui_style = self.ui_style;
        agent.thread_location = self.thread_location;
        // Restoring previously unset keys must not leave an empty `"agent": {}` behind.
        if *agent == Default::default() {
            content.agent = None;
        }

        // The preset leaves a value the user had alone, so only its own needs removing.
        if self.claude_code_entrypoint.is_some() {
            return;
        }
        let Some(servers) = content.agent_servers.as_mut() else {
            return;
        };
        let Some(server) = servers.get_mut(CLAUDE_AGENT_ID) else {
            return;
        };
        let env = agent_server_env_mut(server);
        // A value changed since then is the user's own.
        if env
            .get(ENTRYPOINT_VARIABLE)
            .is_some_and(|value| value == ENTRYPOINT)
        {
            env.remove(ENTRYPOINT_VARIABLE);
        }
        let added_by_preset = !self.claude_agent_server_configured
            && matches!(
                server,
                CustomAgentServerSettings::Registry {
                    env,
                    default_mode: None,
                    default_config_options,
                    favorite_config_option_values,
                } if env.is_empty()
                    && default_config_options.is_empty()
                    && favorite_config_option_values.is_empty()
            );
        if added_by_preset {
            servers.remove(CLAUDE_AGENT_ID);
        }
        if servers.is_empty() {
            content.agent_servers = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use gpui::{BorrowAppContext as _, TestAppContext};
    use serde_json::json;
    use settings::RootUserSettings as _;

    async fn init_settings(original_settings: &str, cx: &mut TestAppContext) -> Arc<FakeFs> {
        let fs = FakeFs::new(cx.executor());
        fs.save(
            paths::settings_file().as_path(),
            &original_settings.into(),
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
                    .set_user_settings(original_settings, cx)
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
        cx.update(|cx| apply_claude_code_style(fs, cx))
            .await
            .expect("applying the Claude Code style");
        cx.run_until_parked();
    }

    async fn restore(fs: &Arc<FakeFs>, cx: &mut TestAppContext) -> bool {
        let fs = fs.clone();
        let restored = cx
            .update(|cx| restore_ui_style(fs, cx))
            .await
            .expect("restoring the previous agent style");
        cx.run_until_parked();
        restored
    }

    #[gpui::test]
    async fn test_claude_code_style_apply_and_restore_unset(cx: &mut TestAppContext) {
        let fs = init_settings(r#"{ "vim_mode": true }"#, cx).await;
        let (_, original_settings) = load_settings_file(&fs).await;

        apply(&fs, cx).await;
        let (applied, _) = load_settings_file(&fs).await;
        assert_eq!(applied["agent"]["ui_style"], json!("claude_code"));
        assert_eq!(applied["agent"]["thread_location"], json!("editor"));
        assert_eq!(applied["vim_mode"], json!(true));

        assert!(restore(&fs, cx).await);
        let (restored, restored_settings) = load_settings_file(&fs).await;
        assert_eq!(restored_settings, original_settings);
        assert!(
            restored.get("agent").is_none(),
            "agent was not set before the style was applied: {restored:#}"
        );

        assert!(
            !restore(&fs, cx).await,
            "the remembered style is used up by restoring it"
        );
    }

    #[gpui::test]
    async fn test_claude_code_style_restores_previous_values(cx: &mut TestAppContext) {
        let fs = init_settings(
            r#"{
                "agent": {
                    "ui_style": "zed",
                    "thread_location": "panel",
                    "dock": "left"
                }
            }"#,
            cx,
        )
        .await;
        let (_, original_settings) = load_settings_file(&fs).await;

        apply(&fs, cx).await;
        let (applied, _) = load_settings_file(&fs).await;
        assert_eq!(applied["agent"]["ui_style"], json!("claude_code"));
        assert_eq!(applied["agent"]["thread_location"], json!("editor"));
        assert_eq!(applied["agent"]["dock"], json!("left"));

        assert!(restore(&fs, cx).await);
        let (restored, restored_settings) = load_settings_file(&fs).await;
        assert_eq!(restored_settings, original_settings);
        assert_eq!(restored["agent"]["ui_style"], json!("zed"));
        assert_eq!(restored["agent"]["thread_location"], json!("panel"));
        assert_eq!(restored["agent"]["dock"], json!("left"));
    }

    #[gpui::test]
    async fn test_claude_code_style_sets_entrypoint_in_empty_file(cx: &mut TestAppContext) {
        let fs = init_settings("{}", cx).await;
        let (_, original_settings) = load_settings_file(&fs).await;

        apply(&fs, cx).await;
        let (applied, _) = load_settings_file(&fs).await;
        assert_eq!(
            applied["agent_servers"]["claude-acp"],
            json!({ "type": "registry", "env": { "CLAUDE_CODE_ENTRYPOINT": "cli" } })
        );

        assert!(restore(&fs, cx).await);
        let (restored, restored_settings) = load_settings_file(&fs).await;
        assert_eq!(restored_settings, original_settings);
        assert!(
            restored.get("agent_servers").is_none(),
            "agent_servers was not set before the style was applied: {restored:#}"
        );
    }

    #[gpui::test]
    async fn test_claude_code_style_keeps_other_agent_server_settings(cx: &mut TestAppContext) {
        let fs = init_settings(
            r#"{
                "agent_servers": {
                    "claude-acp": {
                        "type": "registry",
                        "default_mode": "plan",
                        "env": { "ANTHROPIC_LOG": "debug" }
                    },
                    "codex-acp": { "type": "registry", "env": { "RUST_LOG": "info" } }
                }
            }"#,
            cx,
        )
        .await;
        let (original, original_settings) = load_settings_file(&fs).await;

        apply(&fs, cx).await;
        let (applied, _) = load_settings_file(&fs).await;
        assert_eq!(
            applied["agent_servers"]["claude-acp"],
            json!({
                "type": "registry",
                "default_mode": "plan",
                "env": { "ANTHROPIC_LOG": "debug", "CLAUDE_CODE_ENTRYPOINT": "cli" }
            })
        );
        assert_eq!(
            applied["agent_servers"]["codex-acp"],
            original["agent_servers"]["codex-acp"]
        );

        assert!(restore(&fs, cx).await);
        let (restored, restored_settings) = load_settings_file(&fs).await;
        assert_eq!(restored_settings, original_settings);
        assert_eq!(restored["agent_servers"], original["agent_servers"]);
    }

    #[gpui::test]
    async fn test_claude_code_style_keeps_user_entrypoint(cx: &mut TestAppContext) {
        let fs = init_settings(
            r#"{
                "agent_servers": {
                    "claude-acp": {
                        "type": "registry",
                        "env": { "CLAUDE_CODE_ENTRYPOINT": "sdk-ts" }
                    }
                }
            }"#,
            cx,
        )
        .await;
        let (original, original_settings) = load_settings_file(&fs).await;

        apply(&fs, cx).await;
        let (applied, _) = load_settings_file(&fs).await;
        assert_eq!(
            applied["agent_servers"]["claude-acp"]["env"]["CLAUDE_CODE_ENTRYPOINT"],
            json!("sdk-ts"),
            "the user's own value stays"
        );

        assert!(restore(&fs, cx).await);
        let (restored, restored_settings) = load_settings_file(&fs).await;
        assert_eq!(restored_settings, original_settings);
        assert_eq!(restored["agent_servers"], original["agent_servers"]);
    }

    #[gpui::test]
    async fn test_claude_code_style_keeps_first_snapshot(cx: &mut TestAppContext) {
        let fs = init_settings(r#"{ "agent": { "thread_location": "panel" } }"#, cx).await;
        let (_, original_settings) = load_settings_file(&fs).await;

        apply(&fs, cx).await;
        {
            let fs = fs.clone();
            cx.update(|cx| {
                update_settings_file_with_completion(fs, cx, |settings, _cx| {
                    settings.agent.get_or_insert_default().ui_style = Some(AgentUiStyle::Zed);
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
