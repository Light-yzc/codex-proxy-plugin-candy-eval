//! 模型数字指纹归因（移植自 Ikaleio/lm-detector MIT 实现，见 data/candy_eval/lm-detector/LICENSE）

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock, RwLock};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const DETECTOR_BIN: &[u8] = include_bytes!("../data/lm-detector/shared_detector.bin");
const MODELS_JSON: &str = include_str!("../data/lm-detector/models.json");
const ENROLLMENT_SUITE_EMBEDDED: &str = include_str!("../data/lm-detector/enrollment-suite.json");
const LICENSE_TEXT: &str = include_str!("../data/lm-detector/LICENSE");
const UPSTREAM_COMMITS_URL: &str = "https://api.github.com/repos/Ikaleio/lm-detector/commits/main";
const UPSTREAM_RAW_BASE: &str = "https://raw.githubusercontent.com/Ikaleio/lm-detector";
const AUTO_SYNC_INTERVAL_MS: i64 = 6 * 3600 * 1000;

pub const FINGERPRINT_TARGET: usize = 3;
pub const FINGERPRINT_MAX_TEXT: usize = 64_000;

#[derive(Debug, Clone, Deserialize)]
pub struct DetectorModelMeta {
    pub id: String,
    pub display_name: String,
    pub family: String,
    pub family_name: String,
    pub response_count: usize,
}

#[derive(Debug, Clone)]
struct Scaling {
    mean: Vec<f64>,
    scale: Vec<f64>,
}

#[derive(Debug, Clone)]
struct FeatureBank {
    mean: Vec<f64>,
    scale: Vec<f64>,
    basis: Vec<Vec<f64>>,
    centroids: Vec<Vec<f64>>,
    environments: Vec<Vec<Vec<f64>>>,
}

