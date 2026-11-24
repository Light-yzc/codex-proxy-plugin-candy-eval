use std::collections::{BTreeMap, HashMap, HashSet};
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
        queue.spread_minutes = 0;
        queue.next_run_at_ms = None;
        queue.window_end_at_ms = None;
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
            queue.spread_minutes = 0;
            queue.next_run_at_ms = None;
            queue.window_end_at_ms = None;
            queue.updated_at_ms = Utc::now().timestamp_millis();
            save_queue(host, version, &queue).await?;
        }
        self.clear_live();
        Ok(queue)
    }

    /// 供管理页驱动步进：避免在 120s 限制的管理 RPC 内同步跑模型请求，交由后台 maintenance (600s) 排空队列
    pub async fn try_step_one(&self, host: &HostClient) -> Result<StepOutcome, PluginFault> {
        let (queue, _) = load_queue(host).await?;
        let remaining = queue.pending_account_ids.len();
        if queue.stop_requested || remaining == 0 {
            return Ok(StepOutcome {
                busy: false,
                processed: false,
                remaining: 0,
                finished: true,
            });
        }
        Ok(StepOutcome {
            busy: true,
            processed: false,
            remaining,
            finished: false,
        })
    }

    /// 宿主每 30 秒通过 `plugin.reconcile` 唤醒：负责检查每日定时计划，并在后台排空队列。
    pub async fn reconcile(&self, host: &HostClient, deadline: Instant) -> Result<(), PluginFault> {
        let Ok(_guard) = self.run_lock.try_lock() else {
            return Ok(());
        };
        if let Err(err) = crate::lm_detector::maybe_auto_sync().await {
            host_calls::log_event(
                host,
                LogLevel::Warn,
                "candy_eval.detector_sync_failed",
                BTreeMap::from([("error".to_owned(), json!(err))]),
            )
            .await;
        }
        let (settings, _) = load_settings(host).await?;
        let (mut queue, mut queue_version) = load_queue(host).await?;

        if settings.schedule_enabled
            && queue.pending_account_ids.is_empty()
            && queue.status == QueueStatus::Idle
            && let Some(today) = should_trigger_today(&settings, &queue.last_scheduled_date)
        {
            let all_accounts = host_calls::list_all_openai_accounts(host).await?;
            let target_ids =
                select_target_accounts(&all_accounts, &settings.selected_account_ids, true);
            if target_ids.is_empty() {
                host_calls::log_event(
                    host,
                    LogLevel::Info,
                    "candy_eval.scheduled_skipped_no_accounts",
                    BTreeMap::from([("date".to_owned(), json!(today))]),
                )
                .await;
            } else {
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
                queue.total = target_ids.len() as u32;
                queue.completed = 0;
                queue.pending_account_ids = target_ids;
                let spread_minutes = settings.schedule_spread_minutes.min(360);
                queue.spread_minutes = spread_minutes;
                if spread_minutes > 0 {
                    let window_ms = i64::from(spread_minutes) * 60_000;
                    let count = i64::from(queue.total.max(1));
                    let max_initial = if count <= 1 {
                        window_ms
                    } else {
                        ((window_ms / count) * 7 / 10).max(1)
                    };
                    let initial_delay = random_millis(0, max_initial);
                    queue.window_end_at_ms = Some(now_ms + window_ms);
                    queue.next_run_at_ms = Some(now_ms + initial_delay);
                } else {
                    queue.window_end_at_ms = None;
                    queue.next_run_at_ms = None;
                }
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
                        ("spread_minutes".to_owned(), json!(spread_minutes)),
                    ]),
                )
                .await;
            }
        }
        let _ = queue_version;

        // 给每轮 reconcile 留足预算余量（至少剩余 150s 才开下一账号），避免超时中断
        loop {
            let (cur_queue, _) = load_queue(host).await?;
            if cur_queue.status != QueueStatus::Running
                || cur_queue.stop_requested
                || cur_queue.pending_account_ids.is_empty()
            {
                break;
            }
            let now_ms = Utc::now().timestamp_millis();
            if let Some(next_at) = cur_queue.next_run_at_ms
                && now_ms < next_at
            {
                break;
            }
            let remaining_budget = deadline.saturating_duration_since(Instant::now());
            if remaining_budget < Duration::from_secs(150) {
                break;
            }
            let outcome = self.execute_next_account(host).await?;
            if !outcome.processed || outcome.finished {
                break;
            }
            let (mut after_queue, after_ver) = load_queue(host).await?;
            if after_queue.spread_minutes > 0 && !after_queue.pending_account_ids.is_empty() {
                let done_ms = Utc::now().timestamp_millis();
                let window_end = after_queue.window_end_at_ms.unwrap_or(done_ms);
                let rem_window = window_end.saturating_sub(done_ms);
                if rem_window > 30_000 {
                    let rem_accounts = after_queue.pending_account_ids.len() as i64;
                    let avg_gap = (rem_window / rem_accounts.max(1)).max(30_000);
                    let low = (avg_gap * 65 / 100).max(30_000);
                    let high = (avg_gap * 115 / 100).min(rem_window).max(low);
                    let gap_ms = random_millis(low, high);
                    after_queue.next_run_at_ms = Some(done_ms + gap_ms);
                    after_queue.updated_at_ms = done_ms;
                    let _ = save_queue(host, after_ver, &after_queue).await;
                    break;
                }
                if after_queue.next_run_at_ms.is_some() {
                    after_queue.next_run_at_ms = None;
                    after_queue.updated_at_ms = done_ms;
                    let _ = save_queue(host, after_ver, &after_queue).await;
                }
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
            .map(|a| format_account_name(a, &account_id))
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

        if let Some(record) = completed_record
            && let Err(err) = append_record(host, record.header, record.detail).await
        {
            host_calls::log_event(
                host,
                LogLevel::Warn,
                "candy_eval.append_record_failed",
                BTreeMap::from([
                    ("account_id".to_owned(), json!(account_id)),
                    ("error".to_owned(), json!(err.message)),
                ]),
            )
            .await;
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

/// 规范化账号展示名称，避免账号 `name` 为空时产生 `" (email)"` 前导空格。
#[must_use]
pub fn format_account_name(account: &AccountFacts, fallback_id: &str) -> String {
    let name = account.name.trim();
    let email = account.email.as_deref().map(str::trim).unwrap_or("");
    match (name.is_empty(), email.is_empty()) {
        (false, false) if email != name => format!("{name} ({email})"),
        (false, _) => name.to_owned(),
        (true, false) => email.to_owned(),
        (true, true) => {
            let trimmed = fallback_id.trim();
            if trimmed.is_empty() {
                account.account_id.clone()
            } else {
                trimmed.to_owned()
            }
        }
    }
}

/// 从候选账号列表筛选目标账号 ID：若 `selected_ids` 非空则保留其中有效的 ID（去重保序），
/// 若 `selected_ids` 为空且 `fallback_to_enabled == true` 则回退至全部已启用账号。
#[must_use]
pub fn select_target_accounts(
    accounts: &[AccountFacts],
    selected_ids: &[String],
    fallback_to_enabled: bool,
) -> Vec<String> {
    let valid_ids: HashSet<&str> = accounts.iter().map(|a| a.account_id.as_str()).collect();
    if selected_ids.is_empty() {
        return if fallback_to_enabled {
            accounts
                .iter()
                .filter(|a| a.enabled)
                .map(|a| a.account_id.clone())
                .collect()
        } else {
            Vec::new()
        };
    }

    let mut target_ids = Vec::new();
    let mut seen = HashSet::new();
    for id in selected_ids {
        let trimmed = id.trim();
        if !trimmed.is_empty() && valid_ids.contains(trimmed) && seen.insert(trimmed.to_owned()) {
            target_ids.push(trimmed.to_owned());
        }
    }
    target_ids
}

#[must_use]
pub fn current_local_hhmm(timezone: &str) -> String {
    let tz: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::Asia::Shanghai);
    Utc::now().with_timezone(&tz).format("%H:%M").to_string()
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

fn random_millis(min_ms: i64, max_ms: i64) -> i64 {
    if max_ms <= min_ms {
        return min_ms.max(0);
    }
    let span = (max_ms - min_ms) as u128 + 1;
    let offset = (Uuid::new_v4().as_u128() % span) as i64;
    min_ms + offset
}
