//! Notifies when upstream Zed publishes a stable release that is newer than this build.
//!
//! vszed is built from source with Zed's auto-updater disabled (`ZED_UPDATE_EXPLANATION`),
//! because the stable channel's updater would replace it with official Zed. This crate only
//! tells the user that a newer upstream release exists. It never downloads or installs anything.

use std::{sync::Arc, time::Duration};

use anyhow::{Context as _, anyhow};
use db::kvp::KeyValueStore;
use futures::AsyncReadExt as _;
use gpui::{
    App, AppContext as _, Context, Entity, Global, Subscription, Task, WeakEntity, actions,
};
use http_client::{
    AsyncBody, HttpClient, HttpRequestExt as _, RedirectPolicy, Request, StatusCode,
};
use release_channel::AppVersion;
use semver::{BuildMetadata, Version};
use serde::Deserialize;
use settings::{RegisterSetting, Settings, SettingsStore};
use ui::IconName;
use util::ResultExt as _;
use workspace::{
    MultiWorkspace, Toast, Workspace,
    notifications::{NotificationId, simple_message_notification::MessageNotification},
};

const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/zed-industries/zed/releases/latest";
const USER_AGENT: &str = "vszed-upstream-release-notifier";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const SKIPPED_VERSION_KEY: &str = "upstream_release_notifier_skipped_version";

// An existing namespace, because zed's `test_action_namespaces` lists every namespace and
// adding one would need another edit to an upstream file.
actions!(
    zed,
    [
        /// Checks GitHub for a stable Zed release that is newer than this build.
        /// Only shows a notification: nothing is downloaded or installed.
        CheckForUpstreamRelease
    ]
);

/// Automatic checks follow the existing `auto_update` setting instead of a setting of their
/// own: with `ZED_UPDATE_EXPLANATION` set, that setting has nothing else to switch.
#[derive(Clone, Copy, Debug, RegisterSetting)]
struct UpstreamReleaseCheckSetting(bool);

impl Settings for UpstreamReleaseCheckSetting {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        Self(content.auto_update.unwrap_or(true))
    }
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct UpstreamRelease {
    version: Version,
    url: String,
}

#[derive(Debug)]
enum CheckError {
    RateLimited(StatusCode),
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for CheckError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

impl CheckError {
    fn toast_message(&self) -> &'static str {
        match self {
            Self::RateLimited(_) => "GitHub's rate limit was reached. Try again later.",
            Self::Failed(_) => "Couldn't check for upstream Zed releases. See the log for details.",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Notify,
    UpToDate,
    AlreadyHandled,
}

enum CheckTrigger {
    Automatic,
    Manual(WeakEntity<Workspace>),
}

struct UpstreamReleaseNotifier {
    current_version: Version,
    /// The newest release shown during this run, so a dismissed notification stays dismissed
    /// until the next start.
    notified_version: Option<Version>,
    polling: Option<Task<()>>,
    _settings_subscription: Subscription,
}

struct GlobalUpstreamReleaseNotifier(Entity<UpstreamReleaseNotifier>);

impl Global for GlobalUpstreamReleaseNotifier {}

struct UpstreamReleaseNotification;
struct UpToDateToast;
struct CheckFailedToast;

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|_workspace, _: &CheckForUpstreamRelease, _window, cx| {
            check_now(cx);
        });
    })
    .detach();

    let current_version = AppVersion::global(cx);
    let notifier = cx.new(|cx| UpstreamReleaseNotifier::new(current_version, cx));
    cx.set_global(GlobalUpstreamReleaseNotifier(notifier));
}

fn check_now(cx: &mut Context<Workspace>) {
    let Some(notifier) = cx
        .try_global::<GlobalUpstreamReleaseNotifier>()
        .map(|global| global.0.clone())
    else {
        return;
    };
    let workspace = cx.weak_entity();
    notifier
        .update(cx, |notifier, cx| {
            notifier.check(CheckTrigger::Manual(workspace), cx)
        })
        .detach();
}

impl UpstreamReleaseNotifier {
    fn new(current_version: Version, cx: &mut Context<Self>) -> Self {
        let settings_subscription =
            cx.observe_global::<SettingsStore>(|notifier, cx| notifier.update_polling(cx));
        let mut notifier = Self {
            current_version: without_build_metadata(&current_version),
            notified_version: None,
            polling: None,
            _settings_subscription: settings_subscription,
        };
        notifier.update_polling(cx);
        notifier
    }