#[derive(Debug, Clone)]
struct Detector {
    revision: String,
    built_at: String,
    tau: f64,
    models: Vec<DetectorModelMeta>,
    head_params: Vec<Scaling>,
    full_params: Vec<Scaling>,
    lda_weights: Vec<Vec<f64>>,
    lda_bias: Vec<f64>,
    references: Vec<Vec<Vec<f64>>>,
    reference_norms: Vec<Vec<f64>>,
    hellinger: FeatureBank,
    ordered: FeatureBank,
    blend_frequency: f64,
    blend_positional: f64,
    positional_params: Vec<Scaling>,
    positional_weights: Vec<Vec<f64>>,
    positional_bias: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FingerprintOutputInput {
    pub text: String,
    pub expected_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FingerprintDiagnostic {
    pub index: usize,
    pub parsed_numbers: usize,
    pub minimum_numbers: usize,
    pub accepted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FingerprintSequence {
    pub autocorrelation: f64,
    pub small_step_share: f64,
    pub model_like: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FingerprintCandidate {
    pub model: String,
    pub display_name: String,
    pub family: String,
    pub family_name: String,
    pub probability: Option<f64>,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FingerprintAttribution {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub decision: String,
    pub prediction: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prediction_name: String,
    pub probability: Option<f64>,
    pub used_outputs: usize,
    pub family_prediction_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<FingerprintSequence>,
    pub results: Vec<FingerprintCandidate>,
    pub diagnostics: Vec<FingerprintDiagnostic>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectorInfo {
    pub revision: String,
    pub built_at: String,
    pub model_count: usize,
    pub source: String,
    pub commit_sha: Option<String>,
    pub updated_at_ms: Option<i64>,
    pub last_checked_at_ms: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedMeta {
    commit_sha: String,
    built_at: String,
    updated_at_ms: i64,
}

#[derive(Debug, Clone)]
struct DetectorRuntime {
    detector: Arc<Detector>,
    enrollment_suite_json: Arc<String>,
    source: String,
    commit_sha: Option<String>,
    updated_at_ms: Option<i64>,
    last_checked_at_ms: Option<i64>,
    last_error: Option<String>,
}

static RUNTIME_STATE: OnceLock<RwLock<DetectorRuntime>> = OnceLock::new();
static SYNC_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub(crate) fn cache_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CANDY_EVAL_CACHE_DIR")
        && !dir.trim().is_empty()
    {
        return PathBuf::from(dir);
    }
    let runtime_root = PathBuf::from("/app/.runtime/data");
    if runtime_root.is_dir() {
        return runtime_root.join("candy-eval-lm-detector");
    }
    std::env::temp_dir().join("candy-eval-lm-detector")
}

fn init_runtime() -> DetectorRuntime {
    #[cfg(not(test))]
    {
        let dir = cache_dir();
        let bin_path = dir.join("shared_detector.bin");
        let models_path = dir.join("models.json");
        let suite_path = dir.join("enrollment-suite.json");
        let meta_path = dir.join("meta.json");
        if let (Ok(bin), Ok(models), Ok(suite)) = (
            std::fs::read(&bin_path),
            std::fs::read_to_string(&models_path),
            std::fs::read_to_string(&suite_path),
        ) && serde_json::from_str::<Value>(&suite).is_ok()
            && let Ok(det) = try_decode_detector(&bin, &models)
        {
            let meta: CachedMeta = std::fs::read_to_string(&meta_path)
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
            return DetectorRuntime {
                detector: Arc::new(det),
                enrollment_suite_json: Arc::new(suite),
                source: "remote".to_owned(),
                commit_sha: (!meta.commit_sha.is_empty()).then_some(meta.commit_sha),
                updated_at_ms: (meta.updated_at_ms > 0).then_some(meta.updated_at_ms),
                last_checked_at_ms: None,
                last_error: None,
            };
        }
    }
    let det = try_decode_detector(DETECTOR_BIN, MODELS_JSON).expect("valid embedded lm-detector");
    DetectorRuntime {
        detector: Arc::new(det),
        enrollment_suite_json: Arc::new(ENROLLMENT_SUITE_EMBEDDED.to_owned()),
        source: "embedded".to_owned(),
        commit_sha: None,
        updated_at_ms: None,
        last_checked_at_ms: None,
        last_error: None,
    }
}

fn runtime_lock() -> &'static RwLock<DetectorRuntime> {
    RUNTIME_STATE.get_or_init(|| RwLock::new(init_runtime()))
}

fn detector() -> Arc<Detector> {
    let guard = runtime_lock()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(&guard.detector)
}

#[must_use]
pub fn current_enrollment_suite_json() -> Arc<String> {
    let guard = runtime_lock()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(&guard.enrollment_suite_json)
}

#[must_use]
pub fn detector_revision() -> String {
    detector().revision.clone()
}

#[must_use]
pub fn detector_info() -> DetectorInfo {
    let guard = runtime_lock()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    DetectorInfo {
        revision: guard.detector.revision.clone(),
        built_at: guard.detector.built_at.clone(),
        model_count: guard.detector.models.len(),
        source: guard.source.clone(),
        commit_sha: guard.commit_sha.clone(),
        updated_at_ms: guard.updated_at_ms,
        last_checked_at_ms: guard.last_checked_at_ms,
        last_error: guard.last_error.clone(),
    }
}

/// 若距上次检查已超过 6 小时（或启动后尚未检查），则自动从 `Ikaleio/lm-detector` 拉取最新指纹库。
pub async fn maybe_auto_sync() -> Result<DetectorInfo, String> {
    sync_detector_inner(false).await
}

/// 立即从 `Ikaleio/lm-detector` 检查并拉取最新指纹库。
pub async fn force_sync_detector() -> Result<DetectorInfo, String> {
    sync_detector_inner(true).await
}

async fn sync_detector_inner(force: bool) -> Result<DetectorInfo, String> {
    let lock = SYNC_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _guard = lock.lock().await;

    let now_ms = Utc::now().timestamp_millis();
    let (last_checked, current_sha, current_source) = {
        let rt = runtime_lock()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            rt.last_checked_at_ms,
            rt.commit_sha.clone(),
            rt.source.clone(),
        )
    };
    if !force
        && let Some(ts) = last_checked
        && now_ms.saturating_sub(ts) < AUTO_SYNC_INTERVAL_MS
    {
        return Ok(detector_info());
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let res = fetch_and_validate_upstream(force, current_sha.as_deref(), &current_source);
        let _ = tx.send(res);
    });

    let outcome = rx.await.map_err(|_| "指纹库同步线程异常终止".to_owned())?;

    let checked_ms = Utc::now().timestamp_millis();
    match outcome {
        Ok(Some((det, suite_json, sha))) => {
            let mut rt = runtime_lock()
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            rt.detector = Arc::new(det);
            rt.enrollment_suite_json = Arc::new(suite_json);
            rt.source = "remote".to_owned();
            rt.commit_sha = Some(sha);
            rt.updated_at_ms = Some(checked_ms);
            rt.last_checked_at_ms = Some(checked_ms);
            rt.last_error = None;
            drop(rt);
            Ok(detector_info())
        }
        Ok(None) => {
            let mut rt = runtime_lock()
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            rt.last_checked_at_ms = Some(checked_ms);
            rt.last_error = None;
            drop(rt);
            Ok(detector_info())
        }
        Err(err) => {
            let mut rt = runtime_lock()
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            rt.last_checked_at_ms = Some(checked_ms);
            rt.last_error = Some(err.clone());
            drop(rt);
            Err(err)
        }
    }
}

fn curl_get(url: &str, timeout_secs: u32) -> Result<Vec<u8>, String> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            &timeout_secs.to_string(),
            "-H",
            "User-Agent: codex-proxy-plugin-candy-eval",
            url,
        ])
        .output()
        .map_err(|e| format!("无法调用 curl：{e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("拉取 {url} 失败：{}", stderr.trim()));
    }
    Ok(output.stdout)
}

fn fetch_and_validate_upstream(
    force: bool,
    current_sha: Option<&str>,
    current_source: &str,
) -> Result<Option<(Detector, String, String)>, String> {
    let commit_bytes = curl_get(UPSTREAM_COMMITS_URL, 20)?;
    let commit_val: Value =
        serde_json::from_slice(&commit_bytes).map_err(|e| format!("解析提交信息失败：{e}"))?;
    let sha = commit_val["sha"]
        .as_str()
        .filter(|s| s.len() >= 7 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| "上游提交 SHA 无效".to_owned())?
        .to_owned();

    if !force && current_source == "remote" && current_sha == Some(sha.as_str()) {
        return Ok(None);
    }

    let bin_url = format!("{UPSTREAM_RAW_BASE}/{sha}/data/shared_detector.bin");
    let suite_url = format!("{UPSTREAM_RAW_BASE}/{sha}/data/enrollment-suite.json");
    let bank_url = format!("{UPSTREAM_RAW_BASE}/{sha}/data/unified_bank.json");

    let bin_bytes = curl_get(&bin_url, 60)?;
    let suite_bytes = curl_get(&suite_url, 30)?;
    let bank_bytes = curl_get(&bank_url, 60)?;

    let suite_json = String::from_utf8(suite_bytes)
        .map_err(|e| format!("enrollment-suite.json 非 UTF-8：{e}"))?;
    let suite_val: Value = serde_json::from_str(&suite_json)
        .map_err(|e| format!("enrollment-suite.json 解析失败：{e}"))?;
    if suite_val.as_array().is_none_or(Vec::is_empty) {
        return Err("enrollment-suite.json 为空".to_owned());
    }

    let bank_val: Value = serde_json::from_slice(&bank_bytes)
        .map_err(|e| format!("unified_bank.json 解析失败：{e}"))?;
    let built_at = bank_val["built_at"]
        .as_str()
        .ok_or_else(|| "unified_bank.json 缺少 built_at".to_owned())?;
    let raw_models = bank_val["models"]
        .as_array()
        .ok_or_else(|| "unified_bank.json 缺少 models".to_owned())?;
    let compact_models: Vec<Value> = raw_models
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m["id"],
                "display_name": m["display_name"],
                "family": m["family"],
                "family_name": m["family_name"],
                "response_count": m["response_count"],
            })
        })
        .collect();
    let models_json = serde_json::to_string_pretty(&serde_json::json!({
        "built_at": built_at,
        "models": compact_models,
    }))
    .map_err(|e| format!("序列化 models.json 失败：{e}"))?;

    let det = try_decode_detector(&bin_bytes, &models_json)?;

    let dir = cache_dir();
    if std::fs::create_dir_all(&dir).is_ok() {
        let now_ms = Utc::now().timestamp_millis();
        let meta = CachedMeta {
            commit_sha: sha.clone(),
            built_at: det.built_at.clone(),
            updated_at_ms: now_ms,
        };
        let _ = write_atomic(&dir.join("shared_detector.bin"), &bin_bytes);
        let _ = write_atomic(&dir.join("models.json"), models_json.as_bytes());
        let _ = write_atomic(&dir.join("enrollment-suite.json"), suite_json.as_bytes());
        if let Ok(meta_raw) = serde_json::to_vec_pretty(&meta) {
            let _ = write_atomic(&dir.join("meta.json"), &meta_raw);
        }
    }

    Ok(Some((det, suite_json, sha)))
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[must_use]
pub const fn detector_license() -> &'static str {
    LICENSE_TEXT
}

