# Upstream release notifier: upstream hooks

Branch `feat/upstream-release-notifier`. vszed builds turn off Zed's auto-updater at compile
time (`ZED_UPDATE_EXPLANATION`), because on the stable channel it would download official Zed
and replace vszed. This branch only *notifies* when upstream publishes a newer stable release,
so the fork can be rebuilt. It never downloads or installs anything.

All new code lives in `crates/upstream_release_notifier/` (fork-only crate) and
`script/vszed-bundle` (fork-only script). These are the only edits to upstream files. Use this
list when resolving rebase conflicts.

| File | Purpose | Lines |
|---|---|---|
| `README.md` (1-2) | `.rules` review banner. Removing it is a manual step for the human author. | +2 |
| `Cargo.toml` (members ~234, `[workspace.dependencies]` ~509) | `"crates/upstream_release_notifier"` member and `upstream_release_notifier = { path = ... }` | +2 |
| `crates/zed/Cargo.toml` (~222) | `upstream_release_notifier.workspace = true` | +1 |
| `crates/zed/src/main.rs` (~671) | `upstream_release_notifier::init(cx);` right after `auto_update_ui::init(cx);` | +1 |
| `Cargo.lock` (~19462, ~22676) | the crate's `[[package]]` entry and the `zed` dependency line. Regenerate with any cargo command on conflict. | +22 |

Commits: `docs: add review banner to README`, `feat(upstream_release_notifier): ...` (crate only),
`build: add upstream_release_notifier to the workspace` (root `Cargo.toml` + `Cargo.lock`),
`feat(zed): initialize upstream_release_notifier` (zed manifest, `main.rs`, `Cargo.lock`),
`feat(script): add vszed-bundle wrapper ...`, and this file.

## Behaviour

- Endpoint: `GET https://api.github.com/repos/zed-industries/zed/releases/latest` (latest
  non-draft, non-prerelease release; fields used: `tag_name`, `html_url`, `prerelease`,
  `draft`). Sent through `cx.http_client()`, so Zed's proxy settings apply.
  Headers: `Accept: application/vnd.github+json`, `X-GitHub-Api-Version: 2022-11-28`,
  `User-Agent: vszed-upstream-release-notifier`. No auth token, no query string, nothing
  about the user. 30 second request timeout.
- Schedule: first check 30 seconds after startup, then every 6 hours, on GPUI executor timers.
- Comparison: the tag minus its leading `v` is parsed with `semver` and compared with
  `AppVersion` (build metadata, where Zed keeps the channel and commit SHA, is stripped from
  both). Only a strictly newer stable version notifies. Prerelease tags (`v1.29.0-pre`),
  `prerelease: true` and drafts are ignored. A malformed tag or a non-`https://github.com/`
  `html_url` counts as a failed check. A dev build of upstream `main` (e.g. 1.30.0) is ahead of
  stable and therefore never notifies: no special case for the dev channel.
- Notification: shown in the active workspace (the focused window, else the frontmost
  workspace window). If no workspace window is open, nothing is recorded and the next check
  tries again. Text: "Zed v1.29.0 is available upstream (you're on v1.28.1). Rebuild vszed
  from source to update." Buttons:
  - "Release Notes" opens `html_url` in the browser.
  - "Skip This Version" stores the version in the KV store under
    `upstream_release_notifier_skipped_version`. Automatic checks never show it or anything
    older again. A later skip never lowers the stored version.
  - Dismissing (or either button, which also dismisses) keeps it hidden for the rest of the
    run. It comes back on the next start unless skipped. A newer release during the same run
    is shown.
- Switch: automatic checks follow the existing `auto_update` setting (default `true`).
  `"auto_update": false` stops them, and turning it back on restarts the schedule (first check
  30 seconds later). No new setting: with `ZED_UPDATE_EXPLANATION` set, `auto_update` has
  nothing else to switch. Read through a private `UpstreamReleaseCheckSetting`
  (`RegisterSetting`) because `auto_update`'s own setting type is private.
- Manual check: `zed: check for upstream release` (`zed::CheckForUpstreamRelease`), registered
  on every workspace. Works even when `auto_update` is false. Shows the notification for any
  newer release (ignoring skips and earlier dismissals, since the user asked), otherwise a
  toast "Zed v1.28.1 is the latest stable release.". Failures give a short toast and are
  logged. The action lives in the existing `zed` namespace because `crates/zed/src/zed.rs`
  `test_action_namespaces` lists every namespace, and a new one would need another upstream
  edit.
- Errors: 403 and 429 are treated as GitHub rate limiting (60 unauthenticated requests an
  hour); automatic checks log at info level and retry at the next interval. Other failures
  (network, non-2xx status, bad JSON, malformed tag) log a warning and also retry at the next
  interval. Nothing panics.
- Tests: in-crate tests use Zed's `FakeHttpClient` and drive the schedule with
  `advance_clock`. The real GitHub API is never called.

## Building a release

```sh
# From the repository root. Arguments go to script/bundle-mac (for example -i to install).
script/vszed-bundle
MACOS_SIGNING_KEY="vszed self-signed" script/vszed-bundle -i
```

`script/vszed-bundle` exports `ZED_UPDATE_EXPLANATION` ("vszed is updated by rebuilding from
source. Run 'zed: check for upstream release' to see the latest Zed release.", overridable)
and an optional `MACOS_SIGNING_KEY` (ad hoc signing when unset), then `exec`s
`script/bundle-mac "$@"`. It exits with an error when not run from the repository root.
With the explanation compiled in, `auto update: check` shows that text instead of checking.
