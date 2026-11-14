//! 构造与 codex-proxy-rs 原生 Codex 指纹管线完全兼容的 OpenAI Responses 请求体

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Utc;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const CODEX_CONTEXT_JSON: &str = include_str!("../data/codex_context.json");

const CODEX_ENVIRONMENT_TEMPLATE: &str = "<environment_context>
  <cwd>/home/user/workspace</cwd>
  <shell>bash</shell>
  <current_date>{CURRENT_DATE}</current_date>
  <timezone>Etc/UTC</timezone>
  <filesystem><workspace_roots><root>/home/user/workspace</root></workspace_roots><permission_profile type=\"managed\"><file_system type=\"restricted\"><entry access=\"read\"><special>:root</special></entry><entry access=\"write\"><path>/home/user/workspace</path></entry><entry access=\"write\"><special>:slash_tmp</special></entry><entry access=\"write\"><special>:tmpdir</special></entry><entry access=\"read\"><path>/home/user/workspace/.git</path></entry><entry access=\"read\"><path>/home/user/workspace/.agents</path></entry><entry access=\"read\"><path>/home/user/workspace/.codex</path></entry><entry access=\"read\"><path>/home/user/workspace/.aws</path></entry></file_system></permission_profile></filesystem>
</environment_context>";

#[derive(Debug, Deserialize)]
struct CodexText {
    kind: String,
    text: String,
}

#[derive(Debug, Deserialize)]
struct CodexContextMessage {
    role: String,
    content: Vec<CodexText>,
}

#[derive(Debug, Deserialize)]
struct CodexContextAsset {
    tools: Value,
    base_instructions: String,
    messages: Vec<CodexContextMessage>,
}

struct ParsedCodexContext {
    tools: Value,
    compact_tools_bytes: Vec<u8>,
    base_instructions: String,
    messages: Vec<CodexContextMessage>,
}

static CODEX_CONTEXT: OnceLock<ParsedCodexContext> = OnceLock::new();

fn codex_context() -> &'static ParsedCodexContext {
    CODEX_CONTEXT.get_or_init(|| {
        let parsed: CodexContextAsset =
            serde_json::from_str(CODEX_CONTEXT_JSON).expect("valid codex_context.json");
        let compact_tools_bytes = serde_json::to_vec(&parsed.tools).expect("serialize codex tools");
        ParsedCodexContext {
            tools: parsed.tools,
            compact_tools_bytes,
            base_instructions: parsed.base_instructions,
            messages: parsed.messages,
        }
    })
}