#[must_use]
pub fn minimum_numbers(expected: usize) -> usize {
    // math.Ceil(float64(expected) * 0.55) 等价于按有理数 55/100 向上取整（小整数范围内无精度误差）
    let scaled = expected.saturating_mul(55).div_ceil(100);
    scaled.max(80)
}

/// 将测试模型名映射为参考库中的标准模型 ID（剥离 `provider/` 前缀）
#[must_use]
pub fn canonical_fingerprint_model_id(model: &str) -> String {
    let trimmed = model.trim().to_ascii_lowercase();
    let short = trimmed.rsplit('/').next().unwrap_or(trimmed.as_str());
    match short {
        "gpt-6.1-sol" => "gpt-6-astra".to_owned(),
        "deepseek-flash" => "deepseek-v4.1-flash-1017".to_owned(),
        other => other.to_owned(),
    }
}

/// 提取最长的有效数字序列（1..=355），若数字间出现 Unicode 字母则认为进入了自然语言段落
#[must_use]
pub fn parse_numbers(text: &str) -> Vec<i32> {
    let mut best: Vec<i32> = Vec::new();
    let mut current: Vec<i32> = Vec::new();
    let bytes = text.as_bytes();
    let mut idx = 0usize;
    let mut prev_end = 0usize;

    while idx < bytes.len() {
        if !bytes[idx].is_ascii_digit() {
            idx += 1;
            continue;
        }
        let span_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        let span_end = idx;
        if text[prev_end..span_start].chars().any(char::is_alphabetic) {
            if current.len() > best.len() {
                best = current;
            }
            current = Vec::new();
        }
        if let Ok(value) = text[span_start..span_end].parse::<i32>()
            && (1..=355).contains(&value)
        {
            current.push(value);
        }
        prev_end = span_end;
    }
    if current.len() > best.len() {
        best = current;
    }
    best
}

/// 校验并裁切可送入归因器的单份数字文本
pub fn normalize_fingerprint_answer(
    answer: &str,
    expected: usize,
) -> Result<(String, usize), &'static str> {
    if answer.len() > FINGERPRINT_MAX_TEXT {
        return Err("回答过长");
    }
    let mut numbers = parse_numbers(answer);
    let mut normalized = answer.to_owned();
    if numbers.len() > expected {
        numbers.truncate(expected);
        normalized = numbers
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
    }
    let count = numbers.len();
    if count < minimum_numbers(expected) {
        return Err("有效数字不足");
    }
    Ok((normalized, count))
}

