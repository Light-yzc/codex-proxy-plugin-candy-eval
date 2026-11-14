use std::collections::BTreeMap;

use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        data::{AccountFacts, AccountFactsQuery},
        host::{
            KeyListRequest, KeyListResult, LogLevel, LogRequest, LogResult, MODEL_EXECUTE,
            MODEL_EXECUTE_STREAM, MODEL_STREAM_CLOSE, MODEL_STREAM_READ, ModelEventBatch,
            ModelExecuteRequest, ModelExecuteResult, ModelListRequest, ModelListResult,
            ModelOperation, ModelStreamCloseRequest, ModelStreamReadRequest, ModelStreamReadResult,
            ModelStreamResult, STATE_DELETE, STATE_GET, STATE_PUT, StateDeleteRequest,
            StateDeleteResult, StateGetRequest, StateGetResult, StatePutRequest, StatePutResult,
        },
        model::{CanonicalEvent, ExecutionEvent, WirePayload},
        resources::{GroupEnsureRequest, GroupMembersChange, KeyEnsureRequest, ManagedResource},
    },
    client::{HostClient, SessionError},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

const CLIENT_VERSION: &str = "codex-proxy-plugin-candy-eval/0.1.0";
const STREAM_CHUNK_BYTES: u32 = 1024 * 1024;
const PLUGIN_GROUP_RESOURCE_KEY: &str = "candy-eval-openai";
const PLUGIN_KEY_RESOURCE_KEY: &str = "candy-eval-runner";

#[derive(Debug, Default, Clone, Copy)]
pub struct EvalUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
}

impl EvalUsage {
    pub fn merge_max_or_add(&mut self, other: &Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(other.reasoning_tokens);
    }
}

#[derive(Debug, Clone)]
pub struct ModelCallOutcome {
    pub text: String,
    pub usage: EvalUsage,
}

#[derive(Debug, Clone)]
pub struct CancelFlag {
    tx: tokio::sync::watch::Sender<bool>,
    rx: tokio::sync::watch::Receiver<bool>,
}

impl Default for CancelFlag {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelFlag {
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::watch::channel(false);
        Self { tx, rx }
    }

