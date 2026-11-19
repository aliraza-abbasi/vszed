use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use acp_thread::AcpThreadEvent;
use agent_client_protocol::v2::schema as acp_v2;
use chrono::{DateTime, TimeZone, Utc};
use collections::HashMap;
use fs::Fs;
use futures::{FutureExt as _, StreamExt as _, channel::mpsc};
use gpui::{App, Context, EntityId, FontWeight, Global, Hsla, Task, px};
use serde::{Deserialize, Deserializer, de::IgnoredAny};
use serde_json::Value;
use ui::prelude::*;

use crate::conversation_view::ThreadView;
use crate::ui_styles::claude_code::side_bar_label_size;

/// The usage update `_meta` key claude-agent-acp puts the Claude CLI's
/// `rate_limit_info` under.
pub(crate) const RATE_LIMIT_META_KEY: &str = "_claude/rateLimit";

/// Claude Code rewrites its account file in bursts (every session start and
/// usage refresh touches it).
const ACCOUNT_FILE_LATENCY: Duration = Duration::from_millis(500);
pub(crate) const ACCOUNT_FILE_DEBOUNCE: Duration = Duration::from_millis(500);

/// A usage limit window, as the Claude CLI names them in `rateLimitType` and
/// `unifiedWindows`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RateLimitType {
    FiveHour,
    SevenDay,
    SevenDayOpus,
    SevenDaySonnet,
    SevenDayOverageIncluded,
    Overage,
    /// Requests are refused or close to it, but the CLI didn't say which limit.
    Unspecified,
    Other(SharedString),
}

impl RateLimitType {
    fn parse(name: &str) -> Self {
        match name {
            "five_hour" => Self::FiveHour,
            "seven_day" => Self::SevenDay,
            "seven_day_opus" => Self::SevenDayOpus,
            "seven_day_sonnet" => Self::SevenDaySonnet,
            "seven_day_overage_included" => Self::SevenDayOverageIncluded,
            "overage" => Self::Overage,
            other => Self::Other(SharedString::from(other.to_string())),
        }
    }

    pub(crate) fn id(&self) -> SharedString {
        match self {
            Self::FiveHour => "five_hour".into(),
            Self::SevenDay => "seven_day".into(),
            Self::SevenDayOpus => "seven_day_opus".into(),
            Self::SevenDaySonnet => "seven_day_sonnet".into(),
            Self::SevenDayOverageIncluded => "seven_day_overage_included".into(),
            Self::Overage => "overage".into(),
            Self::Unspecified => "unspecified".into(),
            Self::Other(name) => name.clone(),
        }
    }

    /// "Weekly (7 day)" and "Fable limit" are the extension's labels. Its
    /// 5-hour label isn't known, so that one follows the weekly pattern, and
    /// the other windows use the bundled CLI's names for them (the CLI calls
    /// `seven_day_overage_included` "Fable limit").
    pub(crate) fn label(&self) -> SharedString {
        match self {
            Self::FiveHour => "Session (5 hour)".into(),
            Self::SevenDay => "Weekly (7 day)".into(),
            Self::SevenDayOpus => "Opus limit".into(),
            Self::SevenDaySonnet => "Sonnet limit".into(),
            Self::SevenDayOverageIncluded => "Fable limit".into(),
            Self::Overage => "Usage credit limit".into(),
            Self::Unspecified => "Usage limit".into(),
            Self::Other(name) => {
                let words = name.replace('_', " ");
                let mut characters = words.chars();
                match characters.next() {
                    Some(first) => {
                        format!("{}{}", first.to_uppercase(), characters.as_str()).into()
                    }
                    None => "Usage limit".into(),
                }
            }
        }
    }

