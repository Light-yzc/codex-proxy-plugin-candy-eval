use std::collections::HashSet;

use gateway_plugin_sdk::{PluginFault, call::host::StatePutRequest, client::HostClient};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::host_calls;
use crate::manifest::{STATE_NS_DETAIL, STATE_NS_QUEUE, STATE_NS_RECORDS, STATE_NS_SETTINGS};

const KEY_SETTINGS: &str = "default";
const KEY_QUEUE: &str = "state";
const KEY_RECORDS: &str = "index";
const MAX_RECORDS_KEEP: usize = 120;
const MAX_DETAIL_JSON_BYTES: usize = 230 * 1024;
const MAX_INDEX_JSON_BYTES: usize = 230 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalMode {
    Candy,
    Fingerprint,
    Pelican,
}

impl EvalMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Candy => "candy",
            Self::Fingerprint => "fingerprint",
            Self::Pelican => "pelican",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    Manual,
    Scheduled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PelicanVariantKind {
    Original,
    #[serde(rename = "all-in-one")]
    AllInOne,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueStatus {
    Idle,
    Running,
    Stopping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandyEvalSettings {
    pub mode: EvalMode,
    pub model: String,
    pub effort: String,
    pub runs: u32,
    pub fingerprint_concurrency: u32,
    pub pelican_variant: PelicanVariantKind,
    pub client_key_id: String,
    pub selected_account_ids: Vec<String>,
    pub schedule_enabled: bool,
    pub schedule_time: String,
    pub timezone: String,
    #[serde(default)]
    pub schedule_spread_minutes: u32,
    pub updated_at_ms: i64,
}

impl Default for CandyEvalSettings {
    fn default() -> Self {
        Self {
            mode: EvalMode::Candy,
            model: "gpt-5.4".to_owned(),
            effort: "high".to_owned(),
            runs: 3,
            fingerprint_concurrency: 1,
            pelican_variant: PelicanVariantKind::Original,
            client_key_id: String::new(),
            selected_account_ids: Vec::new(),
            schedule_enabled: false,
            schedule_time: "08:00".to_owned(),
            timezone: "Asia/Shanghai".to_owned(),
            schedule_spread_minutes: 60,
            updated_at_ms: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueueState {
    pub active_job_id: Option<String>,
    pub status: QueueStatus,
    pub stop_requested: bool,
    pub trigger_kind: TriggerKind,
    pub mode: EvalMode,
    pub model: String,
    pub effort: String,
    pub runs: u32,
    pub fingerprint_concurrency: u32,
    pub pelican_variant: PelicanVariantKind,
    pub client_key_id: String,
    pub pending_account_ids: Vec<String>,
    pub total: u32,
    pub completed: u32,
    pub started_at_ms: Option<i64>,
    pub updated_at_ms: i64,
    pub last_scheduled_date: String,
    #[serde(default)]
    pub spread_minutes: u32,
    #[serde(default)]
    pub next_run_at_ms: Option<i64>,
    #[serde(default)]
    pub window_end_at_ms: Option<i64>,
}

impl Default for QueueState {
    fn default() -> Self {
        Self {
            active_job_id: None,
            status: QueueStatus::Idle,
            stop_requested: false,
            trigger_kind: TriggerKind::Manual,
            mode: EvalMode::Candy,
            model: String::new(),
            effort: String::new(),
            runs: 3,
            fingerprint_concurrency: 1,
            pelican_variant: PelicanVariantKind::Original,
            client_key_id: String::new(),
            pending_account_ids: Vec::new(),
            total: 0,
            completed: 0,
            started_at_ms: None,
            updated_at_ms: 0,
            last_scheduled_date: String::new(),
            spread_minutes: 0,
            next_run_at_ms: None,
            window_end_at_ms: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordHeader {
    pub id: String,
    pub account_id: String,
    pub account_name: String,
    pub eval_type: EvalMode,
    pub trigger_kind: TriggerKind,
    pub model: String,
    pub status: String,
    pub effort: String,
    pub variant: String,
    pub summary: Value,
    pub duration_ms: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub error_message: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordsIndex {
    pub records: Vec<RecordHeader>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordDetail {
    pub id: String,
    pub detail: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScheduleExt {
    #[serde(default)]
    schedule_spread_minutes: u32,
    #[serde(default)]
    spread_minutes: u32,
    #[serde(default)]
    next_run_at_ms: Option<i64>,
    #[serde(default)]
    window_end_at_ms: Option<i64>,
}

#[cfg(not(test))]
fn schedule_ext_path() -> std::path::PathBuf {
    crate::lm_detector::cache_dir().join("schedule_ext.json")
}

fn load_schedule_ext() -> ScheduleExt {
    #[cfg(test)]
    {
        ScheduleExt::default()
    }
    #[cfg(not(test))]
    {
        std::fs::read_to_string(schedule_ext_path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }
}

fn save_schedule_ext(ext: &ScheduleExt) {
    #[cfg(not(test))]
    {
        let path = schedule_ext_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(ext) {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, &bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
    #[cfg(test)]
    let _ = ext;
}

pub async fn load_settings(
    host: &HostClient,
) -> Result<(CandyEvalSettings, Option<u64>), PluginFault> {
    let (mut settings, version): (CandyEvalSettings, Option<u64>) =
        load_or_default(host, STATE_NS_SETTINGS, KEY_SETTINGS).await?;
    let ext = load_schedule_ext();
    settings.schedule_spread_minutes = ext.schedule_spread_minutes.min(360);
    Ok((settings, version))
}

pub async fn save_settings(
    host: &HostClient,
    expected_version: Option<u64>,
    settings: &CandyEvalSettings,
) -> Result<u64, PluginFault> {
    let mut ext = load_schedule_ext();
    ext.schedule_spread_minutes = settings.schedule_spread_minutes.min(360);
    save_schedule_ext(&ext);

    let mut val = serde_json::to_value(settings).unwrap_or_else(|_| json!({}));
    if let Some(obj) = val.as_object_mut() {
        obj.remove("scheduleSpreadMinutes");
    }
    put_typed(
        host,
        STATE_NS_SETTINGS,
        KEY_SETTINGS,
        expected_version,
        &val,
    )
    .await
}

pub async fn load_queue(host: &HostClient) -> Result<(QueueState, Option<u64>), PluginFault> {
    let (mut queue, version): (QueueState, Option<u64>) =
        load_or_default(host, STATE_NS_QUEUE, KEY_QUEUE).await?;
    let ext = load_schedule_ext();
    queue.spread_minutes = ext.spread_minutes.min(360);
    queue.next_run_at_ms = ext.next_run_at_ms;
    queue.window_end_at_ms = ext.window_end_at_ms;
    Ok((queue, version))
}

pub async fn save_queue(
    host: &HostClient,
    expected_version: Option<u64>,
    queue: &QueueState,
) -> Result<u64, PluginFault> {
    let mut ext = load_schedule_ext();
    ext.spread_minutes = queue.spread_minutes.min(360);
    ext.next_run_at_ms = queue.next_run_at_ms;
    ext.window_end_at_ms = queue.window_end_at_ms;
    save_schedule_ext(&ext);

    let mut val = serde_json::to_value(queue).unwrap_or_else(|_| json!({}));
    if let Some(obj) = val.as_object_mut() {
        obj.remove("spreadMinutes");
        obj.remove("nextRunAtMs");
        obj.remove("windowEndAtMs");
    }
    put_typed(host, STATE_NS_QUEUE, KEY_QUEUE, expected_version, &val).await
}

pub async fn load_records_index(
    host: &HostClient,
) -> Result<(RecordsIndex, Option<u64>), PluginFault> {
    load_or_default(host, STATE_NS_RECORDS, KEY_RECORDS).await
}

pub async fn load_record_detail(
    host: &HostClient,
    id: &str,
) -> Result<Option<RecordDetail>, PluginFault> {
    let res = host_calls::get_state(host, STATE_NS_DETAIL, id).await?;
    match res.record {
        Some(rec) => Ok(serde_json::from_value(rec.value).ok()),
        None => Ok(None),
    }
}

pub async fn append_record(
    host: &HostClient,
    mut header: RecordHeader,
    detail: Value,
) -> Result<(), PluginFault> {
    header.error_message = truncate_chars(&header.error_message, 900);
    let fitted_detail = fit_detail_payload(&header.id, detail);
    let detail_record = RecordDetail {
        id: header.id.clone(),
        detail: fitted_detail,
    };
    let existing_detail = host_calls::get_state(host, STATE_NS_DETAIL, &header.id).await?;
    let detail_version = existing_detail.record.map(|r| r.version);
    put_typed(
        host,
        STATE_NS_DETAIL,
        &header.id,
        detail_version,
        &detail_record,
    )
    .await?;

    let (mut index, version) = load_records_index(host).await?;
    index.records.insert(0, header);
    let mut removed_ids = Vec::new();
    while index.records.len() > MAX_RECORDS_KEEP
        || serde_json::to_vec(&index).is_ok_and(|bytes| bytes.len() > MAX_INDEX_JSON_BYTES)
    {
        let Some(removed) = index.records.pop() else {
            break;
        };
        removed_ids.push(removed.id);
    }
    put_typed(host, STATE_NS_RECORDS, KEY_RECORDS, version, &index).await?;

    for id in removed_ids {
        delete_detail_best_effort(host, &id).await;
    }
    Ok(())
}

pub async fn delete_records(
    host: &HostClient,
    ids: Option<&[String]>,
) -> Result<usize, PluginFault> {
    let (mut index, version) = load_records_index(host).await?;
    let to_delete: Vec<String> = match ids {
        None => index.records.drain(..).map(|r| r.id).collect(),
        Some(target_ids) => {
            let set: HashSet<&str> = target_ids.iter().map(String::as_str).collect();
            let mut deleted = Vec::new();
            index.records.retain(|r| {
                if set.contains(r.id.as_str()) {
                    deleted.push(r.id.clone());
                    false
                } else {
                    true
                }
            });
            deleted
        }
    };
    let count = to_delete.len();
    if count > 0 || version.is_some() {
        put_typed(host, STATE_NS_RECORDS, KEY_RECORDS, version, &index).await?;
    }
    for id in to_delete {
        delete_detail_best_effort(host, &id).await;
    }
    Ok(count)
}

async fn delete_detail_best_effort(host: &HostClient, id: &str) {
    if let Ok(existing) = host_calls::get_state(host, STATE_NS_DETAIL, id).await
        && let Some(rec) = existing.record
    {
        let _ = host_calls::delete_state(host, STATE_NS_DETAIL, id, rec.version).await;
    }
}

fn fit_detail_payload(id: &str, detail: Value) -> Value {
    let probe = RecordDetail {
        id: id.to_owned(),
        detail: detail.clone(),
    };
    if serde_json::to_vec(&probe).is_ok_and(|b| b.len() <= MAX_DETAIL_JSON_BYTES) {
        return detail;
    }
    // 对超大详情优先裁剪 html/samples 长文本，避免触碰单值 256 KiB 硬上限
    if let Some(obj) = detail.as_object() {
        if obj.contains_key("html") {
            return json!({
                "variant": obj.get("variant").cloned().unwrap_or(Value::Null),
                "variant_label": obj.get("variant_label").cloned().unwrap_or(Value::Null),
                "attempts": obj.get("attempts").cloned().unwrap_or(Value::Null),
                "truncated": true,
                "html": "<!doctype html><p>生成内容超过插件单条状态容量上限（240 KB），已省略保存。</p>",
            });
        }
        if let Some(items) = obj.get("items").and_then(Value::as_array) {
            let slim_items: Vec<Value> = items
                .iter()
                .map(|item| {
                    let mut m = item.as_object().cloned().unwrap_or_default();
                    if let Some(Value::String(ans)) = m.get("answer") {
                        m.insert("answer".to_owned(), Value::String(truncate_chars(ans, 800)));
                    }
                    Value::Object(m)
                })
                .collect();
            let mut next = obj.clone();
            next.insert("items".to_owned(), Value::Array(slim_items));
            next.insert("truncated".to_owned(), Value::Bool(true));
            let val = Value::Object(next);
            let test = RecordDetail {
                id: id.to_owned(),
                detail: val.clone(),
            };
            if serde_json::to_vec(&test).is_ok_and(|b| b.len() <= MAX_DETAIL_JSON_BYTES) {
                return val;
            }
        }
    }
    json!({ "truncated": true })
}

async fn load_or_default<T>(
    host: &HostClient,
    namespace: &str,
    key: &str,
) -> Result<(T, Option<u64>), PluginFault>
where
    T: Default + DeserializeOwned,
{
    let res = host_calls::get_state(host, namespace, key).await?;
    match res.record {
        Some(record) => {
            let value = serde_json::from_value(record.value).unwrap_or_default();
            Ok((value, Some(record.version)))
        }
        None => Ok((T::default(), None)),
    }
}

async fn put_typed<T: Serialize>(
    host: &HostClient,
    namespace: &str,
    key: &str,
    expected_version: Option<u64>,
    value: &T,
) -> Result<u64, PluginFault> {
    let encoded = serde_json::to_value(value).map_err(|_| {
        PluginFault::new(
            gateway_plugin_sdk::ErrorCode::InvalidInput,
            "state encoding failed".to_owned(),
        )
    })?;
    let res = host_calls::put_state(
        host,
        &StatePutRequest {
            namespace: namespace.to_owned(),
            key: key.to_owned(),
            value: encoded,
            expected_version,
        },
    )
    .await?;
    Ok(res.version)
}

#[must_use]
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}…")
}