/// 构造一次全新的 Codex thread 首轮 JSON 请求体，交给宿主 `host.model.execute` 转发到 Core。
pub fn build_codex_turn_body(
    account_id: &str,
    model: &str,
    effort: &str,
    instructions: &str,
    prompt: &str,
) -> Result<Vec<u8>, String> {
    let ctx = codex_context();
    let now = Utc::now();
    let thread_id = Uuid::now_v7();
    let thread_str = thread_id.to_string();
    let turn_id = Uuid::now_v7().to_string();
    let window_id = format!("{thread_str}:0");
    let context_window_id = Uuid::now_v7().to_string();
    let installation_id = deterministic_installation_uuid(account_id);
    let now_ms = now.timestamp_millis();
    let created_sec = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());

    let mut turn_metadata = json!({
        "installation_id": installation_id,
        "session_id": thread_str,
        "thread_id": thread_str,
        "agent_name": "/root",
        "turn_id": turn_id,
        "window_id": window_id,
        "window_number": 0,
        "context_window_id": context_window_id,
        "request_kind": "turn",
        "history_initialization": "new",
        "root_turn_id": turn_id,
        "thread_source": "user",
        "turn_trigger": "user",
        "sandbox": "seccomp",
        "sandbox_mode": "workspace-write",
        "auto_review_enabled": true,
        "node_repl_auto_review_required": true,
        "node_repl_disabled": false,
        "turn_started_at_unix_ms": now_ms,
        "analytics_enabled": true,
        "model": model,
    });
    if !effort.trim().is_empty()
        && let Some(obj) = turn_metadata.as_object_mut()
    {
        obj.insert(
            "reasoning_effort".to_owned(),
            Value::String(effort.trim().to_owned()),
        );
    }
    let turn_metadata_str = serde_json::to_string(&turn_metadata).map_err(|e| e.to_string())?;

    let tools_item_id = format!(
        "at_{}",
        deterministic_scoped_uuid(&thread_str, &ctx.compact_tools_bytes)
    );
    let base_item_id = format!(
        "msg_{}",
        deterministic_scoped_uuid(&thread_str, ctx.base_instructions.as_bytes())
    );

    let mut input = vec![
        json!({
            "type": "additional_tools",
            "id": tools_item_id,
            "role": "developer",
            "tools": ctx.tools,
        }),
        codex_message(
            &base_item_id,
            "developer",
            "",
            0.0,
            &[("model.base_instructions", &ctx.base_instructions)],
        ),
    ];

    for (idx, msg) in ctx.messages.iter().enumerate() {
        let mut parts: Vec<(&str, &str)> = Vec::new();
        if idx == 0 && !instructions.is_empty() {
            parts.push(("generic.developer_instructions", instructions));
        }
        for part in &msg.content {
            parts.push((part.kind.as_str(), part.text.as_str()));
        }
        input.push(codex_message(
            &format!("msg_{}", Uuid::now_v7()),
            &msg.role,
            &turn_id,
            created_sec,
            &parts,
        ));
    }

    let env_text =
        CODEX_ENVIRONMENT_TEMPLATE.replace("{CURRENT_DATE}", &now.format("%Y-%m-%d").to_string());
    input.push(codex_message(
        &format!("msg_{}", Uuid::now_v7()),
        "user",
        &turn_id,
        created_sec,
        &[("environments.environment_context", &env_text)],
    ));
    input.push(codex_message(
        &format!("msg_{}", Uuid::now_v7()),
        "user",
        &turn_id,
        created_sec,
        &[("user.text", prompt)],
    ));

    let mut reasoning = Map::new();
    if !effort.trim().is_empty() {
        reasoning.insert("effort".to_owned(), Value::String(effort.trim().to_owned()));
    }
    reasoning.insert("context".to_owned(), Value::String("all_turns".to_owned()));

    let mut body = Map::new();
    body.insert("model".to_owned(), Value::String(model.to_owned()));
    body.insert("stream".to_owned(), Value::Bool(true));
    body.insert("store".to_owned(), Value::Bool(false));
    body.insert("input".to_owned(), Value::Array(input));
    body.insert("tool_choice".to_owned(), Value::String("auto".to_owned()));
    body.insert("parallel_tool_calls".to_owned(), Value::Bool(true));
    body.insert("reasoning".to_owned(), Value::Object(reasoning));
    body.insert("include".to_owned(), json!(["reasoning.encrypted_content"]));
    body.insert(
        "prompt_cache_key".to_owned(),
        Value::String(thread_str.clone()),
    );
    body.insert("text".to_owned(), json!({ "verbosity": "low" }));
    body.insert(
        "client_metadata".to_owned(),
        json!({
            "thread_id": thread_str,
            "x-codex-window-id": window_id,
            "turn_id": turn_id,
            "root_turn_id": turn_id,
            "x-codex-turn-metadata": turn_metadata_str,
            "x-codex-installation-id": installation_id,
            "session_id": thread_str,
        }),
    );

    serde_json::to_vec(&body).map_err(|e| e.to_string())
}

fn codex_message(
    id: &str,
    role: &str,
    turn_id: &str,
    created: f64,
    parts: &[(&str, &str)],
) -> Value {
    let mut content = Vec::with_capacity(parts.len());
    let mut kinds = Vec::with_capacity(parts.len());
    for &(kind, text) in parts {
        content.push(json!({
            "type": "input_text",
            "text": text,
        }));
        kinds.push(Value::String(kind.to_owned()));
    }
    let mut metadata = Map::new();
    if !turn_id.is_empty() {
        metadata.insert("turn_id".to_owned(), Value::String(turn_id.to_owned()));
    }
    if created > 0.0 {
        metadata.insert("create_time".to_owned(), json!(created));
    }
    metadata.insert("content_item_kinds".to_owned(), Value::Array(kinds));
    json!({
        "type": "message",
        "id": id,
        "role": role,
        "content": content,
        "internal_chat_message_metadata_passthrough": metadata,
    })
}

fn deterministic_installation_uuid(account_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"codex-installation:");
    hasher.update(account_id.as_bytes());
    uuid_from_hash(hasher.finalize().into(), 4)
}

fn deterministic_scoped_uuid(scope: &str, data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(scope.as_bytes());
    hasher.update([0u8]);
    hasher.update(data);
    uuid_from_hash(hasher.finalize().into(), 5)
}

fn uuid_from_hash(digest: [u8; 32], version: u8) -> String {
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | (version << 4);
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}