    fn update_polling(&mut self, cx: &mut Context<Self>) {
        let enabled = UpstreamReleaseCheckSetting::try_get(cx).is_some_and(|setting| setting.0);
        if !enabled {
            self.polling = None;
        } else if self.polling.is_none() {
            self.polling = Some(Self::poll(cx));
        }
    }

    fn poll(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIRST_CHECK_DELAY).await;
            loop {
                let Ok(check) = this.update(cx, |notifier, cx| {
                    notifier.check(CheckTrigger::Automatic, cx)
                }) else {
                    break;
                };
                check.await;
                cx.background_executor().timer(CHECK_INTERVAL).await;
            }
        })
    }

    fn check(&self, trigger: CheckTrigger, cx: &mut Context<Self>) -> Task<()> {
        let http_client = cx.http_client();
        let fetch = cx.background_spawn(fetch_latest_release(http_client));
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |notifier, cx| {
                notifier.finish_check(result, trigger, cx)
            })
            .log_err();
        })
    }

    fn finish_check(
        &mut self,
        result: Result<Option<UpstreamRelease>, CheckError>,
        trigger: CheckTrigger,
        cx: &mut Context<Self>,
    ) {
        let release = match result {
            Ok(Some(release)) => release,
            Ok(None) => {
                log::info!("upstream release check: the latest upstream release is not stable");
                show_manual_check_toast::<CheckFailedToast>(
                    &trigger,
                    "Couldn't find a stable upstream Zed release.".to_string(),
                    cx,
                );
                return;
            }
            Err(error) => {
                match &error {
                    CheckError::RateLimited(status) => log::info!(
                        "upstream release check: GitHub rate limit reached ({status}), retrying at the next check"
                    ),
                    CheckError::Failed(error) => {
                        log::warn!("upstream release check failed: {error:#}")
                    }
                }
                show_manual_check_toast::<CheckFailedToast>(
                    &trigger,
                    error.toast_message().to_string(),
                    cx,
                );
                return;
            }
        };

        let skipped_version = read_skipped_version(cx);
        let decision = decide(
            &release.version,
            &self.current_version,
            skipped_version.as_ref(),
            self.notified_version.as_ref(),
            matches!(trigger, CheckTrigger::Manual(_)),
        );
        match decision {
            Decision::UpToDate => show_manual_check_toast::<UpToDateToast>(
                &trigger,
                format!("Zed v{} is the latest stable release.", release.version),
                cx,
            ),
            Decision::AlreadyHandled => log::debug!(
                "upstream release check: v{} was already skipped or shown",
                release.version
            ),
            Decision::Notify => {
                let workspace = match &trigger {
                    CheckTrigger::Automatic => active_workspace(cx),
                    CheckTrigger::Manual(workspace) => workspace.upgrade(),
                };
                let Some(workspace) = workspace else {
                    log::info!(
                        "upstream release check: no open workspace to notify, retrying at the next check"
                    );
                    return;
                };
                show_release_notification(&workspace, &release, &self.current_version, cx);
                self.notified_version = self.notified_version.take().max(Some(release.version));
            }
        }
    }
}

async fn fetch_latest_release(
    http_client: Arc<dyn HttpClient>,
) -> Result<Option<UpstreamRelease>, CheckError> {
    let request = Request::get(LATEST_RELEASE_URL)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", USER_AGENT)
        .follow_redirects(RedirectPolicy::FollowAll)
        .timeout(REQUEST_TIMEOUT)
        .body(AsyncBody::empty())
        .context("building the release request")?;
    let mut response = http_client
        .send(request)
        .await
        .context("requesting the latest release")?;

    let status = response.status();
    // GitHub answers 403 or 429 once the hourly quota for unauthenticated requests is used up.
    if status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS {
        return Err(CheckError::RateLimited(status));
    }
    if !status.is_success() {
        return Err(anyhow!("unexpected response status {status}").into());
    }

    let mut body = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut body)
        .await
        .context("reading the latest release")?;
    let release: GithubRelease =
        serde_json::from_slice(&body).context("parsing the latest release")?;
    parse_release(release).map_err(CheckError::Failed)
}

