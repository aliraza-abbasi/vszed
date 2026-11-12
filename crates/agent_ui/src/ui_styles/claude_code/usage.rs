use acp_thread::AcpThreadEvent;
use agent_client_protocol::v2::schema as acp_v2;
use chrono::{DateTime, TimeZone};
use collections::HashMap;
use gpui::{App, Context, EntityId, FontWeight, Global, Hsla, px};
use serde_json::Value;
use ui::prelude::*;

use crate::conversation_view::ThreadView;

/// The usage update `_meta` key claude-agent-acp puts the Claude CLI's
/// `rate_limit_info` under.
pub(crate) const RATE_LIMIT_META_KEY: &str = "_claude/rateLimit";

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
    /// Past a warning threshold (`allowed_warning`).
    Warning,
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
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UsageWindow {
    pub limit_type: RateLimitType,
    /// The fraction of the window used. Above 1 when usage legitimately ran
    /// past the window's cap.
    pub utilization: Option<f64>,
    /// Unix time in seconds.
    pub resets_at: Option<i64>,
    pub status: Option<RateLimitStatus>,
}

impl UsageWindow {
    fn has_reset(&self, now: i64) -> bool {
        self.resets_at.is_some_and(|resets_at| resets_at <= now)
    }

    fn has_anything_to_show(&self) -> bool {
        self.utilization.is_some()
            || matches!(
                self.status,
                Some(RateLimitStatus::Warning | RateLimitStatus::Rejected)
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

/// The usage windows to show: the most recently received reading of each
/// limit type, without the windows that have reset since.
pub(crate) fn latest_windows<'a>(
    readings: impl IntoIterator<Item = &'a (UsageWindow, u64)>,
    now: i64,
) -> Vec<UsageWindow> {
    let mut latest: HashMap<&RateLimitType, &(UsageWindow, u64)> = HashMap::default();
    for reading in readings {
        let (window, sequence) = reading;
        match latest.get(&window.limit_type) {
            Some((_, latest_sequence)) if latest_sequence > sequence => {}
            _ => {
                latest.insert(&window.limit_type, reading);
            }
        }
    }
    let mut windows = latest
        .into_values()
        .map(|(window, _)| window)
        .filter(|window| !window.has_reset(now) && window.has_anything_to_show())
        .cloned()
        .collect::<Vec<_>>();
    windows.sort_by_key(|window| (window.limit_type.sort_order(), window.limit_type.id()));
    windows
}

struct ThreadUsage {
    project_id: EntityId,
    last_info: Value,
    /// The latest reading of each limit type, with the store's sequence
    /// number at the time it arrived.
    windows: Vec<(UsageWindow, u64)>,
}

/// The rate limit readings of every open thread view, so a panel can show the
/// most recent reading across a project's threads. Each Claude session runs
/// its own CLI process, which reports only what its own requests observed.
#[derive(Default)]
struct ClaudeUsageStore {
    next_sequence: u64,
    threads: HashMap<EntityId, ThreadUsage>,
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
    let release_subscription = cx.on_release(move |_, cx| {
        let has_readings = cx
            .try_global::<ClaudeUsageStore>()
            .is_some_and(|store| store.threads.contains_key(&thread_view_id));
        if has_readings {
            cx.default_global::<ClaudeUsageStore>()
                .threads
                .remove(&thread_view_id);
        }
    });
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
    let store = cx.default_global::<ClaudeUsageStore>();
    let sequence = if live {
        store.next_sequence += 1;
        store.next_sequence
    } else {
        0
    };
    let thread = store
        .threads
        .entry(thread_view_id)
        .or_insert_with(|| ThreadUsage {
            project_id,
            last_info: Value::Null,
            windows: Vec::new(),
        });
    thread.last_info = info.clone();
    merge_reading(&mut thread.windows, &rate_limit, sequence);
}

/// Updates a thread's stored windows with a new rate limit event. Windows the
/// event doesn't mention keep their last reading, because the CLI reports the
/// model and overage windows only while they limit requests.
fn merge_reading(
    stored: &mut Vec<(UsageWindow, u64)>,
    rate_limit: &ClaudeRateLimit,
    sequence: u64,
) {
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
            Some(reading) => *reading = (window, sequence),
            None => stored.push((window, sequence)),
        }
    }
}

/// The usage windows to show for a project, from its open threads.
pub(crate) fn project_usage_windows(project_id: EntityId, now: i64, cx: &App) -> Vec<UsageWindow> {
    let Some(store) = cx.try_global::<ClaudeUsageStore>() else {
        return Vec::new();
    };
    latest_windows(
        store
            .threads
            .values()
            .filter(|thread| thread.project_id == project_id)
            .flat_map(|thread| thread.windows.iter()),
        now,
    )
}

/// Observes usage changes, for views that show them.
pub(crate) fn observe_usage<V: 'static>(cx: &mut Context<V>) -> gpui::Subscription {
    cx.observe_global::<ClaudeUsageStore>(|_, cx| cx.notify())
}

pub(crate) fn render_account_and_usage(windows: &[UsageWindow], cx: &App) -> impl IntoElement {
    v_flex()
        .px_3()
        .pb_2()
        .gap_1()
        .child(subheading("Account"))
        .child(
            h_flex()
                .w_full()
                .items_start()
                .justify_between()
                .gap_3()
                .child(Label::new("Email").flex_none())
                .child(
                    div().flex_1().min_w_0().text_right().child(
                        Label::new("Not available through the Claude adapter")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                ),
        )
        .child(div().pt_2().child(subheading("Usage")))
        .map(|this| {
            if windows.is_empty() {
                this.child(
                    v_flex()
                        .debug_selector(|| "claude-code-usage-empty".to_string())
                        .child(
                            Label::new("Usage appears after Claude's next reply")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                        .child(
                            Label::new("Claude reports plan limits for claude.ai sign-ins only")
                                .size(LabelSize::XSmall)
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

fn subheading(text: &'static str) -> impl IntoElement {
    Label::new(text.to_uppercase())
        .size(LabelSize::Small)
        .weight(FontWeight::SEMIBOLD)
        .color(Color::Muted)
}

fn render_usage_window(window: &UsageWindow, cx: &App) -> impl IntoElement {
    let colors = cx.theme().colors();
    let status_colors = cx.theme().status();
    let (fill, percent_color, state) = match window.status {
        Some(RateLimitStatus::Rejected) => {
            (status_colors.error, Color::Error, Some("Limit reached"))
        }
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
                .child(Label::new(window.limit_type.label()).truncate())
                .when_some(window.utilization, |this, utilization| {
                    this.child(
                        Label::new(format_percent(utilization))
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
                    .size(LabelSize::XSmall)
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
    use chrono::Utc;
    use serde_json::json;

    fn window(
        limit_type: RateLimitType,
        utilization: Option<f64>,
        resets_at: Option<i64>,
        status: Option<RateLimitStatus>,
    ) -> UsageWindow {
        UsageWindow {
            limit_type,
            utilization,
            resets_at,
            status,
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
            latest_windows(&readings, now),
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
}
