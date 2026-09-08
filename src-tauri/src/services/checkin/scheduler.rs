//! Desktop-owned daily scheduler. It runs independently of the rendered page,
//! catches up after startup/resume, and never performs concurrent check-in batches.

use super::{executor, CheckinConfig, CheckinSite, CheckinStatus};
use crate::store::AppState;
use chrono::{DateTime, Local, NaiveDate, TimeZone, Timelike};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

const SCHEDULE_POLL_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct CheckinRuntime {
    pub(crate) execution: Mutex<()>,
    pub(crate) config_write: std::sync::Mutex<()>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl CheckinRuntime {
    pub async fn start(&self, app: tauri::AppHandle, state: AppState) {
        let mut task = self.task.lock().await;
        if task.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        *task = Some(tokio::spawn(async move {
            let mut ticks = tokio::time::interval(SCHEDULE_POLL_INTERVAL);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticks.tick().await;
                if let Err(error) = executor::run_scheduled(Some(&app), &state, Local::now()).await
                {
                    log::error!("[Checkin] 自动签到执行失败: {error}");
                }
            }
        }));
    }

    pub async fn shutdown(&self) {
        let task = self.task.lock().await.take();
        if let Some(task) = task {
            task.abort();
            if let Err(error) = task.await {
                if !error.is_cancelled() {
                    log::warn!("[Checkin] 调度任务异常退出: {error}");
                }
            }
        }
    }
}

pub(super) fn schedule_is_due(config: &CheckinConfig, now: DateTime<Local>) -> bool {
    if !config.schedule_enabled
        || now.hour() < u32::from(config.schedule_hour)
        || !config.sites.iter().any(|site| site.enabled)
    {
        return false;
    }
    let last_date = config
        .last_run_date
        .as_deref()
        .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok());
    last_date.is_none_or(|date| date < now.date_naive())
}

pub(super) fn succeeded_on_date(site: &CheckinSite, date: NaiveDate) -> bool {
    site.last_result.as_ref().is_some_and(|result| {
        result.status == CheckinStatus::Success
            && Local
                .timestamp_opt(result.at, 0)
                .single()
                .is_some_and(|at| at.date_naive() == date)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> CheckinConfig {
        serde_json::from_value(serde_json::json!({
            "scheduleEnabled": true, "scheduleHour": 9,
            "sites": [{"id":"site", "name":"test", "authKind":"header", "request":{"url":"http://localhost/"}}]
        })).unwrap()
    }

    #[test]
    fn schedule_waits_until_hour_then_catches_up_once_per_day() {
        let mut config = configured();
        let morning = Local.with_ymd_and_hms(2026, 9, 8, 8, 59, 0).unwrap();
        assert!(!schedule_is_due(&config, morning));
        let late_start = Local.with_ymd_and_hms(2026, 9, 8, 15, 0, 0).unwrap();
        assert!(schedule_is_due(&config, late_start));
        config.last_run_date = Some("2026-09-08".into());
        assert!(!schedule_is_due(&config, late_start));
        assert!(schedule_is_due(
            &config,
            late_start + chrono::Duration::days(1)
        ));
        // Clock rollback must not cause a second batch for an earlier date.
        assert!(!schedule_is_due(
            &config,
            late_start - chrono::Duration::days(1)
        ));
    }

    #[test]
    fn disabled_or_empty_schedule_does_not_run() {
        let now = Local.with_ymd_and_hms(2026, 9, 8, 15, 0, 0).unwrap();
        let mut config = configured();
        config.schedule_enabled = false;
        assert!(!schedule_is_due(&config, now));
        config.schedule_enabled = true;
        config.sites[0].enabled = false;
        assert!(!schedule_is_due(&config, now));
    }
}