    pub fn cancel(&self) {
        let _ = self.tx.send(true);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    pub async fn cancelled(&self) {
        let mut rx = self.rx.clone();
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }
}

pub async fn list_all_openai_accounts(host: &HostClient) -> Result<Vec<AccountFacts>, PluginFault> {
    let mut accounts = Vec::new();
    let mut cursor = None;
    for _ in 0..50 {
        let page = host
            .account_facts(AccountFactsQuery {
                provider_id: Some("openai".to_owned()),
                cursor: cursor.take(),
                limit: 100,
            })
            .await?;
        accounts.extend(page.accounts);
        match page.next_cursor {
            Some(next) if !next.is_empty() => cursor = Some(next),
            _ => break,
        }
    }
    Ok(accounts)
}

pub async fn list_all_client_keys(
    host: &HostClient,
) -> Result<Vec<gateway_plugin_sdk::call::host::ClientKey>, PluginFault> {
    let mut keys = Vec::new();
    let mut cursor = None;
    for _ in 0..50 {
        let page: KeyListResult = host
            .list_keys(KeyListRequest {
                cursor: cursor.take(),
                limit: 100,
            })
            .await?;
        keys.extend(page.keys);
        match page.next_cursor {
            Some(next) if !next.is_empty() => cursor = Some(next),
            _ => break,
        }
    }
    Ok(keys)
}

pub async fn list_models(
    host: &HostClient,
    client_key_id: &str,
) -> Result<Vec<String>, PluginFault> {
    let (result, _): (ModelListResult, _) = metadata(
        host,
        "host.models.list",
        &ModelListRequest {
            client_key_id: client_key_id.to_owned(),
            protocol: "openai".to_owned(),
            client_version: CLIENT_VERSION.to_owned(),
        },
    )
    .await?;
    Ok(result.models)
}

/// 确保存在插件内部专用的无限制分组与执行 Key，并同步所有已知 OpenAI 账号。
pub async fn ensure_runner_client_key(
    host: &HostClient,
    account_ids: &[String],
) -> Result<ManagedResource, PluginFault> {
    host.ensure_group(GroupEnsureRequest {
        resource_key: PLUGIN_GROUP_RESOURCE_KEY.to_owned(),
        name: "candy-eval-internal".to_owned(),
        color: "#0ea5e9".to_owned(),
        description: Some("Codex 降智检测插件内置执行分组".to_owned()),
    })
    .await?;

    if !account_ids.is_empty() {
        for chunk in account_ids.chunks(200) {
            host.change_group_members(GroupMembersChange {
                resource_key: PLUGIN_GROUP_RESOURCE_KEY.to_owned(),
                add: chunk.to_vec(),
                remove: Vec::new(),
            })
            .await?;
        }
    }

    host.ensure_key(KeyEnsureRequest {
        resource_key: PLUGIN_KEY_RESOURCE_KEY.to_owned(),
        name: "candy-eval-internal-key".to_owned(),
        group_resource_keys: vec![PLUGIN_GROUP_RESOURCE_KEY.to_owned()],
        max_concurrency: 0,
        requests_per_minute: 0,
        daily_limit_usd: "0".to_owned(),
        weekly_limit_usd: "0".to_owned(),
    })
    .await
}

pub async fn get_state(
    host: &HostClient,
    namespace: &str,
    key: &str,
) -> Result<StateGetResult, PluginFault> {
    metadata(
        host,
        STATE_GET,
        &StateGetRequest {
            namespace: namespace.to_owned(),
            key: key.to_owned(),
        },
    )
    .await
    .map(|(result, _)| result)
}

pub async fn put_state(
    host: &HostClient,
    request: &StatePutRequest,
) -> Result<StatePutResult, PluginFault> {
    metadata(host, STATE_PUT, request)
        .await
        .map(|(result, _)| result)
}

pub async fn delete_state(
    host: &HostClient,
    namespace: &str,
    key: &str,
    expected_version: u64,
) -> Result<StateDeleteResult, PluginFault> {
    metadata(
        host,
        STATE_DELETE,
        &StateDeleteRequest {
            namespace: namespace.to_owned(),
            key: key.to_owned(),
            expected_version,
        },
    )
    .await
    .map(|(result, _)| result)
}

pub async fn log_event(
    host: &HostClient,
    level: LogLevel,
    event: &str,
    fields: BTreeMap<String, Value>,
) {
    let _ = metadata::<_, LogResult>(
        host,
        "host.log",
        &LogRequest {
            event: event.to_owned(),
            level,
            fields,
        },
    )
    .await;
}

/// 通过宿主 `host.model.execute`（超限时自动切换 `host.model.execute_stream`）执行一次 OpenAI 请求。
pub async fn execute_openai_generate(
    host: &HostClient,
    cancel: &CancelFlag,
    client_key_id: &str,
    account_id: &str,
    model: &str,
    body_bytes: Vec<u8>,
) -> Result<ModelCallOutcome, PluginFault> {
    let req = ModelExecuteRequest {
        client_key_id: Some(client_key_id.to_owned()),
        model: model.to_owned(),
        protocol: "openai".to_owned(),
        operation: ModelOperation::Generate,
        provider: Some("openai".to_owned()),
        account_id: Some(account_id.to_owned()),
        previous_response_id: None,
    };
    let params = serde_json::to_value(&req).map_err(|_| invalid_fault())?;

    let unary = tokio::select! {
        _ = cancel.cancelled() => {
            return Err(PluginFault::new(ErrorCode::Cancelled, "已取消检测"));
        }
        res = host.call(MODEL_EXECUTE, params.clone(), body_bytes.clone()) => res.map_err(SessionError::into_plugin_fault),
    };

    match unary {
        Ok(reply) => {
            let _: ModelExecuteResult =
                serde_json::from_value(reply.result).map_err(|_| invalid_fault())?;
            let batch = ModelEventBatch::decode(&reply.payload).map_err(|_| invalid_fault())?;
            Ok(extract_outcome(&batch.events))
        }
        Err(fault)
            if fault.code == ErrorCode::Capacity
                && fault.message.contains("requires streaming") =>
        {
            execute_stream(host, cancel, params, body_bytes).await
        }
        Err(fault) => Err(fault),
    }
}

async fn execute_stream(
    host: &HostClient,
    cancel: &CancelFlag,
    params: Value,
    body_bytes: Vec<u8>,
) -> Result<ModelCallOutcome, PluginFault> {
    let reply = tokio::select! {
        _ = cancel.cancelled() => {
            return Err(PluginFault::new(ErrorCode::Cancelled, "已取消检测"));
        }
        res = host.call(MODEL_EXECUTE_STREAM, params, body_bytes) => res.map_err(SessionError::into_plugin_fault)?,
    };
    let started: ModelStreamResult =
        serde_json::from_value(reply.result).map_err(|_| invalid_fault())?;
    let mut events = Vec::new();
    loop {
        if cancel.is_cancelled() {
            let _ = close_stream(host, &started.stream).await;
            return Err(PluginFault::new(ErrorCode::Cancelled, "已取消检测"));
        }
        let read_params = serde_json::to_value(&ModelStreamReadRequest {
            stream: started.stream.clone(),
            maximum_bytes: STREAM_CHUNK_BYTES,
        })
        .map_err(|_| invalid_fault())?;
        let read_res = tokio::select! {
            _ = cancel.cancelled() => {
                let _ = close_stream(host, &started.stream).await;
                return Err(PluginFault::new(ErrorCode::Cancelled, "已取消检测"));
            }
            res = host.call(MODEL_STREAM_READ, read_params, Vec::new()) => res.map_err(SessionError::into_plugin_fault),
        };
        let read_reply = match read_res {
            Ok(r) => r,
            Err(err) => {
                let _ = close_stream(host, &started.stream).await;
                return Err(err);
            }
        };
        let chunk: ModelStreamReadResult =
            serde_json::from_value(read_reply.result).map_err(|_| invalid_fault())?;
        if !read_reply.payload.is_empty() {
            let batch =
                ModelEventBatch::decode(&read_reply.payload).map_err(|_| invalid_fault())?;
            events.extend(batch.events);
        }
        if chunk.end {
            break;
        }
    }
    Ok(extract_outcome(&events))
}

async fn close_stream(host: &HostClient, stream: &str) -> Result<(), PluginFault> {
    let params = serde_json::to_value(&ModelStreamCloseRequest {
        stream: stream.to_owned(),
    })
    .map_err(|_| invalid_fault())?;
    let _ = host
        .call(MODEL_STREAM_CLOSE, params, Vec::new())
        .await
        .map_err(SessionError::into_plugin_fault)?;
    Ok(())
}

fn extract_outcome(events: &[ExecutionEvent]) -> ModelCallOutcome {
    let mut text = String::new();
    let mut wire_fallback = String::new();
    let mut usage = EvalUsage::default();

    for ev in events {
        for fact in &ev.facts {
            match fact {
                CanonicalEvent::TextDelta { text: delta, .. } => {
                    text.push_str(delta);
                }
                CanonicalEvent::Usage { usage: u } => {
                    if let Some(v) = u.input_tokens {
                        usage.input_tokens = usage.input_tokens.max(v);
                    }
                    if let Some(v) = u.output_tokens {
                        usage.output_tokens = usage.output_tokens.max(v);
                    }
                    if let Some(v) = u.reasoning_tokens {
                        usage.reasoning_tokens = usage.reasoning_tokens.max(v);
                    }
                }
                _ => {}
            }
        }
        if let Some(wire) = &ev.wire
            && let WirePayload::Json { event, data, .. } = &wire.payload
        {
            let ev_type = event
                .as_deref()
                .or_else(|| data.get("type").and_then(Value::as_str))
                .unwrap_or("");
            if ev_type == "response.output_text.delta"
                && let Some(delta) = data.get("delta").and_then(Value::as_str)
            {
                wire_fallback.push_str(delta);
            }
            if let Some(u) = data.get("response").and_then(|r| r.get("usage")) {
                if let Some(v) = u.get("input_tokens").and_then(Value::as_u64) {
                    usage.input_tokens = usage.input_tokens.max(v);
                }
                if let Some(v) = u.get("output_tokens").and_then(Value::as_u64) {
                    usage.output_tokens = usage.output_tokens.max(v);
                }
                if let Some(v) = u
                    .get("output_tokens_details")
                    .and_then(|d| d.get("reasoning_tokens"))
                    .and_then(Value::as_u64)
                {
                    usage.reasoning_tokens = usage.reasoning_tokens.max(v);
                }
            }
        }
    }

    if text.is_empty() && !wire_fallback.is_empty() {
        text = wire_fallback;
    }

    ModelCallOutcome { text, usage }
}

async fn metadata<I, O>(
    host: &HostClient,
    method: &str,
    input: &I,
) -> Result<(O, Vec<u8>), PluginFault>
where
    I: Serialize,
    O: DeserializeOwned,
{
    let params = serde_json::to_value(input).map_err(|_| invalid_fault())?;
    let reply = host
        .call(method, params, Vec::new())
        .await
        .map_err(SessionError::into_plugin_fault)?;
    let output = serde_json::from_value(reply.result).map_err(|_| invalid_fault())?;
    Ok((output, reply.payload))
}

fn invalid_fault() -> PluginFault {
    PluginFault::new(
        ErrorCode::InvalidInput,
        "plugin callback encoding failed".to_owned(),
    )
}