/// Returns `None` for drafts and prereleases, which never trigger a notification.
fn parse_release(release: GithubRelease) -> anyhow::Result<Option<UpstreamRelease>> {
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let tag = release.tag_name.as_str();
    let version = Version::parse(tag.strip_prefix('v').unwrap_or(tag))
        .with_context(|| format!("unexpected release tag {tag:?}"))?;
    if !version.pre.is_empty() {
        return Ok(None);
    }
    // The URL is opened in the browser, so only accept GitHub pages.
    anyhow::ensure!(
        release.html_url.starts_with("https://github.com/"),
        "unexpected release URL {:?}",
        release.html_url
    );
    Ok(Some(UpstreamRelease {
        version: without_build_metadata(&version),
        url: release.html_url,
    }))
}

fn decide(
    release: &Version,
    current: &Version,
    skipped: Option<&Version>,
    notified_this_run: Option<&Version>,
    is_manual: bool,
) -> Decision {
    if !is_newer(release, current) {
        return Decision::UpToDate;
    }
    // A manual check answers the user's question, so it shows the release even if it was
    // skipped or dismissed before.
    if is_manual {
        return Decision::Notify;
    }
    let covered_by = |handled: Option<&Version>| {
        handled.is_some_and(|handled_version| release <= handled_version)
    };
    if covered_by(skipped) || covered_by(notified_this_run) {
        Decision::AlreadyHandled
    } else {
        Decision::Notify
    }
}

fn is_newer(release: &Version, current: &Version) -> bool {
    release.pre.is_empty() && without_build_metadata(release) > without_build_metadata(current)
}

/// `semver` also orders by build metadata, where Zed stores the channel and commit SHA.
fn without_build_metadata(version: &Version) -> Version {
    Version {
        build: BuildMetadata::EMPTY,
        ..version.clone()
    }
}

/// Scheduled checks usually finish while the app is in the background, so this falls back
/// from the focused window to the frontmost one.
fn active_workspace(cx: &App) -> Option<Entity<Workspace>> {
    cx.active_window()
        .into_iter()
        .chain(cx.window_stack().unwrap_or_default())
        .chain(cx.windows())
        .filter_map(|window| window.downcast::<MultiWorkspace>())
        .find_map(|window| Some(window.read(cx).ok()?.workspace().clone()))
}

fn show_release_notification(
    workspace: &Entity<Workspace>,
    release: &UpstreamRelease,
    current_version: &Version,
    cx: &mut App,
) {
    let message = format!(
        "Zed v{} is available upstream (you're on v{current_version}). Rebuild vszed from source to update.",
        release.version
    );
    let url = release.url.clone();
    let version = release.version.clone();
    workspace.update(cx, |workspace, cx| {
        workspace.show_notification(
            NotificationId::unique::<UpstreamReleaseNotification>(),
            cx,
            |cx| {
                cx.new(|cx| {
                    MessageNotification::new(message, cx)
                        .primary_message("Release Notes")
                        .primary_end_icon(IconName::ArrowUpRight)
                        .primary_on_click(move |_window, cx| cx.open_url(&url))
                        .secondary_message("Skip This Version")
                        .secondary_on_click(move |_window, cx| skip_version(&version, cx))
                        .show_suppress_button(false)
                })
            },
        );
    });
}

fn show_manual_check_toast<T: 'static>(trigger: &CheckTrigger, message: String, cx: &mut App) {
    let CheckTrigger::Manual(workspace) = trigger else {
        return;
    };
    let Some(workspace) = workspace.upgrade() else {
        return;
    };
    workspace.update(cx, |workspace, cx| {
        workspace.show_toast(
            Toast::new(NotificationId::unique::<T>(), message).autohide(),
            cx,
        );
    });
}

fn read_skipped_version(cx: &App) -> Option<Version> {
    let stored = KeyValueStore::global(cx)
        .read_kvp(SKIPPED_VERSION_KEY)
        .log_err()??;
    Version::parse(&stored).log_err()
}