/// 执行指纹归因
pub fn analyze_fingerprint(
    outputs: &[FingerprintOutputInput],
) -> Result<FingerprintAttribution, &'static str> {
    let d = detector();
    let mut diagnostics = Vec::with_capacity(outputs.len());
    let mut accepted: Vec<Vec<i32>> = Vec::new();

    for (index, output) in outputs.iter().enumerate() {
        let numbers = parse_numbers(&output.text);
        let minimum = minimum_numbers(output.expected_count);
        let valid = numbers.len() >= minimum;
        diagnostics.push(FingerprintDiagnostic {
            index,
            parsed_numbers: numbers.len(),
            minimum_numbers: minimum,
            accepted: valid,
        });
        if valid {
            accepted.push(numbers);
        }
    }

    let used_outputs = accepted.len();
    if used_outputs == 0 {
        return Err("未采集到有效数字序列");
    }

    let sequence = sequence_shape(&accepted);
    if !sequence.model_like {
        return Ok(FingerprintAttribution {
            decision: "not_model".to_owned(),
            prediction: String::new(),
            prediction_name: String::new(),
            probability: None,
            used_outputs,
            family_prediction_name: String::new(),
            sequence: Some(sequence),
            results: Vec::new(),
            diagnostics,
        });
    }

    let ranking = d.rank(&accepted);
    let (probabilities, decision) = if used_outputs == FINGERPRINT_TARGET {
        (Some(softmax(&ranking, d.tau)), String::new())
    } else {
        (None, "partial".to_owned())
    };

    let mut order: Vec<usize> = (0..ranking.len()).collect();
    order.sort_by(|&a, &b| ranking[b].total_cmp(&ranking[a]));

    let mut results = Vec::with_capacity(order.len());
    for i in order {
        let meta = &d.models[i];
        results.push(FingerprintCandidate {
            model: meta.id.clone(),
            display_name: meta.display_name.clone(),
            family: meta.family.clone(),
            family_name: meta.family_name.clone(),
            probability: probabilities.as_ref().map(|probs| probs[i]),
            score: ranking[i],
        });
    }

    let top = &results[0];
    Ok(FingerprintAttribution {
        decision,
        prediction: top.model.clone(),
        prediction_name: top.display_name.clone(),
        probability: top.probability,
        used_outputs,
        family_prediction_name: top.family_name.clone(),
        sequence: Some(sequence),
        results,
        diagnostics,
    })
}

