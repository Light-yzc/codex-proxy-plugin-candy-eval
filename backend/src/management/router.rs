use std::collections::HashSet;

use chrono::Utc;
use gateway_plugin_sdk::PluginFault;
use gateway_plugin_sdk::call::management::{ManagementRequest, ManagementResponse};
use gateway_plugin_sdk::client::{TypedCall, TypedReply};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::coordinator::{EvalCoordinator, LiveEvalProgress};
use crate::eval_runner::{CANDY_MAX_RUNS, PELICAN_VARIANTS};
use crate::host_calls;
use crate::lm_detector::detector_revision;
use crate::state_store::{
    CandyEvalSettings, EvalMode, PelicanVariantKind, QueueState, load_queue, load_record_detail,
    load_records_index, load_settings, save_settings,
};

use super::response::{ApiError, ApiResult, json_reply};

const MAXIMUM_BODY_BYTES: usize = 256 * 1024;

pub async fn handle(
    coordinator: &EvalCoordinator,
    call: TypedCall<ManagementRequest>,
) -> Result<TypedReply<ManagementResponse>, PluginFault> {
    route(coordinator, call).await.or_else(ApiError::into_reply)
}

async fn route(coordinator: &EvalCoordinator, call: TypedCall<ManagementRequest>) -> ApiResult {
    if !call.request.query.is_empty() {
        return Err(ApiError::invalid("此接口不支持查询参数"));
    }
    if call.payload.len() > MAXIMUM_BODY_BYTES {
        return Err(ApiError::invalid("请求体超过限制"));
    }
    match (call.request.method.as_str(), call.request.path.as_str()) {
        ("GET", "api/status") => status(coordinator, call).await,
        ("GET", "api/records") => records(call).await,
        ("POST", "api/record-detail") => record_detail(call).await,
        ("POST", "api/settings") => update_settings(call).await,
        ("POST", "api/run") => start_run(coordinator, call).await,
        ("POST", "api/step") => step_run(coordinator, call).await,
        ("POST", "api/stop") => stop_run(coordinator, call).await,
        ("POST", "api/records/delete") => delete_records(call).await,
        _ => Err(ApiError::new(404, "not_found", "未找到插件管理接口")),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountView {
    account_id: String,
    name: String,
    email: Option<String>,
    enabled: bool,
    group_ids: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusResponse {
    detector_revision: String,
    accounts: Vec<AccountView>,
    keys: Vec<gateway_plugin_sdk::call::host::ClientKey>,
    models: Vec<String>,
    settings: CandyEvalSettings,
    queue: QueueState,
    live: LiveEvalProgress,
    pelican_variants: Vec<Value>,
}

async fn status(coordinator: &EvalCoordinator, call: TypedCall<ManagementRequest>) -> ApiResult {
    require_empty_body(&call)?;
    let (settings, _) = load_settings(&call.host).await?;
    let (queue, _) = load_queue(&call.host).await?;
    let accounts = host_calls::list_all_openai_accounts(&call.host).await?;
    let keys = host_calls::list_all_client_keys(&call.host).await?;

    let preferred_key = if !settings.client_key_id.trim().is_empty() {
        Some(settings.client_key_id.as_str())
    } else {
        keys.iter().find(|k| k.enabled).map(|k| k.id.as_str())
    };
    let mut models = match preferred_key {
        Some(kid) => host_calls::list_models(&call.host, kid)
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    if models.is_empty() {
        models = default_models();
    }

    let accounts = accounts
        .into_iter()
        .map(|a| AccountView {
            account_id: a.account_id,
            name: a.name,
            email: a.email,
            enabled: a.enabled,
            group_ids: a.group_ids,
        })
        .collect();

    let pelican_variants = PELICAN_VARIANTS
        .iter()
        .map(|v| json!({ "id": v.id, "label": v.label }))
        .collect();

    json_reply(&StatusResponse {
        detector_revision: detector_revision().to_owned(),
        accounts,
        keys,
        models,
        settings,
        queue,
        live: coordinator.snapshot(),
        pelican_variants,
    })
}

async fn records(call: TypedCall<ManagementRequest>) -> ApiResult {
    require_empty_body(&call)?;
    let (index, _) = load_records_index(&call.host).await?;
    json_reply(&json!({ "records": index.records }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecordDetailRequest {
    id: String,
}

async fn record_detail(call: TypedCall<ManagementRequest>) -> ApiResult {
    let req: RecordDetailRequest = decode_json(&call)?;
    if req.id.trim().is_empty() || req.id.len() > 128 {
        return Err(ApiError::invalid("记录 ID 无效"));
    }
    let detail = load_record_detail(&call.host, req.id.trim())
        .await?
        .ok_or_else(|| ApiError::new(404, "not_found", "未找到该记录详情"))?;
    json_reply(&detail)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettingsPatchRequest {
    mode: Option<EvalMode>,
    model: Option<String>,
    effort: Option<String>,
    runs: Option<u32>,
    fingerprint_concurrency: Option<u32>,
    pelican_variant: Option<PelicanVariantKind>,
    client_key_id: Option<String>,
    selected_account_ids: Option<Vec<String>>,
    schedule_enabled: Option<bool>,
    schedule_time: Option<String>,
    timezone: Option<String>,
}

async fn update_settings(call: TypedCall<ManagementRequest>) -> ApiResult {
    let req: SettingsPatchRequest = decode_json(&call)?;
    let (mut settings, version) = load_settings(&call.host).await?;
    apply_settings_patch(&mut settings, req)?;
    settings.updated_at_ms = Utc::now().timestamp_millis();
    save_settings(&call.host, version, &settings).await?;
    json_reply(&json!({ "settings": settings }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RunRequest {
    mode: Option<EvalMode>,
    model: Option<String>,
    effort: Option<String>,
    runs: Option<u32>,
    fingerprint_concurrency: Option<u32>,
    pelican_variant: Option<PelicanVariantKind>,
    client_key_id: Option<String>,
    account_ids: Option<Vec<String>>,
}

async fn start_run(coordinator: &EvalCoordinator, call: TypedCall<ManagementRequest>) -> ApiResult {
    let req: RunRequest = decode_json(&call)?;
    let (mut settings, version) = load_settings(&call.host).await?;
    apply_settings_patch(
        &mut settings,
        SettingsPatchRequest {
            mode: req.mode,
            model: req.model,
            effort: req.effort,
            runs: req.runs,
            fingerprint_concurrency: req.fingerprint_concurrency,
            pelican_variant: req.pelican_variant,
            client_key_id: req.client_key_id,
            selected_account_ids: None,
            schedule_enabled: None,
            schedule_time: None,
            timezone: None,
        },
    )?;

    let all_accounts = host_calls::list_all_openai_accounts(&call.host).await?;
    let valid_ids: HashSet<&str> = all_accounts.iter().map(|a| a.account_id.as_str()).collect();

    let requested_ids = req
        .account_ids
        .unwrap_or_else(|| settings.selected_account_ids.clone());
    let mut target_ids = Vec::new();
    let mut seen = HashSet::new();
    for id in requested_ids {
        let trimmed = id.trim();
        if !trimmed.is_empty() && valid_ids.contains(trimmed) && seen.insert(trimmed.to_owned()) {
            target_ids.push(trimmed.to_owned());
        }
    }
    if target_ids.is_empty() {
        return Err(ApiError::invalid("请至少选择一个有效的 OpenAI 账号"));
    }

    settings.selected_account_ids = target_ids.clone();
    settings.updated_at_ms = Utc::now().timestamp_millis();
    save_settings(&call.host, version, &settings).await?;

    let queue = coordinator
        .start_manual_job(&call.host, settings, target_ids)
        .await?;
    json_reply(&json!({ "started": true, "queue": queue }))
}

async fn step_run(coordinator: &EvalCoordinator, call: TypedCall<ManagementRequest>) -> ApiResult {
    require_empty_json(&call)?;
    let outcome = coordinator.try_step_one(&call.host).await?;
    json_reply(&outcome)
}

async fn stop_run(coordinator: &EvalCoordinator, call: TypedCall<ManagementRequest>) -> ApiResult {
    require_empty_json(&call)?;
    let queue = coordinator.stop_current_job(&call.host).await?;
    json_reply(&json!({ "stopped": true, "queue": queue }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeleteRecordsRequest {
    ids: Option<Vec<String>>,
}

async fn delete_records(call: TypedCall<ManagementRequest>) -> ApiResult {
    let req: DeleteRecordsRequest = decode_json(&call)?;
    let deleted = crate::state_store::delete_records(&call.host, req.ids.as_deref()).await?;
    json_reply(&json!({ "deleted": deleted }))
}

fn apply_settings_patch(
    settings: &mut CandyEvalSettings,
    req: SettingsPatchRequest,
) -> Result<(), ApiError> {
    if let Some(mode) = req.mode {
        settings.mode = mode;
    }
    if let Some(model) = req.model {
        let trimmed = model.trim();
        if trimmed.is_empty() || trimmed.len() > 128 {
            return Err(ApiError::invalid("模型名称无效"));
        }
        settings.model = trimmed.to_owned();
    }
    if let Some(effort) = req.effort {
        let trimmed = effort.trim();
        if trimmed.len() > 32 {
            return Err(ApiError::invalid("推理强度参数过长"));
        }
        settings.effort = trimmed.to_owned();
    }
    if let Some(runs) = req.runs {
        if !(1..=CANDY_MAX_RUNS).contains(&runs) {
            return Err(ApiError::invalid("测试次数须在 1 到 10 之间"));
        }
        settings.runs = runs;
    }
    if let Some(conc) = req.fingerprint_concurrency {
        if !(1..=3).contains(&conc) {
            return Err(ApiError::invalid("指纹并发须在 1 到 3 之间"));
        }
        settings.fingerprint_concurrency = conc;
    }
    if let Some(variant) = req.pelican_variant {
        settings.pelican_variant = variant;
    }
    if let Some(key_id) = req.client_key_id {
        let trimmed = key_id.trim();
        if trimmed.len() > 128 {
            return Err(ApiError::invalid("Key ID 无效"));
        }
        settings.client_key_id = trimmed.to_owned();
    }
    if let Some(ids) = req.selected_account_ids {
        if ids.len() > 200 {
            return Err(ApiError::invalid("最多选择 200 个账号"));
        }
        settings.selected_account_ids = ids
            .into_iter()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty() && id.len() <= 128)
            .collect();
    }
    if let Some(enabled) = req.schedule_enabled {
        settings.schedule_enabled = enabled;
    }
    if let Some(time) = req.schedule_time {
        if !valid_hhmm(&time) {
            return Err(ApiError::invalid("每日执行时间必须为 HH:MM（如 08:00）"));
        }
        settings.schedule_time = time;
    }
    if let Some(tz) = req.timezone {
        if tz.parse::<chrono_tz::Tz>().is_err() {
            return Err(ApiError::invalid("时区无效（如 Asia/Shanghai）"));
        }
        settings.timezone = tz;
    }
    Ok(())
}

fn valid_hhmm(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 5 || bytes[2] != b':' {
        return false;
    }
    let Ok(hour) = value[..2].parse::<u32>() else {
        return false;
    };
    let Ok(minute) = value[3..].parse::<u32>() else {
        return false;
    };
    hour < 24 && minute < 60
}

fn require_empty_body(call: &TypedCall<ManagementRequest>) -> Result<(), ApiError> {
    if call.payload.is_empty() {
        Ok(())
    } else {
        Err(ApiError::invalid("此接口不接受请求体"))
    }
}

fn require_empty_json(call: &TypedCall<ManagementRequest>) -> Result<(), ApiError> {
    if call.payload.is_empty() {
        return Ok(());
    }
    let value: Value = decode_json(call)?;
    if value.as_object().is_some_and(serde_json::Map::is_empty) {
        Ok(())
    } else {
        Err(ApiError::invalid("请求体须为空对象"))
    }
}

fn decode_json<T: for<'de> Deserialize<'de>>(
    call: &TypedCall<ManagementRequest>,
) -> Result<T, ApiError> {
    if call.request.content_type.as_deref() != Some("application/json") {
        // 允许缺省或带 charset 的 application/json
        if let Some(ct) = &call.request.content_type
            && ct.split(';').next().map(str::trim) != Some("application/json")
        {
            return Err(ApiError::invalid("请求 Content-Type 须为 application/json"));
        }
    }
    serde_json::from_slice(&call.payload).map_err(|_| ApiError::invalid("请求 JSON 格式无效"))
}

fn default_models() -> Vec<String> {
    vec![
        "gpt-5.4".to_owned(),
        "gpt-5.3-codex".to_owned(),
        "gpt-5.2-codex".to_owned(),
        "gpt-5.2".to_owned(),
        "gpt-5.1-codex".to_owned(),
    ]
}