fn skip_version(version: &Version, cx: &mut App) {
    if read_skipped_version(cx).is_some_and(|skipped| skipped >= *version) {
        return;
    }
    let store = KeyValueStore::global(cx);
    let version_text = version.to_string();
    db::write_and_log(cx, move || async move {
        store
            .write_kvp(SKIPPED_VERSION_KEY.to_string(), version_text)
            .await
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use http_client::{FakeHttpClient, Response};
    use project::Project;
    use release_channel::ReleaseChannel;
    use std::sync::Mutex;
    use workspace::AppState;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn github_release(tag: &str, prerelease: bool) -> GithubRelease {
        GithubRelease {
            tag_name: tag.to_string(),
            html_url: format!("https://github.com/zed-industries/zed/releases/tag/{tag}"),
            prerelease,
            draft: false,
        }
    }

    fn parsed_version(tag: &str) -> Version {
        parse_release(github_release(tag, false))
            .unwrap()
            .unwrap()
            .version
    }

    #[test]
    fn test_version_comparison() {
        // Zed's app version carries the channel and commit SHA as build metadata.
        let current = version("1.28.1+stable.123.abcdef0");

        assert_eq!(parsed_version("v1.29.0"), version("1.29.0"));
        assert_eq!(parsed_version("1.29.0"), version("1.29.0"));

        assert!(is_newer(&parsed_version("v1.29.0"), &current));
        assert!(is_newer(&parsed_version("v1.28.2"), &current));
        assert!(!is_newer(&parsed_version("v1.28.1"), &current));
        assert!(!is_newer(&parsed_version("v1.27.9"), &current));
        assert!(!is_newer(
            &parsed_version("v1.28.1"),
            &version("1.30.0+dev")
        ));
        assert!(!is_newer(&version("1.28.1+other"), &version("1.28.1")));
    }

    #[test]
    fn test_prereleases_are_ignored() {
        assert_eq!(
            parse_release(github_release("v1.29.0-pre", false)).unwrap(),
            None
        );
        assert_eq!(
            parse_release(github_release("v1.29.0", true)).unwrap(),
            None
        );
        let draft = GithubRelease {
            draft: true,
            ..github_release("v1.29.0", false)
        };
        assert_eq!(parse_release(draft).unwrap(), None);
        assert!(!is_newer(&version("1.29.0-pre"), &version("1.28.1")));
    }

    #[test]
    fn test_malformed_release() {
        for tag in [
            "",
            "v",
            "nightly",
            "v1.29",
            "v1.29.x",
            "zed-v1.29.0",
            " v1.29.0",
        ] {
            assert!(
                parse_release(github_release(tag, false)).is_err(),
                "tag {tag:?} should be rejected"
            );
        }

        let unexpected_url = GithubRelease {
            html_url: "file:///Applications".to_string(),
            ..github_release("v1.29.0", false)
        };
        assert!(parse_release(unexpected_url).is_err());
    }

    #[test]
    fn test_skip_and_once_per_run_decisions() {
        let current = version("1.28.1");
        let release = version("1.29.0");
        let next_release = version("1.29.1");

        assert_eq!(
            decide(&release, &current, None, None, false),
            Decision::Notify
        );

        // Skipped versions and older ones are never shown again by automatic checks.
        assert_eq!(
            decide(&release, &current, Some(&release), None, false),
            Decision::AlreadyHandled
        );
        assert_eq!(
            decide(&release, &current, Some(&next_release), None, false),
            Decision::AlreadyHandled
        );
        assert_eq!(
            decide(&next_release, &current, Some(&release), None, false),
            Decision::Notify
        );

        // A release shown earlier in this run is not shown again, a newer one is.
        assert_eq!(
            decide(&release, &current, None, Some(&release), false),
            Decision::AlreadyHandled
        );
        assert_eq!(
            decide(&next_release, &current, None, Some(&release), false),
            Decision::Notify
        );

        // A manual check always reports a newer release.
        assert_eq!(
            decide(&release, &current, Some(&release), Some(&release), true),
            Decision::Notify
        );
        assert_eq!(
            decide(&current, &current, None, None, true),
            Decision::UpToDate
        );
        assert_eq!(
            decide(&release, &version("1.30.0"), None, None, false),
            Decision::UpToDate
        );
    }

    #[derive(Clone, Copy)]
    enum FakeResponse {
        Release(&'static str),
        Status(u16),
        Body(&'static str),
        NetworkError,
    }

    struct RecordedRequest {
        uri: String,
        user_agent: Option<String>,
        has_authorization: bool,
    }

    struct FakeGithub {
        response: Mutex<FakeResponse>,
        requests: Mutex<Vec<RecordedRequest>>,
    }

    impl FakeGithub {
        fn new(response: FakeResponse) -> Arc<Self> {
            Arc::new(Self {
                response: Mutex::new(response),
                requests: Mutex::new(Vec::new()),
            })
        }

        fn respond_with(&self, response: FakeResponse) {
            *self.response.lock().unwrap() = response;
        }

        fn request_count(&self) -> usize {
            self.requests.lock().unwrap().len()
        }

        fn http_client(self: &Arc<Self>) -> Arc<dyn HttpClient> {
            let github = self.clone();
            FakeHttpClient::create(move |request| {
                github.requests.lock().unwrap().push(RecordedRequest {
                    uri: request.uri().to_string(),
                    user_agent: request
                        .headers()
                        .get("User-Agent")
                        .map(|value| value.to_str().unwrap().to_string()),
                    has_authorization: request.headers().contains_key("Authorization"),
                });
                let response = *github.response.lock().unwrap();
                async move {
                    let (status, body) = match response {
                        FakeResponse::Release(tag) => (200, release_json(tag)),
                        FakeResponse::Status(status) => (
                            status,
                            r#"{"message":"API rate limit exceeded"}"#.to_string(),
                        ),
                        FakeResponse::Body(body) => (200, body.to_string()),
                        FakeResponse::NetworkError => return Err(anyhow!("connection refused")),
                    };
                    Ok(Response::builder()
                        .status(status)
                        .body(AsyncBody::from(body))
                        .unwrap())
                }
            })
        }
    }

    fn release_json(tag: &str) -> String {
        serde_json::json!({
            "tag_name": tag,
            "html_url": format!("https://github.com/zed-industries/zed/releases/tag/{tag}"),
            "published_at": "2026-11-11T19:08:27Z",
            "prerelease": false,
            "draft": false,
        })
        .to_string()
    }

    fn init_test(
        current_version: &str,
        github: &Arc<FakeGithub>,
        cx: &mut TestAppContext,
    ) -> Arc<AppState> {
        cx.update(|cx| {
            let app_state = AppState::test(cx);
            cx.set_global(db::AppDatabase::test_new());
            release_channel::init_test(version(current_version), ReleaseChannel::Stable, cx);
            cx.set_http_client(github.http_client());
            app_state
        })
    }

    async fn open_workspace<'a>(
        app_state: &Arc<AppState>,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Workspace>, &'a mut VisualTestContext) {
        let project = Project::test(app_state.fs.clone(), [], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
        cx.run_until_parked();
        (workspace, cx)
    }

    /// Runs pending work first so that timers created by it exist before the clock moves.
    fn advance(duration: Duration, cx: &mut TestAppContext) {
        cx.run_until_parked();
        cx.executor().advance_clock(duration);
        cx.run_until_parked();
    }

    fn set_auto_update(enabled: bool, cx: &mut TestAppContext) {
        cx.update_global::<SettingsStore, _>(|store, cx| {
            store.update_user_settings(cx, |content| content.auto_update = Some(enabled));
        });
    }

    fn restart_notifier(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let current_version = AppVersion::global(cx);
            let notifier = cx.new(|cx| UpstreamReleaseNotifier::new(current_version, cx));
            cx.set_global(GlobalUpstreamReleaseNotifier(notifier));
        });
    }

    fn has_notification<T: 'static>(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> bool {
        workspace.read_with(cx, |workspace, _| {
            workspace.has_notification(&NotificationId::unique::<T>())
        })
    }

    fn dismiss<T: 'static>(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        workspace.update(cx, |workspace, cx| {
            workspace.dismiss_notification(&NotificationId::unique::<T>(), cx);
        });
    }

    #[gpui::test]
    async fn test_automatic_check_notifies_once_per_run(cx: &mut TestAppContext) {
        let github = FakeGithub::new(FakeResponse::Release("v1.29.0"));
        let app_state = init_test("1.28.1", &github, cx);
        cx.update(init);
        let (workspace, cx) = open_workspace(&app_state, cx).await;

        assert_eq!(
            github.request_count(),
            0,
            "nothing is checked right at startup"
        );

        advance(FIRST_CHECK_DELAY, cx);
        assert_eq!(github.request_count(), 1);
        assert!(has_notification::<UpstreamReleaseNotification>(
            &workspace, cx
        ));
        {
            let requests = github.requests.lock().unwrap();
            assert_eq!(requests[0].uri, LATEST_RELEASE_URL);
            assert_eq!(requests[0].user_agent.as_deref(), Some(USER_AGENT));
            assert!(!requests[0].has_authorization);
        }

        dismiss::<UpstreamReleaseNotification>(&workspace, cx);
        advance(CHECK_INTERVAL, cx);
        assert_eq!(github.request_count(), 2);
        assert!(
            !has_notification::<UpstreamReleaseNotification>(&workspace, cx),
            "a dismissed release is not shown again in the same run"
        );

        github.respond_with(FakeResponse::Release("v1.29.1"));
        advance(CHECK_INTERVAL, cx);
        assert_eq!(github.request_count(), 3);
        assert!(
            has_notification::<UpstreamReleaseNotification>(&workspace, cx),
            "a newer release is shown"
        );

        dismiss::<UpstreamReleaseNotification>(&workspace, cx);
        restart_notifier(cx);
        advance(FIRST_CHECK_DELAY, cx);
        assert_eq!(github.request_count(), 4);
        assert!(
            has_notification::<UpstreamReleaseNotification>(&workspace, cx),
            "a dismissed release is shown again after a restart"
        );
    }

    #[gpui::test]
    async fn test_skipped_version_is_not_shown_again(cx: &mut TestAppContext) {
        let github = FakeGithub::new(FakeResponse::Release("v1.29.0"));
        let app_state = init_test("1.28.1", &github, cx);
        cx.update(init);
        let (workspace, cx) = open_workspace(&app_state, cx).await;

        advance(FIRST_CHECK_DELAY, cx);
        assert!(has_notification::<UpstreamReleaseNotification>(
            &workspace, cx
        ));

        // What the "Skip This Version" button does before the notification dismisses itself.
        cx.update(|_window, cx| skip_version(&version("1.29.0"), cx));
        dismiss::<UpstreamReleaseNotification>(&workspace, cx);
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_window, cx| read_skipped_version(cx)),
            Some(version("1.29.0"))
        );

        restart_notifier(cx);
        advance(FIRST_CHECK_DELAY, cx);
        assert_eq!(github.request_count(), 2);
        assert!(
            !has_notification::<UpstreamReleaseNotification>(&workspace, cx),
            "a skipped release is not shown after a restart"
        );

        cx.update(|_window, cx| skip_version(&version("1.28.5"), cx));
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_window, cx| read_skipped_version(cx)),
            Some(version("1.29.0")),
            "skipping an older version keeps the newer skip"
        );

        github.respond_with(FakeResponse::Release("v1.29.1"));
        advance(CHECK_INTERVAL, cx);
        assert!(
            has_notification::<UpstreamReleaseNotification>(&workspace, cx),
            "a release newer than the skipped one is shown"
        );
    }

    #[gpui::test]
    async fn test_auto_update_setting_switches_automatic_checks(cx: &mut TestAppContext) {
        let github = FakeGithub::new(FakeResponse::Release("v1.29.0"));
        let app_state = init_test("1.28.1", &github, cx);
        set_auto_update(false, cx);
        cx.update(init);
        let (workspace, cx) = open_workspace(&app_state, cx).await;

        advance(FIRST_CHECK_DELAY + CHECK_INTERVAL, cx);
        assert_eq!(github.request_count(), 0);

        set_auto_update(true, cx);
        advance(FIRST_CHECK_DELAY, cx);
        assert_eq!(github.request_count(), 1);
        assert!(has_notification::<UpstreamReleaseNotification>(
            &workspace, cx
        ));

        set_auto_update(false, cx);
        advance(CHECK_INTERVAL, cx);
        advance(CHECK_INTERVAL, cx);
        assert_eq!(github.request_count(), 1);

        // The manual action still checks while automatic checks are off.
        dismiss::<UpstreamReleaseNotification>(&workspace, cx);
        cx.dispatch_action(CheckForUpstreamRelease);
        cx.run_until_parked();
        assert_eq!(github.request_count(), 2);
        assert!(has_notification::<UpstreamReleaseNotification>(
            &workspace, cx
        ));
    }

    #[gpui::test]
    async fn test_manual_check(cx: &mut TestAppContext) {
        let github = FakeGithub::new(FakeResponse::Release("v1.28.1"));
        let app_state = init_test("1.28.1", &github, cx);
        cx.update(init);
        let (workspace, cx) = open_workspace(&app_state, cx).await;

        cx.dispatch_action(CheckForUpstreamRelease);
        cx.run_until_parked();
        assert_eq!(github.request_count(), 1);
        assert!(has_notification::<UpToDateToast>(&workspace, cx));
        assert!(!has_notification::<UpstreamReleaseNotification>(
            &workspace, cx
        ));

        github.respond_with(FakeResponse::Release("v1.29.0"));
        cx.dispatch_action(CheckForUpstreamRelease);
        cx.run_until_parked();
        assert_eq!(github.request_count(), 2);
        assert!(has_notification::<UpstreamReleaseNotification>(
            &workspace, cx
        ));

        dismiss::<UpstreamReleaseNotification>(&workspace, cx);
        advance(FIRST_CHECK_DELAY, cx);
        assert_eq!(github.request_count(), 3);
        assert!(
            !has_notification::<UpstreamReleaseNotification>(&workspace, cx),
            "a release shown by a manual check counts as shown for this run"
        );

        cx.dispatch_action(CheckForUpstreamRelease);
        cx.run_until_parked();
        assert!(
            has_notification::<UpstreamReleaseNotification>(&workspace, cx),
            "a manual check shows a newer release even after it was dismissed"
        );

        cx.update(|_window, cx| skip_version(&version("1.29.0"), cx));
        dismiss::<UpstreamReleaseNotification>(&workspace, cx);
        cx.dispatch_action(CheckForUpstreamRelease);
        cx.run_until_parked();
        assert!(
            has_notification::<UpstreamReleaseNotification>(&workspace, cx),
            "a manual check shows a newer release even after it was skipped"
        );
    }

    #[gpui::test]
    async fn test_errors_and_rate_limits(cx: &mut TestAppContext) {
        let github = FakeGithub::new(FakeResponse::Status(403));
        let app_state = init_test("1.28.1", &github, cx);
        cx.update(init);
        let (workspace, cx) = open_workspace(&app_state, cx).await;

        // Automatic checks fail quietly and retry at the next interval.
        let quiet_failures = [
            FakeResponse::Status(403),
            FakeResponse::Status(429),
            FakeResponse::Status(500),
            FakeResponse::NetworkError,
            FakeResponse::Body("not json"),
            FakeResponse::Release("nightly"),
        ];
        for (index, response) in quiet_failures.into_iter().enumerate() {
            github.respond_with(response);
            let interval = if index == 0 {
                FIRST_CHECK_DELAY
            } else {
                CHECK_INTERVAL
            };
            advance(interval, cx);
            assert_eq!(github.request_count(), index + 1);
            assert!(!has_notification::<UpstreamReleaseNotification>(
                &workspace, cx
            ));
            assert!(!has_notification::<CheckFailedToast>(&workspace, cx));
        }

        github.respond_with(FakeResponse::Release("v1.29.0"));
        advance(CHECK_INTERVAL, cx);
        assert!(
            has_notification::<UpstreamReleaseNotification>(&workspace, cx),
            "the check after a failure still notifies"
        );
        dismiss::<UpstreamReleaseNotification>(&workspace, cx);

        // Manual checks report failures with a short toast.
        for response in quiet_failures {
            github.respond_with(response);
            dismiss::<CheckFailedToast>(&workspace, cx);
            cx.dispatch_action(CheckForUpstreamRelease);
            cx.run_until_parked();
            assert!(has_notification::<CheckFailedToast>(&workspace, cx));
            assert!(!has_notification::<UpstreamReleaseNotification>(
                &workspace, cx
            ));
        }
    }

    #[gpui::test]
    async fn test_fetch_classifies_responses(_cx: &mut TestAppContext) {
        let github = FakeGithub::new(FakeResponse::Release("v1.29.0"));
        let http_client = github.http_client();

        let release = fetch_latest_release(http_client.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(release.version, version("1.29.0"));
        assert_eq!(
            release.url,
            "https://github.com/zed-industries/zed/releases/tag/v1.29.0"
        );

        github.respond_with(FakeResponse::Release("v1.30.0-pre"));
        assert!(
            fetch_latest_release(http_client.clone())
                .await
                .unwrap()
                .is_none()
        );

        for status in [403, 429] {
            github.respond_with(FakeResponse::Status(status));
            assert!(matches!(
                fetch_latest_release(http_client.clone()).await,
                Err(CheckError::RateLimited(_))
            ));
        }

        for response in [
            FakeResponse::Status(404),
            FakeResponse::Status(500),
            FakeResponse::NetworkError,
            FakeResponse::Body("not json"),
            FakeResponse::Body(r#"{"message":"Not Found"}"#),
            FakeResponse::Release("v1.29"),
        ] {
            github.respond_with(response);
            assert!(matches!(
                fetch_latest_release(http_client.clone()).await,
                Err(CheckError::Failed(_))
            ));
        }
    }
}