fn try_decode_detector(file: &[u8], models_json: &str) -> Result<Detector, String> {
    let revision = format!("sha256:{}", hex::encode(Sha256::digest(file)));
    if file.len() < 4 {
        return Err("lm-detector binary truncated".to_owned());
    }
    let header_len = u32::from_le_bytes([file[0], file[1], file[2], file[3]]) as usize;
    let header_end = 4usize
        .checked_add(header_len)
        .ok_or_else(|| "header length overflow".to_owned())?;
    if header_end > file.len() {
        return Err("lm-detector header out of bounds".to_owned());
    }
    let start = header_end.div_ceil(4) * 4;
    if start > file.len() {
        return Err("lm-detector payload out of bounds".to_owned());
    }
    let count = (file.len() - start) / 4;
    let mut floats = Vec::with_capacity(count);
    for i in 0..count {
        let b0 = file[start + i];
        let b1 = file[start + count + i];
        let b2 = file[start + 2 * count + i];
        let b3 = file[start + 3 * count + i];
        floats.push(f64::from(f32::from_le_bytes([b0, b1, b2, b3])));
    }

    let header: Value = serde_json::from_slice(&file[4..header_end])
        .map_err(|e| format!("invalid lm-detector header json: {e}"))?;
    #[derive(Deserialize)]
    struct ModelsManifest {
        built_at: String,
        models: Vec<DetectorModelMeta>,
    }
    let bank: ModelsManifest = serde_json::from_str(models_json)
        .map_err(|e| format!("invalid lm-detector models.json: {e}"))?;

    let schema = header["schema"].as_str().unwrap_or_default();
    let bank_built_at = header["bank_built_at"].as_str().unwrap_or_default();
    let base_sha256 = header["base_sha256"].as_str().unwrap_or_default();
    let ref_sha256 = header["source_reference_sha256"]
        .as_str()
        .unwrap_or_default();
    let model_ids: Vec<String> = header["model_ids"]
        .as_array()
        .ok_or_else(|| "missing model_ids array".to_owned())?
        .iter()
        .map(|v| {
            v.as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| "model id not string".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let response_counts = resolve_vector(&header["response_counts"], &floats);

    let calibration = &header["calibration"];
    let cal_schema = calibration["schema"].as_str().unwrap_or_default();
    let cal_method = calibration["method"].as_str().unwrap_or_default();
    let tau = calibration["tau"].as_f64().unwrap_or_default();
    let cal_binding = &calibration["binding"];
    let cal_base = cal_binding["base_sha256"].as_str().unwrap_or_default();
    let cal_ref = cal_binding["reference_sha256"].as_str().unwrap_or_default();
    let cal_ids: Vec<String> = cal_binding["model_ids"]
        .as_array()
        .ok_or_else(|| "missing calibration model_ids".to_owned())?
        .iter()
        .map(|v| {
            v.as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| "cal model id not string".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;

    if !(schema == "shared-detector-v2"
        && bank.built_at == bank_built_at
        && bank.models.len() == model_ids.len()
        && bank
            .models
            .iter()
            .zip(&model_ids)
            .all(|(m, id)| m.id == *id)
        && cal_schema == "shared-confidence-v2"
        && cal_method == "ranking-temperature"
        && (0.001..=1000.0).contains(&tau)
        && cal_base == base_sha256
        && cal_ref == ref_sha256
        && cal_ids == model_ids)
    {
        return Err("lm-detector detector, models and calibration do not match".to_owned());
    }
    if response_counts.len() < bank.models.len() {
        return Err("response_counts shorter than models".to_owned());
    }
    for (i, m) in bank.models.iter().enumerate() {
        if (m.response_count as f64 - response_counts[i]).abs() >= 1e-6 {
            return Err("lm-detector models do not match the detector references".to_owned());
        }
    }

    let ranker = &header["ranker"];
    let head_params = resolve_scalings(&ranker["head_params"], &floats);
    let full_params = resolve_scalings(&ranker["full_params"], &floats);
    let lda_weights = resolve_matrix(&ranker["lda_weights"], &floats);
    let lda_bias = resolve_vector(&ranker["lda_bias"], &floats);
    let references: Vec<Vec<Vec<f64>>> = ranker["references"]
        .as_array()
        .ok_or_else(|| "missing ranker.references".to_owned())?
        .iter()
        .map(|rows| resolve_matrix(rows, &floats))
        .collect();
    let reference_norms: Vec<Vec<f64>> = references
        .iter()
        .map(|rows| rows.iter().map(|r| dot(r, r)).collect())
        .collect();

    let hellinger = resolve_feature_bank(&ranker["bank"]["hellinger"], &floats);
    let ordered = resolve_feature_bank(&ranker["bank"]["ordered_blocks"], &floats);
    let blend_frequency = ranker["blend"]["frequency"]
        .as_f64()
        .ok_or_else(|| "missing blend.frequency".to_owned())?;
    let blend_positional = ranker["blend"]["positional"]
        .as_f64()
        .ok_or_else(|| "missing blend.positional".to_owned())?;
    let positional = &ranker["positional"];
    let positional_params = resolve_scalings(&positional["params"], &floats);
    let positional_weights = resolve_matrix(&positional["weights"], &floats);
    let positional_bias = resolve_vector(&positional["bias"], &floats);

    Ok(Detector {
        revision,
        built_at: bank.built_at,
        tau,
        models: bank.models,
        head_params,
        full_params,
        lda_weights,
        lda_bias,
        references,
        reference_norms,
        hellinger,
        ordered,
        blend_frequency,
        blend_positional,
        positional_params,
        positional_weights,
        positional_bias,
    })
}

fn resolve_vector(value: &Value, floats: &[f64]) -> Vec<f64> {
    let offset = value["$f32"].as_u64().expect("$f32 offset") as usize;
    let n = value["n"].as_u64().expect("n length") as usize;
    floats[offset..offset + n].to_vec()
}

fn resolve_matrix(value: &Value, floats: &[f64]) -> Vec<Vec<f64>> {
    value
        .as_array()
        .expect("matrix array")
        .iter()
        .map(|row| resolve_vector(row, floats))
        .collect()
}

fn resolve_scalings(value: &Value, floats: &[f64]) -> Vec<Scaling> {
    value
        .as_array()
        .expect("scalings array")
        .iter()
        .map(|item| Scaling {
            mean: resolve_vector(&item["mean"], floats),
            scale: resolve_vector(&item["scale"], floats),
        })
        .collect()
}

fn resolve_feature_bank(value: &Value, floats: &[f64]) -> FeatureBank {
    FeatureBank {
        mean: resolve_vector(&value["feature_mean"], floats),
        scale: resolve_vector(&value["feature_scale"], floats),
        basis: resolve_matrix(&value["nuisance_basis"], floats),
        centroids: resolve_matrix(&value["centroids"], floats),
        environments: value["environment_centroids"]
            .as_array()
            .map(|arr| arr.iter().map(|env| resolve_matrix(env, floats)).collect())
            .unwrap_or_default(),
    }
}

fn sequence_shape(answers: &[Vec<i32>]) -> FingerprintSequence {
    let mut autocorrelations = Vec::with_capacity(answers.len());
    let mut small_steps = Vec::with_capacity(answers.len());
    for numbers in answers {
        let x: Vec<f64> = numbers.iter().map(|&n| f64::from(n)).collect();
        let m = mean(&x);
        let variance: f64 = x.iter().map(|&v| (v - m) * (v - m)).sum::<f64>() / x.len() as f64;
        let mut lagged = 0.0;
        let mut small = 0.0;
        for i in 1..x.len() {
            lagged += (x[i] - m) * (x[i - 1] - m);
            if (x[i] - x[i - 1]).abs() <= 20.0 {
                small += 1.0;
            }
        }
        let autocorrelation = if variance != 0.0 {
            lagged / (x.len() - 1) as f64 / variance
        } else {
            1.0
        };
        autocorrelations.push(autocorrelation);
        small_steps.push(small / (x.len() - 1) as f64);
    }
    let ac_med = median(&autocorrelations);
    let ss_med = median(&small_steps);
    FingerprintSequence {
        autocorrelation: ac_med,
        small_step_share: ss_med,
        model_like: ac_med <= -0.23 || ss_med <= 0.05,
    }
}

fn softmax(ranking: &[f64], tau: f64) -> Vec<f64> {
    let maximum = ranking
        .iter()
        .map(|&s| tau * s)
        .fold(f64::NEG_INFINITY, f64::max);
    let mut weights: Vec<f64> = ranking.iter().map(|&s| (tau * s - maximum).exp()).collect();
    let total: f64 = weights.iter().sum();
    for w in &mut weights {
        *w /= total;
    }
    weights
}

impl Detector {
    fn rank(&self, answers: &[Vec<i32>]) -> Vec<f64> {
        let mut lda = Vec::with_capacity(answers.len());
        let mut base = Vec::with_capacity(answers.len());
        let mut positional = Vec::with_capacity(answers.len());
        let mut full = Vec::with_capacity(answers.len());

        for numbers in answers {
            full.push(join_blocks(
                &[hellinger_feature(numbers), ordered_block_feature(numbers)],
                &self.full_params,
                &[0.75, 0.25],
            ));
            let head_len = numbers.len().min(128);
            let head = &numbers[..head_len];
            let x = join_blocks(
                &[hellinger_feature(head), ordered_block_feature(head)],
                &self.head_params,
                &[0.75, 0.25],
            );
            let scores: Vec<f64> = self
                .lda_weights
                .iter()
                .enumerate()
                .map(|(i, row)| dot(row, &x) + self.lda_bias[i])
                .collect();
            lda.push(zscores(&scores));
            base.push(self.baseline(numbers));
            positional.push(zscores(&self.positional(numbers)));
        }

        let mut near = Vec::with_capacity(self.references.len());
        for (m, references) in self.references.iter().enumerate() {
            let distances: Vec<f64> = full
                .iter()
                .map(|x| -nearest_distance(x, references, &self.reference_norms[m]))
                .collect();
            near.push(median(&distances));
        }

        let l = zscores(&column_mean(&lda));
        let n = zscores(&near);
        let b = zscores(&column_mean(&base));
        let raw_freq: Vec<f64> = (0..l.len())
            .map(|i| 0.5 * l[i] + 0.25 * n[i] + 0.25 * b[i])
            .collect();
        let frequency = zscores(&raw_freq);
        let sequential = zscores(&column_mean(&positional));
        (0..frequency.len())
            .map(|i| self.blend_frequency * frequency[i] + self.blend_positional * sequential[i])
            .collect()
    }

    fn baseline(&self, numbers: &[i32]) -> Vec<f64> {
        let h = &self.hellinger;
        let o = &self.ordered;
        let h_std = standardize(&hellinger_feature(numbers), &h.mean, &h.scale);
        let h_unit = unit_vector(&subtract_basis(&h_std, &h.basis));
        let marginal: Vec<f64> = h.centroids.iter().map(|row| dot(&h_unit, row)).collect();

        let centered = standardize(&ordered_block_feature(numbers), &o.mean, &o.scale);
        let raw = unit_vector(&centered);
        let projected = unit_vector(&subtract_basis(&centered, &o.basis));

        let mut templates = Vec::with_capacity(o.centroids.len());
        let mut nuisance = Vec::with_capacity(o.centroids.len());
        for (i, row) in o.centroids.iter().enumerate() {
            let mut best_env = f64::NEG_INFINITY;
            for env in &o.environments {
                best_env = best_env.max(dot(&raw, &env[i]));
            }
            templates.push(best_env);
            nuisance.push(dot(&projected, row));
        }
        let tz = zscores(&templates);
        let nz = zscores(&nuisance);
        let combined: Vec<f64> = (0..tz.len()).map(|i| 0.5 * tz[i] + 0.5 * nz[i]).collect();
        let ordered = zscores(&combined);
        let mz = zscores(&marginal);
        (0..mz.len())
            .map(|i| 0.5 * mz[i] + 0.5 * ordered[i])
            .collect()
    }

    fn positional(&self, numbers: &[i32]) -> Vec<f64> {
        let x = join_blocks(
            &[
                hellinger_feature(numbers),
                ordered_block_feature(numbers),
                position_kernel(numbers),
                sequence_features(numbers),
            ],
            &self.positional_params,
            &[0.6, 0.15, 0.125, 0.125],
        );
        self.positional_weights
            .iter()
            .enumerate()
            .map(|(k, row)| {
                let mut acc = self.positional_bias[k];
                for (i, &w) in row.iter().enumerate() {
                    acc += w * x[i];
                }
                acc
            })
            .collect()
    }
}

fn hellinger_feature(numbers: &[i32]) -> Vec<f64> {
    let mut counts = vec![0.5; 355];
    for &n in numbers {
        counts[(n - 1) as usize] += 1.0;
    }
    root_shares(&counts)
}

fn ordered_block_feature(numbers: &[i32]) -> Vec<f64> {
    let mut out = Vec::with_capacity(74);
    let mut start = 0usize;
    let len = numbers.len();
    for i in 0..4 {
        let mut size = len / 4;
        if i < len % 4 {
            size += 1;
        }
        let mut bins = vec![0.5; 16];
        for &n in &numbers[start..start + size] {
            let bucket = (((f64::from(n - 1) / 355.0) * 16.0).floor() as usize).min(15);
            bins[bucket] += 1.0;
        }
        out.extend(root_shares(&bins));
        start += size;
    }
    let mut digits = vec![0.5; 10];
    for &n in numbers {
        digits[(n % 10) as usize] += 1.0;
    }
    out.extend(root_shares(&digits));
    out
}

fn position_kernel(numbers: &[i32]) -> Vec<f64> {
    let n = numbers.len() as f64;
    let mut out = vec![0.0; 34 * 3];
    for (position, &value) in numbers.iter().enumerate() {
        let t = (position as f64 + 0.5) * 2.0 / n - 1.0;
        let basis = [
            t,
            0.5 * (3.0 * t * t - 1.0),
            0.5 * (5.0 * t.powi(3) - 3.0 * t),
        ];
        for k in 0..34usize {
            let mut weight = 0.0;
            if k < 24 {
                let distance = (f64::from(value) - (1.0 + k as f64 * 354.0 / 23.0)) / 20.0;
                weight = (-0.5 * distance * distance).exp();
            } else if k - 24 == (value % 10) as usize {
                weight = 1.0;
            }
            for (j, &b) in basis.iter().enumerate() {
                out[k * 3 + j] += weight * b / n;
            }
        }
    }
    out
}

fn sequence_features(numbers: &[i32]) -> Vec<f64> {
    let x: Vec<f64> = numbers.iter().map(|&n| f64::from(n)).collect();
    let n = x.len();
    let m = mean(&x);
    let variance: f64 = x.iter().map(|&v| (v - m) * (v - m)).sum::<f64>();
    let sd = (variance / n as f64).sqrt().max(1.0);
    let s: Vec<f64> = x.iter().map(|&v| (v - m) / sd).collect();

    let fraction = |count: usize, test: &dyn Fn(usize) -> bool| -> f64 {
        let mut hits = 0usize;
        for i in 0..count {
            if test(i) {
                hits += 1;
            }
        }
        hits as f64 / count as f64
    };

    let mut out = Vec::new();
    for lag in [1usize, 2, 3, 5, 8, 13, 21, 34] {
        let count = n - lag;
        let mut correlation = 0.0;
        let mut distance = 0.0;
        for i in 0..count {
            correlation += s[i + lag] * s[i];
            distance += f64::from((numbers[i + lag] - numbers[i]).abs());
        }
        out.push(correlation / count as f64);
        out.push(distance / count as f64 / 355.0);
        out.push(fraction(count, &|i| numbers[i + lag] - numbers[i] > 0));
        out.push(fraction(count, &|i| numbers[i + lag] == numbers[i]));
        out.push(fraction(count, &|i| {
            (numbers[i + lag] - numbers[i]).abs() <= 5
        }));
        out.push(fraction(count, &|i| {
            (numbers[i + lag] - numbers[i]).abs() <= 20
        }));
    }

    let bins: Vec<usize> = numbers
        .iter()
        .map(|&v| (((v - 1) * 8) / 355).clamp(0, 7) as usize)
        .collect();
    let digits: Vec<usize> = numbers.iter().map(|&v| (v % 10) as usize).collect();
    out.extend(centred_transitions(&bins, 8));
    out.extend(centred_transitions(&digits, 10));

    out.push(fraction(n - 2, &|i| {
        (numbers[i + 2] - numbers[i + 1]).cmp(&0) == (numbers[i + 1] - numbers[i]).cmp(&0)
    }));
    out.push(fraction(n - 2, &|i| {
        (numbers[i + 2] - numbers[i + 1]) == (numbers[i + 1] - numbers[i])
    }));
    out.push(fraction(n - 1, &|i| numbers[i] % 10 == numbers[i + 1] % 10));
    out.push(fraction(n - 1, &|i| (s[i + 1] - s[i]).abs() > 1.0));
    out.push(fraction(n - 1, &|i| (s[i + 1] - s[i]).abs() > 2.0));

    for width in [4usize, 8, 16] {
        let mut coverage = 0.0;
        let windows = n - width + 1;
        for i in 0..windows {
            let mut seen = HashSet::with_capacity(width);
            for &v in &numbers[i..i + width] {
                seen.insert((((v - 1) * 16) / 355).clamp(0, 15));
            }
            coverage += seen.len() as f64 / width as f64;
        }
        out.push(coverage / windows as f64);
    }

    out
}

fn centred_transitions(symbols: &[usize], bins: usize) -> Vec<f64> {
    let mut joint = vec![0.5; bins * bins];
    for i in 1..symbols.len() {
        joint[symbols[i - 1] * bins + symbols[i]] += 1.0;
    }
    let total = (symbols.len() - 1) as f64 + 0.5 * (bins * bins) as f64;
    for v in &mut joint {
        *v /= total;
    }
    let mut rows = vec![0.0; bins];
    for (i, &v) in joint.iter().enumerate() {
        rows[i / bins] += v;
    }
    let mut columns = vec![0.0; bins];
    for (b, col) in columns.iter_mut().enumerate() {
        for a in 0..bins {
            *col += joint[a * bins + b];
        }
    }
    for i in 0..joint.len() {
        joint[i] -= rows[i / bins] * columns[i % bins];
    }
    joint
}

fn join_blocks(blocks: &[Vec<f64>], params: &[Scaling], shares: &[f64]) -> Vec<f64> {
    let mut out = Vec::new();
    for (j, block) in blocks.iter().enumerate() {
        let weight = shares[j].sqrt();
        let normed = unit_vector(&standardize(block, &params[j].mean, &params[j].scale));
        for v in normed {
            out.push(v * weight);
        }
    }
    out
}

fn nearest_distance(x: &[f64], references: &[Vec<f64>], norms: &[f64]) -> f64 {
    let xx = dot(x, x);
    let mut distances: Vec<f64> = references
        .iter()
        .enumerate()
        .map(|(i, r)| (xx + norms[i] - 2.0 * dot(x, r)).max(0.0))
        .collect();
    distances.sort_by(f64::total_cmp);
    mean(&distances[..7])
}

fn root_shares(bins: &[f64]) -> Vec<f64> {
    let total: f64 = bins.iter().sum();
    bins.iter().map(|&v| (v / total).sqrt()).collect()
}

fn standardize(v: &[f64], center: &[f64], scale: &[f64]) -> Vec<f64> {
    v.iter()
        .enumerate()
        .map(|(i, &val)| (val - center[i]) / scale[i])
        .collect()
}

fn subtract_basis(v: &[f64], basis: &[Vec<f64>]) -> Vec<f64> {
    let coefficients: Vec<f64> = basis.iter().map(|row| dot(v, row)).collect();
    v.iter()
        .enumerate()
        .map(|(i, &x)| {
            let projection: f64 = basis
                .iter()
                .enumerate()
                .map(|(j, row)| coefficients[j] * row[i])
                .sum();
            x - projection
        })
        .collect()
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(&x, &y)| x * y).sum()
}

fn mean(a: &[f64]) -> f64 {
    a.iter().sum::<f64>() / a.len() as f64
}

fn unit_vector(a: &[f64]) -> Vec<f64> {
    let scale = dot(a, a).sqrt().max(1e-12);
    a.iter().map(|&v| v / scale).collect()
}

fn zscores(a: &[f64]) -> Vec<f64> {
    let m = mean(a);
    let variance: f64 = a.iter().map(|&v| (v - m) * (v - m)).sum::<f64>();
    let scale = (variance / a.len() as f64).sqrt().max(1e-12);
    a.iter().map(|&v| (v - m) / scale).collect()
}

fn column_mean(rows: &[Vec<f64>]) -> Vec<f64> {
    let cols = rows[0].len();
    let mut out = vec![0.0; cols];
    for row in rows {
        for (i, &val) in row.iter().enumerate() {
            out[i] += val;
        }
    }
    let count = rows.len() as f64;
    for v in &mut out {
        *v /= count;
    }
    out
}

fn median(a: &[f64]) -> f64 {
    let mut sorted = a.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct GoldenCase {
        outputs: Vec<FingerprintOutputInput>,
        expected: FingerprintAttribution,
    }

    #[test]
    fn matches_lm_detector_golden_cases() {
        let raw = include_str!("../testdata/lm-detector-golden.json");
        let cases: Vec<GoldenCase> = serde_json::from_str(raw).expect("golden json");
        assert!(!cases.is_empty());
        for (idx, case) in cases.iter().enumerate() {
            let actual = analyze_fingerprint(&case.outputs).expect("analyze succeeds");
            assert_eq!(
                actual.decision, case.expected.decision,
                "case {idx} decision"
            );
            assert_eq!(
                actual.used_outputs, case.expected.used_outputs,
                "case {idx} used"
            );
            assert_eq!(
                actual.diagnostics, case.expected.diagnostics,
                "case {idx} diag"
            );
            assert_eq!(
                actual.prediction, case.expected.prediction,
                "case {idx} prediction"
            );
            assert_eq!(
                actual.prediction_name, case.expected.prediction_name,
                "case {idx} prediction_name"
            );
            assert_eq!(
                actual.family_prediction_name, case.expected.family_prediction_name,
                "case {idx} family"
            );
            if let (Some(a), Some(e)) = (actual.probability, case.expected.probability) {
                assert!(
                    (a - e).abs() < 1e-6,
                    "case {idx} prob: actual={a}, expected={e}"
                );
            } else {
                assert_eq!(actual.probability, case.expected.probability);
            }
            if let (Some(a), Some(e)) = (&actual.sequence, &case.expected.sequence) {
                assert_eq!(a.model_like, e.model_like);
                assert!((a.autocorrelation - e.autocorrelation).abs() < 1e-6);
                assert!((a.small_step_share - e.small_step_share).abs() < 1e-6);
            }
        }
    }
}