    fn sort_order(&self) -> usize {
        match self {
            Self::Unspecified => 0,
            Self::FiveHour => 1,
            Self::SevenDay => 2,
            Self::SevenDayOpus => 3,
            Self::SevenDaySonnet => 4,
            Self::SevenDayOverageIncluded => 5,
            Self::Overage => 6,
            Self::Other(_) => 7,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RateLimitStatus {
    Allowed,
    /// Past a warning threshold (`allowed_warning`, or the usage endpoint's
    /// `warning` severity).
    Warning,
    /// The usage endpoint's `critical` severity: shown in the error color, but
    /// requests aren't necessarily refused yet.
    Critical,
    Rejected,
}

impl RateLimitStatus {
    fn parse(value: &Value) -> Option<Self> {
        match value.as_str()? {
            "allowed" => Some(Self::Allowed),
            "allowed_warning" => Some(Self::Warning),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }

    fn from_severity(severity: &str) -> Option<Self> {
        match severity {
            "normal" => Some(Self::Allowed),
            "warning" => Some(Self::Warning),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UsageWindow {
    pub limit_type: RateLimitType,
    /// The server's name for the window, when the account cache gave one.
    pub label: Option<SharedString>,
    /// The fraction of the window used. Above 1 when usage legitimately ran
    /// past the window's cap.
    pub utilization: Option<f64>,
    /// Unix time in seconds.
    pub resets_at: Option<i64>,
    pub status: Option<RateLimitStatus>,
}

impl UsageWindow {
    pub(crate) fn label(&self) -> SharedString {
        self.label
            .clone()
            .unwrap_or_else(|| self.limit_type.label())
    }

    fn has_reset(&self, now: i64) -> bool {
        self.resets_at.is_some_and(|resets_at| resets_at <= now)
    }

    fn has_anything_to_show(&self) -> bool {
        self.utilization.is_some()
            || matches!(
                self.status,
                Some(
                    RateLimitStatus::Warning
                        | RateLimitStatus::Critical
                        | RateLimitStatus::Rejected
                )
            )
    }
}

/// The Claude CLI's `rate_limit_info`, parsed leniently: unknown fields are
/// ignored and a missing or malformed field only drops that field.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ClaudeRateLimit {
    /// Whether requests are allowed, describing the currently limiting window.
    pub status: Option<RateLimitStatus>,
    pub limit_type: Option<RateLimitType>,
    pub utilization: Option<f64>,
    pub resets_at: Option<i64>,
    /// Per-window readings (`unifiedWindows`), which the CLI includes for
    /// claude.ai sign-ins whatever the status is.
    pub windows: Vec<UsageWindow>,
}

impl ClaudeRateLimit {
    pub(crate) fn parse(value: &Value) -> Option<Self> {
        let info = value.as_object()?;
        let windows = info
            .get("unifiedWindows")
            .and_then(Value::as_object)
            .map(|windows| {
                windows
                    .iter()
                    .filter_map(|(name, window)| {
                        let window = window.as_object()?;
                        Some(UsageWindow {
                            limit_type: RateLimitType::parse(name),
                            label: None,
                            utilization: parse_utilization(window.get("utilization")),
                            resets_at: parse_timestamp(window.get("resetsAt")),
                            status: window.get("status").and_then(RateLimitStatus::parse),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            status: info.get("status").and_then(RateLimitStatus::parse),
            limit_type: info
                .get("rateLimitType")
                .and_then(Value::as_str)
                .map(RateLimitType::parse),
            utilization: parse_utilization(info.get("utilization")),
            resets_at: parse_timestamp(info.get("resetsAt")),
            windows,
        })
    }

    /// One window per limit type: the per-window readings, with the top-level
    /// fields filling in (or adding) the window they describe.
    pub(crate) fn usage_windows(&self) -> Vec<UsageWindow> {
        let mut windows = self.windows.clone();
        let top_level_type = match (&self.limit_type, self.status) {
            (Some(limit_type), _) => limit_type.clone(),
            (None, Some(RateLimitStatus::Warning | RateLimitStatus::Rejected)) => {
                RateLimitType::Unspecified
            }
            (None, _) => return windows,
        };
        match windows
            .iter_mut()
            .find(|window| window.limit_type == top_level_type)
        {
            Some(window) => {
                window.utilization = window.utilization.or(self.utilization);
                window.resets_at = window.resets_at.or(self.resets_at);
                // A window's own status can be "rejected" while overage keeps
                // requests going; the top-level status says whether they are refused.
                window.status = self.status.or(window.status);
            }
            None => windows.push(UsageWindow {
                limit_type: top_level_type,
                label: None,
                utilization: self.utilization,
                resets_at: self.resets_at,
                status: self.status,
            }),
        }
        windows
    }
}

fn parse_utilization(value: Option<&Value>) -> Option<f64> {
    value?
        .as_f64()
        .filter(|utilization| utilization.is_finite() && *utilization >= 0.)
}

fn parse_timestamp(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|seconds| seconds.is_finite())
            .map(|seconds| seconds as i64)
    })
}

/// What Claude Code's own account file (`~/.claude.json`) says about the
/// signed-in claude.ai account. Only these values are ever read from it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ClaudeAccountCache {
    pub email: Option<SharedString>,
    pub usage: Option<CachedUsage>,
}

/// Claude Code's last fetch of the claude.ai usage endpoint.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CachedUsage {
    /// Unix time in milliseconds; 0 when the file didn't say.
    pub fetched_at_ms: i64,
    /// In the server's order.
    pub windows: Vec<UsageWindow>,
}

/// A value that reads as `None` when the file gives it a shape this code
/// doesn't expect, so a format change drops that value instead of the file.
#[derive(Debug)]
struct Lenient<T>(Option<T>);

impl<T> Default for Lenient<T> {
    fn default() -> Self {
        Self(None)
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LenientShape<T> {
    Expected(T),
    Unexpected(IgnoredAny),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Lenient<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match LenientShape::<T>::deserialize(deserializer)? {
            LenientShape::Expected(value) => Self(Some(value)),
            LenientShape::Unexpected(_) => Self(None),
        })
    }
}

// Every other key of the file (credentials among them) is skipped unread.
#[derive(Default, Deserialize)]
#[serde(default)]
struct AccountFile {
    #[serde(rename = "oauthAccount")]
    oauth_account: Option<OauthAccountFields>,
    #[serde(rename = "cachedUsageUtilization")]
    cached_usage: Option<CachedUsageFields>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct OauthAccountFields {
    #[serde(rename = "emailAddress")]
    email_address: Lenient<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct CachedUsageFields {
    #[serde(rename = "fetchedAtMs")]
    fetched_at_ms: Lenient<f64>,
    utilization: Option<UtilizationFields>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct UtilizationFields {
    limits: Lenient<Vec<Lenient<LimitFields>>>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct LimitFields {
    kind: Lenient<String>,
    percent: Lenient<f64>,
    severity: Lenient<String>,
    resets_at: Lenient<String>,
    scope: Lenient<ScopeFields>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ScopeFields {
    model: Lenient<DisplayNameFields>,
    surface: Lenient<DisplayNameFields>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct DisplayNameFields {
    display_name: Lenient<String>,
}

impl DisplayNameFields {
    fn name(self) -> Option<String> {
        self.display_name
            .0
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
    }
}

/// Reads the account values out of the file's text. Anything unexpected
/// yields the empty cache.
pub(crate) fn parse_account_file(contents: &str) -> ClaudeAccountCache {
    let Ok(file) = serde_json::from_str::<AccountFile>(contents) else {
        // The parse error can quote the file, which holds credentials.
        log::debug!("failed to parse Claude account cache");
        return ClaudeAccountCache::default();
    };
    let email = file
        .oauth_account
        .and_then(|account| account.email_address.0)
        .map(|email| email.trim().to_string())
        .filter(|email| !email.is_empty())
        .map(SharedString::from);
    let usage = file.cached_usage.and_then(|cached| {
        let limits = cached.utilization?.limits.0?;
        Some(CachedUsage {
            fetched_at_ms: cached
                .fetched_at_ms
                .0
                .filter(|fetched_at| fetched_at.is_finite() && *fetched_at > 0.)
                .map(|fetched_at| fetched_at as i64)
                .unwrap_or(0),
            windows: cached_windows(limits.into_iter().filter_map(|limit| limit.0).collect()),
        })
    });
    ClaudeAccountCache { email, usage }
}

fn is_named(name: &str, model: &str) -> bool {
    name.eq_ignore_ascii_case(model)
}

fn window_id_part(name: &str) -> String {
    name.to_lowercase().replace(char::is_whitespace, "_")
}

/// Maps the usage endpoint's rows (`kind` `session`, `weekly_all` and
/// `weekly_scoped`, per the bundled CLI's schema) onto the windows that live
/// rate limit events name, so the two sources describe the same rows.
fn cached_windows(limits: Vec<LimitFields>) -> Vec<UsageWindow> {
    struct Row {
        kind: String,
        model: Option<String>,
        surface: Option<String>,
        limit: LimitFields,
    }
    let rows = limits
        .into_iter()
        .filter_map(|mut limit| {
            let kind = limit.kind.0.take()?;
            let scope = limit.scope.0.take().unwrap_or_default();
            Some(Row {
                kind,
                model: scope.model.0.and_then(DisplayNameFields::name),
                surface: scope.surface.0.and_then(DisplayNameFields::name),
                limit,
            })
        })
        .collect::<Vec<_>>();
    // The CLI builds its live "weekly_scoped" row from
    // `seven_day_overage_included`, so a lone scoped model that isn't Opus or
    // Sonnet is that window.
    let other_model_rows = rows
        .iter()
        .filter(|row| {
            row.kind == "weekly_scoped"
                && row
                    .model
                    .as_deref()
                    .is_some_and(|model| !is_named(model, "opus") && !is_named(model, "sonnet"))
        })
        .count();

    let mut windows: Vec<UsageWindow> = Vec::new();
    for row in rows {
        let scope_name = row.model.clone().or_else(|| row.surface.clone());
        let limit_type = match (row.kind.as_str(), row.model.as_deref()) {
            ("session", _) => RateLimitType::FiveHour,
            ("weekly_all", _) => RateLimitType::SevenDay,
            ("weekly_scoped", Some(model)) if is_named(model, "opus") => {
                RateLimitType::SevenDayOpus
            }
            ("weekly_scoped", Some(model)) if is_named(model, "sonnet") => {
                RateLimitType::SevenDaySonnet
            }
            ("weekly_scoped", Some(_)) if other_model_rows == 1 => {
                RateLimitType::SevenDayOverageIncluded
            }
            (kind, _) => RateLimitType::Other(
                match &scope_name {
                    Some(name) => format!("{kind}_{}", window_id_part(name)),
                    None => kind.to_string(),
                }
                .into(),
            ),
        };
        if windows.iter().any(|window| window.limit_type == limit_type) {
            continue;
        }
        let label = scope_name.map(|name| SharedString::from(format!("{name} limit")));
        let window = UsageWindow {
            limit_type,
            label,
            utilization: row
                .limit
                .percent
                .0
                .filter(|percent| percent.is_finite() && *percent >= 0.)
                .map(|percent| percent / 100.),
            resets_at: row
                .limit
                .resets_at
                .0
                .and_then(|resets_at| DateTime::parse_from_rfc3339(resets_at.trim()).ok())
                .map(|resets_at| resets_at.timestamp()),
            status: row
                .limit
                .severity
                .0
                .and_then(|severity| RateLimitStatus::from_severity(&severity)),
        };
        windows.push(window);
    }
    windows
}

pub(crate) fn format_percent(utilization: f64) -> String {
    format!("{}%", (utilization * 100.).round().max(0.) as u64)
}

/// "Resets Sat, Nov 14 at 1:00 PM", in `timezone`.
pub(crate) fn format_reset_time<Tz: TimeZone>(resets_at: i64, timezone: &Tz) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    let reset = DateTime::from_timestamp(resets_at, 0)?.with_timezone(timezone);
    Some(format!(
        "Resets {}",
        reset.format("%a, %b %-d at %-I:%M %p")
    ))
}

/// A live reading with the time it arrived, in Unix milliseconds. 0 marks a
/// reading of unknown age (one a thread already had when its view opened),
/// which every other reading and the account cache win over.
type Reading = (UsageWindow, i64);

/// The most recently received reading of each limit type.
fn latest_readings<'a>(readings: impl IntoIterator<Item = &'a Reading>) -> Vec<&'a Reading> {
    let mut latest: HashMap<&RateLimitType, &Reading> = HashMap::default();
    for reading in readings {
        let (window, received_at) = reading;
        match latest.get(&window.limit_type) {
            Some((_, latest_received_at)) if latest_received_at > received_at => {}
            _ => {
                latest.insert(&window.limit_type, reading);
            }
        }
    }
    latest.into_values().collect()
}

/// The usage rows to show. The account cache's rows come first, in the
/// server's order; each takes a live reading of the same window instead when
/// that reading arrived after the cache was fetched. Live windows the cache
/// doesn't have follow. Windows that have reset since are left out.
pub(crate) fn merge_usage<'a>(
    readings: impl IntoIterator<Item = &'a Reading>,
    cache: Option<&CachedUsage>,
    now: i64,
) -> Vec<UsageWindow> {
    let mut live = latest_readings(readings);
    let mut rows = Vec::new();
    if let Some(cache) = cache {
        for cached in &cache.windows {
            let live_index = live
                .iter()
                .position(|(window, _)| window.limit_type == cached.limit_type);
            let newer_live = live_index
                .map(|index| live.swap_remove(index))
                .filter(|(_, received_at)| *received_at > cache.fetched_at_ms);
            rows.push(match newer_live {
                Some((window, _)) => UsageWindow {
                    label: cached.label.clone().or_else(|| window.label.clone()),
                    utilization: window.utilization.or(cached.utilization),
                    resets_at: window.resets_at.or(cached.resets_at),
                    ..window.clone()
                },
                None => cached.clone(),
            });
        }
    }
    let mut live_only = live
        .into_iter()
        .map(|(window, _)| window.clone())
        .collect::<Vec<_>>();
    live_only.sort_by_key(|window| (window.limit_type.sort_order(), window.limit_type.id()));
    rows.extend(live_only);
    rows.retain(|window| !window.has_reset(now) && window.has_anything_to_show());
    rows
}

struct ThreadUsage {
    project_id: EntityId,
    last_info: Value,
    /// The latest reading of each limit type.
    windows: Vec<Reading>,
}

/// The rate limit readings of every thread view, so a panel can show the
/// most recent reading across a project's threads. Each Claude session runs
/// its own CLI process, which reports only what its own requests observed.
#[derive(Default)]
struct ClaudeUsageStore {
    threads: HashMap<EntityId, ThreadUsage>,
    /// The last readings of each project's closed thread views, in memory only.
    retained: HashMap<EntityId, Vec<Reading>>,
    account: ClaudeAccountCache,
    /// Shared by every open sessions panel; the file stops being watched when
    /// the last one closes.
    account_watch: Weak<Task<()>>,
}

impl Global for ClaudeUsageStore {}

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|thread_view: &mut ThreadView, _window, cx| {
        track_thread_view(thread_view, cx);
    })
    .detach();
}

fn track_thread_view(thread_view: &mut ThreadView, cx: &mut Context<ThreadView>) {
    let thread_view_id = cx.entity_id();
    let project_id = thread_view.project.entity_id();

    // A view opened on a thread that already received usage starts from it,
    // ordered before every reading that arrives live.
    let thread = thread_view.thread.downgrade();
    cx.defer(move |cx| {
        if let Ok(meta) = thread.read_with(cx, |thread, _| thread.usage_meta().cloned()) {
            record_usage_meta(thread_view_id, project_id, meta.as_ref(), false, cx);
        }
    });

    let thread_subscription = cx.subscribe(&thread_view.thread, move |_, thread, event, cx| {
        if matches!(event, AcpThreadEvent::TokenUsageUpdated(_)) {
            let meta = thread.read(cx).usage_meta().cloned();
            record_usage_meta(thread_view_id, project_id, meta.as_ref(), true, cx);
        }
    });
    let release_subscription =
        cx.on_release(move |_, cx| retain_thread_readings(thread_view_id, cx));
    thread_view
        ._subscriptions
        .extend([thread_subscription, release_subscription]);
}

fn record_usage_meta(
    thread_view_id: EntityId,
    project_id: EntityId,
    meta: Option<&acp_v2::Meta>,
    live: bool,
    cx: &mut App,
) {
    let Some(info) = meta.and_then(|meta| meta.get(RATE_LIMIT_META_KEY)) else {
        return;
    };
    // Every usage update reaches here, but the rate limit info only changes
    // on the CLI's rate limit events.
    let unchanged = cx
        .try_global::<ClaudeUsageStore>()
        .and_then(|store| store.threads.get(&thread_view_id))
        .is_some_and(|thread| &thread.last_info == info);
    if unchanged {
        return;
    }
    let Some(rate_limit) = ClaudeRateLimit::parse(info) else {
        return;
    };
    let received_at = if live {
        Utc::now().timestamp_millis()
    } else {
        0
    };
    let store = cx.default_global::<ClaudeUsageStore>();
    let thread = store
        .threads
        .entry(thread_view_id)
        .or_insert_with(|| ThreadUsage {
            project_id,
            last_info: Value::Null,
            windows: Vec::new(),
        });
    thread.last_info = info.clone();
    merge_reading(&mut thread.windows, &rate_limit, received_at);
}

/// Keeps a closed thread view's readings for its project, so the panel still
/// shows the last known limits after every Claude tab is closed.
fn retain_thread_readings(thread_view_id: EntityId, cx: &mut App) {
    let has_readings = cx
        .try_global::<ClaudeUsageStore>()
        .is_some_and(|store| store.threads.contains_key(&thread_view_id));
    if !has_readings {
        return;
    }
    let store = cx.default_global::<ClaudeUsageStore>();
    let Some(thread) = store.threads.remove(&thread_view_id) else {
        return;
    };
    let retained = store.retained.entry(thread.project_id).or_default();
    for (window, received_at) in thread.windows {
        match retained
            .iter_mut()
            .find(|(existing, _)| existing.limit_type == window.limit_type)
        {
            Some((_, existing_received_at)) if *existing_received_at > received_at => {}
            Some(existing) => *existing = (window, received_at),
            None => retained.push((window, received_at)),
        }
    }
}

/// Updates a thread's stored windows with a new rate limit event. Windows the
/// event doesn't mention keep their last reading, because the CLI reports the
/// model and overage windows only while they limit requests.
fn merge_reading(stored: &mut Vec<Reading>, rate_limit: &ClaudeRateLimit, received_at: i64) {
    let windows = rate_limit.usage_windows();
    if rate_limit.status == Some(RateLimitStatus::Allowed) {
        // The CLI reports a plain "allowed" only when no window is near its
        // limit, so earlier warnings this event doesn't repeat are over.
        stored.retain_mut(|(stored_window, _)| {
            if windows
                .iter()
                .any(|window| window.limit_type == stored_window.limit_type)
            {
                return true;
            }
            if matches!(
                stored_window.status,
                Some(RateLimitStatus::Warning | RateLimitStatus::Rejected)
            ) {
                stored_window.status = None;
            }
            stored_window.limit_type != RateLimitType::Unspecified
        });
    }
    for window in windows {
        match stored
            .iter_mut()
            .find(|(existing, _)| existing.limit_type == window.limit_type)
        {
            Some(reading) => *reading = (window, received_at),
            None => stored.push((window, received_at)),
        }
    }
}

/// The usage windows to show for a project: its open and closed threads'
/// live readings merged with Claude Code's account cache.
pub(crate) fn project_usage_windows(project_id: EntityId, now: i64, cx: &App) -> Vec<UsageWindow> {
    let Some(store) = cx.try_global::<ClaudeUsageStore>() else {
        return Vec::new();
    };
    merge_usage(
        store
            .threads
            .values()
            .filter(|thread| thread.project_id == project_id)
            .flat_map(|thread| thread.windows.iter())
            .chain(store.retained.get(&project_id).into_iter().flatten()),
        store.account.usage.as_ref(),
        now,
    )
}

/// The signed-in account's email, from Claude Code's account file. The
/// adapter doesn't report it.
pub(crate) fn account_email(cx: &App) -> Option<SharedString> {
    cx.try_global::<ClaudeUsageStore>()?.account.email.clone()
}

/// Observes usage changes, for views that show them.
pub(crate) fn observe_usage<V: 'static>(cx: &mut Context<V>) -> gpui::Subscription {
    cx.observe_global::<ClaudeUsageStore>(|_, cx| cx.notify())
}

/// Keeps Claude Code's account file watched while held.
pub(crate) struct AccountFileWatch {
    _task: Rc<Task<()>>,
}

/// `$CLAUDE_CONFIG_DIR/.claude.json` when that is set, as Claude Code itself
/// resolves it, else `~/.claude.json`.
fn claude_account_file_path() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|directory| !directory.is_empty()) {
        Some(directory) => PathBuf::from(directory).join(".claude.json"),
        None => paths::home_dir().join(".claude.json"),
    }
}

pub(crate) fn watch_account_file(fs: Arc<dyn Fs>, cx: &mut App) -> AccountFileWatch {
    watch_account_file_at(fs, claude_account_file_path(), cx)
}

pub(crate) fn watch_account_file_at(
    fs: Arc<dyn Fs>,
    path: PathBuf,
    cx: &mut App,
) -> AccountFileWatch {
    if let Some(task) = cx
        .try_global::<ClaudeUsageStore>()
        .and_then(|store| store.account_watch.upgrade())
    {
        return AccountFileWatch { _task: task };
    }
    let task = Rc::new(spawn_account_file_reader(fs, path, cx));
    cx.default_global::<ClaudeUsageStore>().account_watch = Rc::downgrade(&task);
    AccountFileWatch { _task: task }
}

/// Reads and parses the file on the background executor, so only the parsed
/// values ever reach the main thread.
fn spawn_account_file_reader(fs: Arc<dyn Fs>, path: PathBuf, cx: &mut App) -> Task<()> {
    let (sender, mut receiver) = mpsc::unbounded::<ClaudeAccountCache>();
    let executor = cx.background_executor().clone();
    let reader = cx.background_spawn(async move {
        let path = match fs.canonicalize(&path).await {
            Ok(canonical) => canonical,
            // A missing file is the signed-out state; keep watching for it.
            Err(_) => path,
        };
        let (mut events, _watcher) = fs.watch(&path, ACCOUNT_FILE_LATENCY).await;
        loop {
            let account = match fs.load(&path).await {
                Ok(contents) => parse_account_file(&contents),
                Err(_) => ClaudeAccountCache::default(),
            };
            if sender.unbounded_send(account).is_err() {
                break;
            }
            if events.next().await.is_none() {
                break;
            }
            executor.timer(ACCOUNT_FILE_DEBOUNCE).await;
            while let Some(Some(_)) = events.next().now_or_never() {}
        }
    });
    cx.spawn(async move |cx| {
        let _reader = reader;
        while let Some(account) = receiver.next().await {
            cx.update(|cx| set_account(account, cx));
        }
    })
}

fn set_account(account: ClaudeAccountCache, cx: &mut App) {
    let unchanged = cx
        .try_global::<ClaudeUsageStore>()
        .is_some_and(|store| store.account == account);
    if !unchanged {
        cx.default_global::<ClaudeUsageStore>().account = account;
    }
}

pub(crate) fn render_account_and_usage(
    email: Option<SharedString>,
    windows: &[UsageWindow],
    cx: &App,
) -> impl IntoElement {
    v_flex()
        .px_3()
        .pb_2()
        .gap_1()
        .child(subheading("Account", cx))
        .child(
            h_flex()
                .w_full()
                .items_start()
                .justify_between()
                .gap_3()
                .child(
                    Label::new("Email")
                        .size(side_bar_label_size(1., cx))
                        .flex_none(),
                )
                .child(
                    div().flex_1().min_w_0().text_right().child(match email {
                        Some(email) => Label::new(email)
                            .size(side_bar_label_size(1., cx))
                            .single_line()
                            .truncate()
                            .into_any_element(),
                        None => Label::new("Not available")
                            .size(side_bar_label_size(0.9, cx))
                            .color(Color::Muted)
                            .into_any_element(),
                    }),
                ),
        )
        .child(div().pt_2().child(subheading("Usage", cx)))
        .map(|this| {
            if windows.is_empty() {
                this.child(
                    v_flex()
                        .debug_selector(|| "claude-code-usage-empty".to_string())
                        .child(
                            Label::new("Usage appears after Claude's next reply")
                                .size(side_bar_label_size(0.9, cx))
                                .color(Color::Muted),
                        )
                        .child(
                            Label::new("Claude reports plan limits for claude.ai sign-ins only")
                                .size(side_bar_label_size(0.8, cx))
                                .color(Color::Muted),
                        ),
                )
            } else {
                this.children(
                    windows
                        .iter()
                        .map(|window| render_usage_window(window, cx).into_any_element()),
                )
            }
        })
}

fn subheading(text: &'static str, cx: &App) -> impl IntoElement {
    Label::new(text.to_uppercase())
        .size(side_bar_label_size(0.9, cx))
        .weight(FontWeight::SEMIBOLD)
        .color(Color::Muted)
}

fn render_usage_window(window: &UsageWindow, cx: &App) -> impl IntoElement {
    let colors = cx.theme().colors();
    let status_colors = cx.theme().status();
    let limit_reached = window
        .utilization
        .is_some_and(|utilization| utilization >= 1.);
    let (fill, percent_color, state) = match window.status {
        Some(RateLimitStatus::Rejected) => {
            (status_colors.error, Color::Error, Some("Limit reached"))
        }
        Some(RateLimitStatus::Critical) => (
            status_colors.error,
            Color::Error,
            Some(if limit_reached {
                "Limit reached"
            } else {
                "Approaching limit"
            }),
        ),
        Some(RateLimitStatus::Warning) => (
            status_colors.warning,
            Color::Warning,
            Some("Approaching limit"),
        ),
        Some(RateLimitStatus::Allowed) | None => (colors.text_muted, Color::Default, None),
    };
    let details = state
        .map(str::to_string)
        .into_iter()
        .chain(
            window
                .resets_at
                .and_then(|resets_at| format_reset_time(resets_at, &chrono::Local)),
        )
        .collect::<Vec<_>>()
        .join(" · ");
    let limit_id = window.limit_type.id();

    v_flex()
        .debug_selector(move || format!("claude-code-usage-{limit_id}"))
        .w_full()
        .pt_1()
        .gap_1()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .gap_2()
                .child(
                    Label::new(window.label())
                        .size(side_bar_label_size(1., cx))
                        .truncate(),
                )
                .when_some(window.utilization, |this, utilization| {
                    this.child(
                        Label::new(format_percent(utilization))
                            .size(side_bar_label_size(1., cx))
                            .color(percent_color)
                            .flex_none(),
                    )
                }),
        )
        .when_some(window.utilization, |this, utilization| {
            this.child(usage_bar(utilization, fill, colors.border_variant))
        })
        .when(!details.is_empty(), |this| {
            this.child(
                Label::new(details)
                    .size(side_bar_label_size(0.8, cx))
                    .color(Color::Muted),
            )
        })
}

fn usage_bar(utilization: f64, fill: Hsla, track: Hsla) -> impl IntoElement {
    let fraction = utilization.clamp(0., 1.) as f32;
    div()
        .w_full()
        .h(px(4.))
        .rounded_full()
        .bg(track)
        .child(div().h_full().rounded_full().bg(fill).w(relative(fraction)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use serde_json::json;
    use std::path::Path;

    fn window(
        limit_type: RateLimitType,
        utilization: Option<f64>,
        resets_at: Option<i64>,
        status: Option<RateLimitStatus>,
    ) -> UsageWindow {
        UsageWindow {
            limit_type,
            label: None,
            utilization,
            resets_at,
            status,
        }
    }

    fn labeled(window: UsageWindow, label: &str) -> UsageWindow {
        UsageWindow {
            label: Some(label.to_string().into()),
            ..window
        }
    }

    #[test]
    fn test_parses_rate_limit_info_with_unified_windows() {
        let rate_limit = ClaudeRateLimit::parse(&json!({
            "status": "allowed_warning",
            "resetsAt": 1_763_143_200,
            "rateLimitType": "seven_day",
            "utilization": 0.82,
            "overageStatus": "rejected",
            "isUsingOverage": false,
            "someFutureField": { "nested": true },
            "unifiedWindows": {
                "five_hour": { "utilization": 0.14, "resetsAt": 1_763_000_000, "status": "allowed" },
                "seven_day": { "utilization": 0.81, "resetsAt": 1_763_143_200 },
                "seven_day_overage_included": { "utilization": 0, "resetsAt": 1_763_143_200 },
            },
        }))
        .expect("an object parses");

        assert_eq!(rate_limit.status, Some(RateLimitStatus::Warning));
        assert_eq!(rate_limit.limit_type, Some(RateLimitType::SevenDay));
        assert_eq!(rate_limit.resets_at, Some(1_763_143_200));

        let mut windows = rate_limit.usage_windows();
        windows.sort_by_key(|window| window.limit_type.sort_order());
        assert_eq!(
            windows,
            vec![
                window(
                    RateLimitType::FiveHour,
                    Some(0.14),
                    Some(1_763_000_000),
                    Some(RateLimitStatus::Allowed)
                ),
                // The window keeps its own reading; the top-level status marks it.
                window(
                    RateLimitType::SevenDay,
                    Some(0.81),
                    Some(1_763_143_200),
                    Some(RateLimitStatus::Warning)
                ),
                window(
                    RateLimitType::SevenDayOverageIncluded,
                    Some(0.),
                    Some(1_763_143_200),
                    None
                ),
            ]
        );
    }

    #[test]
    fn test_parses_rate_limit_info_leniently() {
        assert_eq!(ClaudeRateLimit::parse(&json!("allowed")), None);
        assert_eq!(
            ClaudeRateLimit::parse(&json!({})),
            Some(ClaudeRateLimit::default())
        );
        assert_eq!(
            ClaudeRateLimit::parse(&json!({})).map(|rate_limit| rate_limit.usage_windows()),
            Some(Vec::new())
        );

        let rate_limit = ClaudeRateLimit::parse(&json!({
            "status": "some_new_status",
            "rateLimitType": "seven_day_opus",
            "utilization": "high",
            "resetsAt": 1_763_143_200.7,
            "unifiedWindows": {
                "five_hour": "not an object",
                "seven_day": { "utilization": null },
                "thirty_day": { "utilization": 0.5 },
            },
        }))
        .expect("an object parses");
        assert_eq!(rate_limit.status, None);
        assert_eq!(rate_limit.utilization, None);
        assert_eq!(rate_limit.resets_at, Some(1_763_143_200));
        let mut windows = rate_limit.usage_windows();
        windows.sort_by_key(|window| window.limit_type.sort_order());
        assert_eq!(
            windows,
            vec![
                window(RateLimitType::SevenDay, None, None, None),
                window(RateLimitType::SevenDayOpus, None, Some(1_763_143_200), None),
                window(
                    RateLimitType::Other("thirty_day".into()),
                    Some(0.5),
                    None,
                    None
                ),
            ]
        );
        assert_eq!(windows[2].limit_type.label(), "Thirty day");

        // Refused without naming a limit.
        let rate_limit =
            ClaudeRateLimit::parse(&json!({ "status": "rejected", "resetsAt": 1_763_000_000 }))
                .expect("an object parses");
        assert_eq!(
            rate_limit.usage_windows(),
            vec![window(
                RateLimitType::Unspecified,
                None,
                Some(1_763_000_000),
                Some(RateLimitStatus::Rejected)
            )]
        );
    }

    #[test]
    fn test_reads_rate_limit_from_usage_meta() {
        let meta = json!({
            "_claude/model": "claude-opus-5-5",
            "_claude/rateLimit": { "status": "allowed", "unifiedWindows": {
                "seven_day": { "utilization": 0.14, "resetsAt": 1_763_143_200 }
            } },
        });
        let meta = meta.as_object().expect("an object");
        let rate_limit = meta
            .get(RATE_LIMIT_META_KEY)
            .and_then(ClaudeRateLimit::parse)
            .expect("rate limit present");
        assert_eq!(rate_limit.status, Some(RateLimitStatus::Allowed));
        assert_eq!(rate_limit.windows.len(), 1);
    }

    #[test]
    fn test_format_percent() {
        assert_eq!(format_percent(0.), "0%");
        assert_eq!(format_percent(0.004), "0%");
        assert_eq!(format_percent(0.14), "14%");
        assert_eq!(format_percent(0.996), "100%");
        assert_eq!(format_percent(1.), "100%");
        assert_eq!(format_percent(1.07), "107%");
    }

    #[test]
    fn test_format_reset_time() {
        // 2026-11-14 13:00:00 UTC.
        assert_eq!(
            format_reset_time(1_794_661_200, &Utc).as_deref(),
            Some("Resets Sat, Nov 14 at 1:00 PM")
        );
        let pacific = chrono::FixedOffset::west_opt(8 * 3600).expect("valid offset");
        assert_eq!(
            format_reset_time(1_794_661_200, &pacific).as_deref(),
            Some("Resets Sat, Nov 14 at 5:00 AM")
        );
        assert_eq!(format_reset_time(i64::MAX, &Utc), None);
    }

    #[test]
    fn test_latest_windows_keep_the_newest_reading_per_type() {
        let now = 1_000;
        let readings = vec![
            (
                window(RateLimitType::SevenDay, Some(0.10), Some(5_000), None),
                1,
            ),
            (
                window(RateLimitType::SevenDay, Some(0.12), Some(5_000), None),
                3,
            ),
            (
                window(RateLimitType::FiveHour, Some(0.50), Some(2_000), None),
                2,
            ),
            // Reset since it was read.
            (
                window(RateLimitType::SevenDayOpus, Some(0.90), Some(900), None),
                4,
            ),
            // Nothing to show.
            (
                window(
                    RateLimitType::SevenDaySonnet,
                    None,
                    Some(5_000),
                    Some(RateLimitStatus::Allowed),
                ),
                5,
            ),
        ];
        assert_eq!(
            merge_usage(&readings, None, now),
            vec![
                window(RateLimitType::FiveHour, Some(0.50), Some(2_000), None),
                window(RateLimitType::SevenDay, Some(0.12), Some(5_000), None),
            ]
        );
    }

    #[test]
    fn test_merge_reading_keeps_one_window_per_type() {
        let parse = |value: Value| ClaudeRateLimit::parse(&value).expect("an object parses");
        let mut stored = Vec::new();
        merge_reading(
            &mut stored,
            &parse(json!({
                "status": "allowed_warning",
                "rateLimitType": "seven_day_opus",
                "utilization": 0.9,
                "resetsAt": 5_000,
                "unifiedWindows": { "seven_day": { "utilization": 0.4, "resetsAt": 5_000 } },
            })),
            1,
        );
        merge_reading(
            &mut stored,
            &parse(json!({
                "status": "allowed",
                "unifiedWindows": { "seven_day": { "utilization": 0.5, "resetsAt": 5_000 } },
            })),
            2,
        );
        stored.sort_by_key(|(window, _)| window.limit_type.sort_order());
        assert_eq!(
            stored,
            vec![
                (
                    window(RateLimitType::SevenDay, Some(0.5), Some(5_000), None),
                    2
                ),
                // Still the last reading, but no longer near the limit.
                (
                    window(RateLimitType::SevenDayOpus, Some(0.9), Some(5_000), None),
                    1
                ),
            ]
        );

        merge_reading(
            &mut stored,
            &parse(json!({ "status": "rejected", "resetsAt": 6_000 })),
            3,
        );
        assert!(stored.iter().any(|(window, _)| {
            window.limit_type == RateLimitType::Unspecified
                && window.status == Some(RateLimitStatus::Rejected)
        }));
        merge_reading(&mut stored, &parse(json!({ "status": "allowed" })), 4);
        assert!(
            stored
                .iter()
                .all(|(window, _)| window.limit_type != RateLimitType::Unspecified)
        );
    }

    #[test]
    fn test_labels() {
        assert_eq!(RateLimitType::FiveHour.label(), "Session (5 hour)");
        assert_eq!(RateLimitType::SevenDay.label(), "Weekly (7 day)");
        assert_eq!(
            RateLimitType::SevenDayOverageIncluded.label(),
            "Fable limit"
        );
        assert_eq!(RateLimitType::SevenDayOpus.label(), "Opus limit");
    }

    /// The shape Claude Code writes, with every key this code must not read
    /// replaced by a placeholder of the same kind.
    fn account_file(limits: Value) -> String {
        json!({
            "userID": "placeholder-user-id",
            "primaryApiKey": "placeholder-key",
            "oauthAccount": {
                "accountUuid": "placeholder-uuid",
                "emailAddress": " person@example.com ",
                "organizationName": "Placeholder Org",
                "planDisplayName": "Max",
            },
            "cachedUsageUtilization": {
                "fetchedAtMs": 1_794_000_000_000_i64,
                "accountUuid": "placeholder-uuid",
                "utilization": {
                    "five_hour": null,
                    "seven_day": { "utilization": 15, "resets_at": "2026-11-14T13:00:00.000000+00:00" },
                    "extra_usage": { "is_enabled": false, "monthly_limit": 0 },
                    "spend": { "percent": 0, "severity": "normal" },
                    "limits": limits,
                },
            },
        })
        .to_string()
    }

    #[test]
    fn test_parses_the_account_file() {
        let account = parse_account_file(&account_file(json!([
            {
                "kind": "weekly_all", "group": "weekly", "percent": 15, "severity": "normal",
                "resets_at": "2026-11-14T13:00:00.000000+00:00", "scope": null, "is_active": true,
            },
            {
                "kind": "weekly_scoped", "group": "weekly", "percent": 0, "severity": "warning",
                "resets_at": "2026-11-14T13:00:00.000000+00:00", "is_active": false,
                "scope": { "model": { "display_name": "Fable", "id": "placeholder" }, "surface": null },
            },
            {
                "kind": "session", "group": "session", "percent": 96.5, "severity": "critical",
                "resets_at": "2026-11-12T18:00:00Z",
            },
        ])));
        assert_eq!(account.email.as_deref(), Some("person@example.com"));
        let usage = account.usage.expect("cached usage");
        assert_eq!(usage.fetched_at_ms, 1_794_000_000_000);
        assert_eq!(
            usage.windows,
            vec![
                window(
                    RateLimitType::SevenDay,
                    Some(0.15),
                    Some(1_794_661_200),
                    Some(RateLimitStatus::Allowed)
                ),
                labeled(
                    window(
                        RateLimitType::SevenDayOverageIncluded,
                        Some(0.),
                        Some(1_794_661_200),
                        Some(RateLimitStatus::Warning)
                    ),
                    "Fable limit"
                ),
                window(
                    RateLimitType::FiveHour,
                    Some(0.965),
                    Some(1_794_506_400),
                    Some(RateLimitStatus::Critical)
                ),
            ]
        );
        let labels = usage
            .windows
            .iter()
            .map(|window| window.label())
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            ["Weekly (7 day)", "Fable limit", "Session (5 hour)"]
        );
    }

    #[test]
    fn test_account_file_unknown_limits_and_shapes() {
        let account = parse_account_file(&account_file(json!([
            { "kind": "monthly_all", "percent": 40, "severity": "normal" },
            { "kind": "weekly_scoped", "percent": 10, "scope": { "model": { "display_name": "Opus" } } },
            { "kind": "weekly_scoped", "percent": 20, "scope": { "model": { "display_name": "Fable" } } },
            { "kind": "weekly_scoped", "percent": 30, "scope": { "model": { "display_name": "Mythos Max" } } },
            { "kind": "weekly_scoped", "percent": 5, "scope": { "surface": { "display_name": "Claude Code" } } },
            { "kind": "future_meter", "percent": "high", "severity": 3, "resets_at": "tomorrow" },
            "not an object",
            { "percent": 50 },
        ])));
        let windows = account.usage.expect("cached usage").windows;
        let rows = windows
            .iter()
            .map(|window| {
                (
                    window.limit_type.id().to_string(),
                    window.label().to_string(),
                    window.utilization,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            vec![
                (
                    "monthly_all".to_string(),
                    "Monthly all".to_string(),
                    Some(0.4)
                ),
                (
                    "seven_day_opus".to_string(),
                    "Opus limit".to_string(),
                    Some(0.1)
                ),
                // With two other scoped models, neither is known to be the
                // overage-included window.
                (
                    "weekly_scoped_fable".to_string(),
                    "Fable limit".to_string(),
                    Some(0.2)
                ),
                (
                    "weekly_scoped_mythos_max".to_string(),
                    "Mythos Max limit".to_string(),
                    Some(0.3)
                ),
                (
                    "weekly_scoped_claude_code".to_string(),
                    "Claude Code limit".to_string(),
                    Some(0.05)
                ),
                ("future_meter".to_string(), "Future meter".to_string(), None),
            ]
        );
        assert_eq!(windows[5].status, None);
        assert_eq!(windows[5].resets_at, None);
    }

    #[test]
    fn test_account_file_fails_closed() {
        assert_eq!(parse_account_file(""), ClaudeAccountCache::default());
        assert_eq!(
            parse_account_file("{ not json"),
            ClaudeAccountCache::default()
        );
        assert_eq!(parse_account_file("[]"), ClaudeAccountCache::default());
        assert_eq!(parse_account_file("{}"), ClaudeAccountCache::default());
        // Signed out.
        assert_eq!(
            parse_account_file(r#"{ "oauthAccount": null, "cachedUsageUtilization": null }"#),
            ClaudeAccountCache::default()
        );
        // A section that changed type entirely drops the whole file.
        assert_eq!(
            parse_account_file(r#"{ "oauthAccount": "person@example.com" }"#),
            ClaudeAccountCache::default()
        );
        // A field of an unexpected shape only drops that field.
        let account = parse_account_file(
            r#"{
                "oauthAccount": { "emailAddress": 42 },
                "cachedUsageUtilization": { "fetchedAtMs": "soon", "utilization": { "limits": { "kind": "session" } } }
            }"#,
        );
        assert_eq!(account, ClaudeAccountCache::default());
        let account = parse_account_file(
            r#"{ "cachedUsageUtilization": { "utilization": { "limits": [ { "kind": "session", "percent": 12 } ] } } }"#,
        );
        assert_eq!(
            account.usage,
            Some(CachedUsage {
                fetched_at_ms: 0,
                windows: vec![window(RateLimitType::FiveHour, Some(0.12), None, None)],
            })
        );
    }

    #[test]
    fn test_newer_source_wins_per_window() {
        let now = 1_000;
        let cache = CachedUsage {
            fetched_at_ms: 50_000,
            windows: vec![
                window(
                    RateLimitType::SevenDay,
                    Some(0.15),
                    Some(5_000),
                    Some(RateLimitStatus::Allowed),
                ),
                labeled(
                    window(
                        RateLimitType::SevenDayOverageIncluded,
                        Some(0.),
                        Some(5_000),
                        Some(RateLimitStatus::Allowed),
                    ),
                    "Mythos limit",
                ),
                // Reset since the cache was fetched.
                window(
                    RateLimitType::Other("monthly_all".into()),
                    Some(0.5),
                    Some(900),
                    None,
                ),
            ],
        };
        let readings = vec![
            // Older than the cache.
            (
                window(RateLimitType::SevenDay, Some(0.10), Some(5_000), None),
                40_000,
            ),
            // Newer than the cache: replaces its reading, keeps its label.
            (
                window(
                    RateLimitType::SevenDayOverageIncluded,
                    Some(0.30),
                    Some(5_000),
                    Some(RateLimitStatus::Warning),
                ),
                60_000,
            ),
            // Not in the cache.
            (
                window(RateLimitType::FiveHour, Some(0.42), Some(2_000), None),
                10_000,
            ),
        ];
        assert_eq!(
            merge_usage(&readings, Some(&cache), now),
            vec![
                window(
                    RateLimitType::SevenDay,
                    Some(0.15),
                    Some(5_000),
                    Some(RateLimitStatus::Allowed)
                ),
                labeled(
                    window(
                        RateLimitType::SevenDayOverageIncluded,
                        Some(0.30),
                        Some(5_000),
                        Some(RateLimitStatus::Warning)
                    ),
                    "Mythos limit"
                ),
                window(RateLimitType::FiveHour, Some(0.42), Some(2_000), None),
            ]
        );
        // A reading of unknown age never beats the cache.
        let unknown_age = vec![(
            window(RateLimitType::SevenDay, Some(0.99), Some(5_000), None),
            0,
        )];
        assert_eq!(
            merge_usage(&unknown_age, Some(&cache), now)
                .first()
                .and_then(|window| window.utilization),
            Some(0.15)
        );
        assert!(merge_usage(&[], None, now).is_empty());
    }

    #[gpui::test]
    fn test_readings_outlive_closed_thread_views(cx: &mut TestAppContext) {
        let project = cx.update(|cx| cx.new(|_| ()));
        let other_project = cx.update(|cx| cx.new(|_| ()));
        let thread_view = cx.update(|cx| cx.new(|_| ()));
        let project_id = project.entity_id();
        let resets_at = Utc::now().timestamp() + 3_600;
        let meta = acp_v2::Meta::from_iter([(
            RATE_LIMIT_META_KEY.into(),
            json!({
                "status": "allowed",
                "unifiedWindows": { "seven_day": { "utilization": 0.14, "resetsAt": resets_at } },
            }),
        )]);
        let rows = |cx: &mut TestAppContext, project_id: EntityId| {
            cx.update(|cx| {
                project_usage_windows(project_id, Utc::now().timestamp(), cx)
                    .into_iter()
                    .map(|window| (window.label(), window.utilization))
                    .collect::<Vec<_>>()
            })
        };

        cx.update(|cx| {
            record_usage_meta(thread_view.entity_id(), project_id, Some(&meta), true, cx)
        });
        let expected = vec![(SharedString::from("Weekly (7 day)"), Some(0.14))];
        assert_eq!(rows(cx, project_id), expected);

        cx.update(|cx| retain_thread_readings(thread_view.entity_id(), cx));
        assert!(cx.read(|cx| {
            !cx.global::<ClaudeUsageStore>()
                .threads
                .contains_key(&thread_view.entity_id())
        }));
        assert_eq!(rows(cx, project_id), expected, "kept after the view closed");
        assert!(rows(cx, other_project.entity_id()).is_empty());
    }

    #[gpui::test]
    async fn test_watches_the_account_file(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let path = Path::new("/home/person/.claude.json");
        fs.insert_tree("/home/person", json!({})).await;
        fs.insert_file(
            path,
            account_file(json!([{ "kind": "weekly_all", "percent": 15, "severity": "normal" }]))
                .into_bytes(),
        )
        .await;

        let watch = cx.update(|cx| watch_account_file_at(fs.clone(), path.to_path_buf(), cx));
        cx.run_until_parked();
        let read_usage = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                let email = account_email(cx);
                let windows = project_usage_windows(EntityId::from(1_u64), 0, cx)
                    .into_iter()
                    .map(|window| window.utilization)
                    .collect::<Vec<_>>();
                (email, windows)
            })
        };
        assert_eq!(
            read_usage(cx),
            (Some("person@example.com".into()), vec![Some(0.15)])
        );

        // A second watcher shares the first one's task.
        let second_watch =
            cx.update(|cx| watch_account_file_at(fs.clone(), path.to_path_buf(), cx));
        assert!(Rc::ptr_eq(&watch._task, &second_watch._task));

        fs.insert_file(
            path,
            account_file(json!([{ "kind": "weekly_all", "percent": 40, "severity": "warning" }]))
                .into_bytes(),
        )
        .await;
        cx.executor().advance_clock(ACCOUNT_FILE_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(
            read_usage(cx),
            (Some("person@example.com".into()), vec![Some(0.4)])
        );

        fs.insert_file(path, b"{ \"oauthAccount\": ".to_vec()).await;
        cx.executor().advance_clock(ACCOUNT_FILE_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(read_usage(cx), (None, Vec::new()), "fails closed");

        drop(watch);
        drop(second_watch);
        assert!(cx.read(|cx| {
            cx.global::<ClaudeUsageStore>()
                .account_watch
                .upgrade()
                .is_none()
        }));
    }
}
