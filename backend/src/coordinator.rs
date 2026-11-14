use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use gateway_plugin_sdk::PluginFault;
use gateway_plugin_sdk::call::{data::AccountFacts, host::LogLevel};
use gateway_plugin_sdk::client::HostClient;
use serde::Serialize;
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::eval_runner::{
    CANDY_MAX_RUNS, pelican_variant_spec, run_candy_eval, run_fingerprint_eval, run_pelican_eval,
};
use crate::host_calls::{self, CancelFlag};
use crate::state_store::{
    CandyEvalSettings, EvalMode, QueueState, QueueStatus, TriggerKind, append_record, load_queue,
    load_settings, save_queue,
};

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveEvalProgress {
    pub active: bool,
    pub current_account_id: Option<String>,
    pub current_account_name: Option<String>,
    pub current_item_done: u32,
    pub current_item_total: u32,
}

#[derive(Default)]
pub struct EvalCoordinator {
    run_lock: AsyncMutex<()>,
    cancel_slot: StdMutex<Option<CancelFlag>>,
    live: Arc<StdMutex<LiveEvalProgress>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepOutcome {
    pub busy: bool,
    pub processed: bool,
    pub remaining: usize,
    pub finished: bool,
}

impl EvalCoordinator {
    #[must_use]
    pub fn snapshot(&self) -> LiveEvalProgress {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub async fn start_manual_job(
        &self,
        host: &HostClient,
        settings: CandyEvalSettings,
        account_ids: Vec<String>,
    ) -> Result<QueueState, PluginFault> {
        let _guard = self.run_lock.lock().await;
        let (mut queue, version) = load_queue(host).await?;
        let now_ms = Utc::now().timestamp_millis();
        queue.active_job_id = Some(Uuid::now_v7().to_string());
        queue.status = QueueStatus::Running;
        queue.stop_requested = false;
        queue.trigger_kind = TriggerKind::Manual;
        queue.mode = settings.mode;
        queue.model = settings.model;
        queue.effort = settings.effort;
        queue.runs = settings.runs.clamp(1, CANDY_MAX_RUNS);
        queue.fingerprint_concurrency = settings.fingerprint_concurrency.clamp(1, 3);
        queue.pelican_variant = settings.pelican_variant;
        queue.client_key_id = settings.client_key_id;
        queue.total = account_ids.len() as u32;
        queue.completed = 0;
        queue.pending_account_ids = account_ids;
        queue.started_at_ms = Some(now_ms);
        queue.updated_at_ms = now_ms;
        save_queue(host, version, &queue).await?;
        Ok(queue)
    }

    pub async fn stop_current_job(&self, host: &HostClient) -> Result<QueueState, PluginFault> {
        if let Some(flag) = self
            .cancel_slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .cloned()
        {
            flag.cancel();
        }
        let (mut queue, version) = load_queue(host).await?;
        if queue.status != QueueStatus::Idle {
            queue.stop_requested = true;
            queue.status = QueueStatus::Idle;
            queue.pending_account_ids.clear();
            queue.updated_at_ms = Utc::now().timestamp_millis();
            save_queue(host, version, &queue).await?;
        }
        self.clear_live();
        Ok(queue)
    }

    /// 供管理页驱动单步立即执行：若后台 maintenance 正在运行则返回 `busy: true`
    pub async fn try_step_one(&self, host: &HostClient) -> Result<StepOutcome, PluginFault> {
        let Ok(_guard) = self.run_lock.try_lock() else {
            return Ok(StepOutcome {
                busy: true,
                processed: false,
                remaining: 0,
                finished: false,
            });
        };
        self.execute_next_account(host).await
    }

    /// 宿主每 30 秒通过 `plugin.reconcile` 唤醒：负责检查每日定时计划，并在后台排空队列。
    pub async fn reconcile(&self, host: &HostClient, deadline: Instant) -> Result<(), PluginFault> {
        let Ok(_guard) = self.run_lock.try_lock() else {
            return Ok(());
        };
        let (settings, _) = load_settings(host).await?;
        let (mut queue, mut queue_version) = load_queue(host).await?;

        if settings.schedule_enabled
            && !settings.selected_account_ids.is_empty()
            && queue.pending_account_ids.is_empty()
            && queue.status == QueueStatus::Idle
            && let Some(today) = should_trigger_today(&settings, &queue.last_scheduled_date)
        {
            let now_ms = Utc::now().timestamp_millis();
            queue.active_job_id = Some(Uuid::now_v7().to_string());
            queue.status = QueueStatus::Running;
            queue.stop_requested = false;
            queue.trigger_kind = TriggerKind::Scheduled;
            queue.mode = settings.mode;
            queue.model = settings.model.clone();
            queue.effort = settings.effort.clone();
            queue.runs = settings.runs.clamp(1, CANDY_MAX_RUNS);
            queue.fingerprint_concurrency = settings.fingerprint_concurrency.clamp(1, 3);
            queue.pelican_variant = settings.pelican_variant;
            queue.client_key_id = settings.client_key_id.clone();
            queue.total = settings.selected_account_ids.len() as u32;
            queue.completed = 0;
            queue.pending_account_ids = settings.selected_account_ids.clone();
            queue.started_at_ms = Some(now_ms);
            queue.updated_at_ms = now_ms;
            queue.last_scheduled_date = today.clone();
            queue_version = Some(save_queue(host, queue_version, &queue).await?);
            host_calls::log_event(
                host,
                LogLevel::Info,
                "candy_eval.scheduled_triggered",
                BTreeMap::from([
                    ("date".to_owned(), json!(today)),
                    ("accounts".to_owned(), json!(queue.total)),
                    ("mode".to_owned(), json!(queue.mode.as_str())),
                ]),
            )
            .await;
        }
        let _ = queue_version;

        // 给每轮 reconcile 留足预算余量（至少剩余 90s 才开下一账号），避免超时中断
        loop {
            let remaining_budget = deadline.saturating_duration_since(Instant::now());
            if remaining_budget < Duration::from_secs(90) {
                break;
            }
            let outcome = self.execute_next_account(host).await?;
            if !outcome.processed || outcome.finished {
                break;
            }
        }
        Ok(())
    }

    async fn execute_next_account(&self, host: &HostClient) -> Result<StepOutcome, PluginFault> {
        let (mut queue, version) = load_queue(host).await?;
        if queue.stop_requested || queue.pending_account_ids.is_empty() {
            if queue.status != QueueStatus::Idle || !queue.pending_account_ids.is_empty() {
                queue.status = QueueStatus::Idle;
                queue.pending_account_ids.clear();
                queue.updated_at_ms = Utc::now().timestamp_millis();
                let _ = save_queue(host, version, &queue).await;
            }
            self.clear_live();
            return Ok(StepOutcome {
                busy: false,
                processed: false,
                remaining: 0,
                finished: true,
            });
        }

        let accounts = host_calls::list_all_openai_accounts(host).await?;
        let account_map: HashMap<String, AccountFacts> = accounts
            .iter()
            .cloned()
            .map(|a| (a.account_id.clone(), a))
            .collect();
        let all_account_ids: Vec<String> = accounts.iter().map(|a| a.account_id.clone()).collect();

        let account_id = queue.pending_account_ids[0].clone();
        let account_name = account_map
            .get(&account_id)
            .map(|a| {
                if let Some(email) = &a.email
                    && !email.is_empty()
                    && email != &a.name
                {
                    format!("{} ({email})", a.name)
                } else {
                    a.name.clone()
                }
            })
            .unwrap_or_else(|| account_id.clone());

        let item_total = match queue.mode {
            EvalMode::Candy => queue.runs.clamp(1, CANDY_MAX_RUNS),
            EvalMode::Fingerprint => 3,
            EvalMode::Pelican => 1,
        };
        let cancel = CancelFlag::new();
        *self
            .cancel_slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cancel.clone());
        {
            let mut live = self
                .live
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            live.active = true;
            live.current_account_id = Some(account_id.clone());
            live.current_account_name = Some(account_name.clone());
            live.current_item_done = 0;
            live.current_item_total = item_total;
        }

        let effective_key_id = if queue.client_key_id.trim().is_empty() {
            host_calls::ensure_runner_client_key(host, &all_account_ids)
                .await?
                .id
        } else {
            queue.client_key_id.trim().to_owned()
        };

        let live_ref = Arc::clone(&self.live);
        let on_progress = move |done: u32| {
            let mut live = live_ref
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            live.current_item_done = done;
        };

        let completed_record = match queue.mode {
            EvalMode::Candy => {
                run_candy_eval(
                    host,
                    &effective_key_id,
                    &account_id,
                    &account_name,
                    &queue.model,
                    &queue.effort,
                    queue.runs,
                    queue.trigger_kind,
                    &cancel,
                    on_progress,
                )
                .await
            }
            EvalMode::Fingerprint => Some(
                run_fingerprint_eval(
                    host,
                    &effective_key_id,
                    &account_id,
                    &account_name,
                    &queue.model,
                    queue.fingerprint_concurrency,
                    queue.trigger_kind,
                    &cancel,
                    on_progress,
                )
                .await,
            ),
            EvalMode::Pelican => Some(
                run_pelican_eval(
                    host,
                    &effective_key_id,
                    &account_id,
                    &account_name,
                    &queue.model,
                    &queue.effort,
                    pelican_variant_spec(queue.pelican_variant),
                    queue.trigger_kind,
                    &cancel,
                )
                .await,
            ),
        };

        *self
            .cancel_slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;

        if let Some(record) = completed_record {
            append_record(host, record.header, record.detail).await?;
        }

        // 重新读取队列，保留在执行期间可能通过 POST /api/stop 提交的状态变更
        let (mut latest_queue, latest_version) = load_queue(host).await?;
        let cancelled_during_step = cancel.is_cancelled() || latest_queue.stop_requested;
        if cancelled_during_step {
            latest_queue.status = QueueStatus::Idle;
            latest_queue.pending_account_ids.clear();
        } else {
            if latest_queue.pending_account_ids.first() == Some(&account_id) {
                latest_queue.pending_account_ids.remove(0);
            }
            latest_queue.completed = latest_queue.completed.saturating_add(1);
            if latest_queue.pending_account_ids.is_empty() {
                latest_queue.status = QueueStatus::Idle;
            }
        }
        latest_queue.updated_at_ms = Utc::now().timestamp_millis();
        save_queue(host, latest_version, &latest_queue).await?;

        let remaining = latest_queue.pending_account_ids.len();
        let finished = remaining == 0 || latest_queue.status == QueueStatus::Idle;
        if finished {
            self.clear_live();
        }

        Ok(StepOutcome {
            busy: false,
            processed: true,
            remaining,
            finished,
        })
    }

    fn clear_live(&self) {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *live = LiveEvalProgress::default();
    }
}

#[must_use]
pub fn should_trigger_today(settings: &CandyEvalSettings, last_date: &str) -> Option<String> {
    let tz: chrono_tz::Tz = settings
        .timezone
        .parse()
        .unwrap_or(chrono_tz::Asia::Shanghai);
    let now_local = Utc::now().with_timezone(&tz);
    let today = now_local.format("%Y-%m-%d").to_string();
    if last_date == today {
        return None;
    }
    let hhmm = now_local.format("%H:%M").to_string();
    if hhmm >= settings.schedule_time {
        Some(today)
    } else {
        None
    }
}
