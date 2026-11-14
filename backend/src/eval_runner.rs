//! 糖果题、模型指纹、画鹈鹕检测执行器（通过宿主 host.model.execute 执行）

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use gateway_plugin_sdk::client::HostClient;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::codex_turn::build_codex_turn_body;
use crate::host_calls::{self, CancelFlag, EvalUsage};
use crate::lm_detector::{
    FINGERPRINT_TARGET, FingerprintAttribution, FingerprintOutputInput, analyze_fingerprint,
    canonical_fingerprint_model_id, detector_revision, normalize_fingerprint_answer,
};
use crate::state_store::{EvalMode, PelicanVariantKind, RecordHeader, TriggerKind, truncate_chars};

const ENROLLMENT_SUITE_JSON: &str = include_str!("../data/lm-detector/enrollment-suite.json");

pub const CANDY_MAX_RUNS: u32 = 10;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
const CHALLENGE_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_ATTEMPTS: usize = 3;
const PELICAN_MAX_CODE: usize = 192 * 1024;

pub const CANDY_PROMPT: &str = "不使用任何外部工具回答以下问题：\n\n\
在一个黑色的袋子里放有三种口味的糖果，每种糖果有两种不同的形状（圆形和五角星形，不同的形状靠手感可以分辨）。现已知不同口味的糖和不同形状的数量统计如下表。参赛者需要在活动前决定摸出的糖果数目，那么，最少取出多少个糖果才能保证手中同时拥有不同形状的苹果味和桃子味的糖？（同时手中有圆形苹果味匹配五角星桃子味糖果，或者有圆形桃子味匹配五角星苹果味糖果都满足要求）\n\n\
        苹果味  桃子味  西瓜味\n\
圆形       7      9      8\n\
五角星形   7      6      4\n";

#[derive(Debug, Clone, Copy)]
pub struct PelicanVariant {
    pub id: &'static str,
    pub label: &'static str,
    pub language: &'static str,
    pub prompt: &'static str,
}

pub const PELICAN_VARIANTS: [PelicanVariant; 2] = [
    PelicanVariant {
        id: "original",
        label: "原版",
        language: "html",
        prompt: "不使用任何工具，也不要创建或修改文件，直接在回复中完成以下任务：\n\n\
创建一个 HTML，内容是 SVG 绘制一个鹈鹕骑自行车的 2D 动画，你不需要任何测试，不要使用外部资产。\n\n\
在一个 ```html 代码块中给出完整的 HTML 文件。",
    },
    PelicanVariant {
        id: "all-in-one",
        label: "多合一版",
        language: "svg",
        prompt: "不使用任何工具，也不要创建或修改文件，直接在回复中完成以下任务：\n\n\
做一张精细的SVG图片，内容是鹈鹕骑着自行车在孙悟空开的大型飞机机翼上骑行，\n\
鹈鹕展开翅膀，上面托着秦始皇的北极熊，秦始皇骑在熊上打螺丝。鹈鹕喊出\n\
“在一个黑色的袋子里放有三种口味的糖果，每种糖果有两种不同的形状（圆形和五角星形，不同的形状靠手感可以分辨）。现已知不同口味的糖和不同形状的数量统计如下表。参赛者需要在活动前决定摸出的糖果数目，那么，最少取出多少个糖果才能保证手中同时拥有不同形状的苹果味和桃子味的糖？（同时手中有圆形苹果味匹配五角星桃子味糖果，或者有圆形桃子味匹配五角星苹果味糖果都满足要求）\n\
苹果味 桃子味 西瓜味\n\
圆形 7 9 8\n\
五角星形 7 6 4”\n\n\
的结果数值\n\n\
在一个 ```svg 代码块中给出完整的 SVG 文件。",
    },
];

#[must_use]
pub fn pelican_variant_spec(kind: PelicanVariantKind) -> PelicanVariant {
    match kind {
        PelicanVariantKind::Original => PELICAN_VARIANTS[0],
        PelicanVariantKind::AllInOne => PELICAN_VARIANTS[1],
    }
}

#[derive(Debug, Clone)]
struct FingerprintChallenge {
    instructions: String,
    prompt: String,
    expected_count: usize,
}

static ENROLLMENT_SUITE: OnceLock<Vec<FingerprintChallenge>> = OnceLock::new();

fn enrollment_suite() -> &'static [FingerprintChallenge] {
    ENROLLMENT_SUITE.get_or_init(|| {
        #[derive(Deserialize)]
        struct RawItem {
            system: String,
            user_prefix: String,
            prompt: String,
            expected_count: usize,
        }
        let items: Vec<RawItem> =
            serde_json::from_str(ENROLLMENT_SUITE_JSON).expect("valid enrollment-suite.json");
        items
            .into_iter()
            .map(|c| {
                let prompt = if c.user_prefix.is_empty() {
                    c.prompt
                } else {
                    format!("{}\n\nFinal task:\n{}", c.user_prefix, c.prompt)
                };
                FingerprintChallenge {
                    instructions: c.system,
                    prompt,
                    expected_count: c.expected_count,
                }
            })
            .collect()
    })
}

#[derive(Debug, Clone)]
pub struct CompletedEvalRecord {
    pub header: RecordHeader,
    pub detail: Value,
}

#[derive(Debug, Clone, Serialize)]
struct CandyAttemptDetail {
    index: u32,
    status: String,
    attempts: usize,
    duration_ms: i64,
    input_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    answer: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
}

#[allow(clippy::too_many_arguments)]
pub async fn run_candy_eval(
    host: &HostClient,
    client_key_id: &str,
    account_id: &str,
    account_name: &str,
    model: &str,
    effort: &str,
    runs: u32,
    trigger_kind: TriggerKind,
    cancel: &CancelFlag,
    on_progress: impl Fn(u32) + Send + Sync,
) -> Option<CompletedEvalRecord> {
    let started = Instant::now();
    let total = runs.clamp(1, CANDY_MAX_RUNS);
    let mut items = Vec::with_capacity(total as usize);
    let mut correct = 0u32;
    let mut wrong = 0u32;
    let mut failed = 0u32;
    let mut total_usage = EvalUsage::default();

    for idx in 1..=total {
        if cancel.is_cancelled() {
            break;
        }
        let item_start = Instant::now();
        let outcome = ask_codex(
            host,
            client_key_id,
            account_id,
            model,
            effort,
            "",
            CANDY_PROMPT,
            REQUEST_TIMEOUT,
            cancel,
            |_| Ok::<(), String>(()),
        )
        .await;
        if cancel.is_cancelled() {
            break;
        }
        let item_ms = item_start.elapsed().as_millis() as i64;
        match outcome {
            Ok((answer, attempts, usage)) => {
                total_usage.merge_max_or_add(&usage);
                let is_correct = has_standalone_21(&answer);
                if is_correct {
                    correct += 1;
                } else {
                    wrong += 1;
                }
                items.push(CandyAttemptDetail {
                    index: idx,
                    status: if is_correct { "correct" } else { "wrong" }.to_owned(),
                    attempts,
                    duration_ms: item_ms,
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    reasoning_tokens: usage.reasoning_tokens,
                    answer: truncate_chars(&answer, 4000),
                    error: String::new(),
                });
            }
            Err((err, attempts, usage)) => {
                total_usage.merge_max_or_add(&usage);
                failed += 1;
                items.push(CandyAttemptDetail {
                    index: idx,
                    status: "failed".to_owned(),
                    attempts,
                    duration_ms: item_ms,
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    reasoning_tokens: usage.reasoning_tokens,
                    answer: String::new(),
                    error: truncate_chars(&err, 500),
                });
            }
        }
        on_progress(idx);
    }

    if cancel.is_cancelled() && items.is_empty() {
        return None;
    }

    let completed = items.len() as u32;
    let status = if cancel.is_cancelled() && completed < total {
        "stopped"
    } else if completed == 0 || (correct == 0 && wrong == 0) {
        "failed"
    } else if wrong == 0 && failed == 0 {
        "passed"
    } else {
        "degraded"
    };

    let last_error = items
        .iter()
        .rev()
        .find(|i| !i.error.is_empty())
        .map(|i| i.error.clone())
        .unwrap_or_default();

    let summary = json!({
        "runs": total,
        "completed": completed,
        "correct": correct,
        "wrong": wrong,
        "failed": failed,
        "pass_rate": if correct + wrong > 0 {
            f64::from(correct) / f64::from(correct + wrong)
        } else {
            0.0
        },
    });
    let detail = json!({
        "runs": total,
        "correct": correct,
        "wrong": wrong,
        "failed": failed,
        "items": items,
    });

    Some(CompletedEvalRecord {
        header: RecordHeader {
            id: Uuid::now_v7().to_string(),
            account_id: account_id.to_owned(),
            account_name: account_name.to_owned(),
            eval_type: EvalMode::Candy,
            trigger_kind,
            model: model.to_owned(),
            status: status.to_owned(),
            effort: effort.to_owned(),
            variant: String::new(),
            summary,
            duration_ms: started.elapsed().as_millis() as i64,
            input_tokens: total_usage.input_tokens as i64,
            output_tokens: total_usage.output_tokens as i64,
            reasoning_tokens: total_usage.reasoning_tokens as i64,
            error_message: last_error,
            created_at_ms: Utc::now().timestamp_millis(),
        },
        detail,
    })
}

#[derive(Debug, Clone, Serialize)]
struct FingerprintSampleRecord {
    prompt: String,
    expected_count: usize,
    attempts: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    text: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
    parsed_numbers: usize,
    accepted: bool,
}

#[allow(clippy::too_many_arguments)]
pub async fn run_fingerprint_eval(
    host: &HostClient,
    client_key_id: &str,
    account_id: &str,
    account_name: &str,
    model: &str,
    concurrency: u32,
    trigger_kind: TriggerKind,
    cancel: &CancelFlag,
    on_progress: impl Fn(u32) + Send + Sync,
) -> CompletedEvalRecord {
    let started = Instant::now();
    let challenges = pick_fingerprint_challenges();
    let per_account_limit = (concurrency as usize).clamp(1, FINGERPRINT_TARGET);
    let semaphore = Arc::new(Semaphore::new(per_account_limit));
    let done_counter = Arc::new(AtomicU32::new(0));
    let on_progress_ref = &on_progress;

    let run_one = async |challenge: FingerprintChallenge| -> (FingerprintSampleRecord, EvalUsage) {
        let _permit = tokio::select! {
            _ = cancel.cancelled() => {
                return (
                    FingerprintSampleRecord {
                        prompt: challenge.prompt,
                        expected_count: challenge.expected_count,
                        attempts: 0,
                        text: String::new(),
                        error: "已停止检测".to_owned(),
                        parsed_numbers: 0,
                        accepted: false,
                    },
                    EvalUsage::default(),
                );
            }
            permit = semaphore.acquire() => match permit {
                Ok(p) => p,
                Err(_) => {
                    return (
                        FingerprintSampleRecord {
                            prompt: challenge.prompt,
                            expected_count: challenge.expected_count,
                            attempts: 0,
                            text: String::new(),
                            error: "信号量异常".to_owned(),
                            parsed_numbers: 0,
                            accepted: false,
                        },
                        EvalUsage::default(),
                    );
                }
            }
        };

        let mut parsed_numbers = 0usize;
        let mut normalized_text = String::new();
        let outcome = ask_codex(
            host,
            client_key_id,
            account_id,
            model,
            "",
            &challenge.instructions,
            &challenge.prompt,
            CHALLENGE_TIMEOUT,
            cancel,
            |answer| {
                let (norm, count) = normalize_fingerprint_answer(answer, challenge.expected_count)
                    .map_err(str::to_owned)?;
                normalized_text = norm;
                parsed_numbers = count;
                Ok::<(), String>(())
            },
        )
        .await;

        let next_done = done_counter.fetch_add(1, Ordering::Relaxed) + 1;
        on_progress_ref(next_done);

        match outcome {
            Ok((_, attempts, usage)) => (
                FingerprintSampleRecord {
                    prompt: challenge.prompt,
                    expected_count: challenge.expected_count,
                    attempts,
                    text: normalized_text,
                    error: String::new(),
                    parsed_numbers,
                    accepted: true,
                },
                usage,
            ),
            Err((err, attempts, usage)) => (
                FingerprintSampleRecord {
                    prompt: challenge.prompt,
                    expected_count: challenge.expected_count,
                    attempts,
                    text: String::new(),
                    error: truncate_chars(&err, 500),
                    parsed_numbers: 0,
                    accepted: false,
                },
                usage,
            ),
        }
    };

    let (r0, r1, r2) = tokio::join!(
        run_one(challenges[0].clone()),
        run_one(challenges[1].clone()),
        run_one(challenges[2].clone()),
    );

    let mut total_usage = EvalUsage::default();
    let mut samples = Vec::with_capacity(FINGERPRINT_TARGET);
    for (sample, usage) in [r0, r1, r2] {
        total_usage.merge_max_or_add(&usage);
        samples.push(sample);
    }

    let outputs: Vec<FingerprintOutputInput> = samples
        .iter()
        .map(|s| FingerprintOutputInput {
            text: s.text.clone(),
            expected_count: s.expected_count,
        })
        .collect();

    let attribution_result = analyze_fingerprint(&outputs);
    let expected_canonical = canonical_fingerprint_model_id(model);
    let mut error_message = String::new();
    let done = done_counter.load(Ordering::Relaxed);
    let (status, attribution_val) = match attribution_result {
        Ok(attr) => {
            let st = if cancel.is_cancelled() && done < FINGERPRINT_TARGET as u32 {
                "stopped"
            } else if attr.decision == "not_model" {
                "uncertain"
            } else if attr.used_outputs < FINGERPRINT_TARGET {
                "partial"
            } else if attr.prediction == expected_canonical {
                "matched"
            } else {
                "mismatched"
            };
            (st, Some(attr))
        }
        Err(err) => {
            error_message = samples
                .iter()
                .rev()
                .find(|s| !s.error.is_empty())
                .map_or_else(|| err.to_owned(), |s| s.error.clone());
            let st = if cancel.is_cancelled() {
                "stopped"
            } else {
                "failed"
            };
            (st, None)
        }
    };

    let summary =
        build_fingerprint_summary(attribution_val.as_ref(), &expected_canonical, concurrency);
    let detail = json!({
        "detector_revision": detector_revision(),
        "expected_model_id": expected_canonical,
        "concurrency": concurrency,
        "samples": samples,
        "attribution": attribution_val,
    });

    CompletedEvalRecord {
        header: RecordHeader {
            id: Uuid::now_v7().to_string(),
            account_id: account_id.to_owned(),
            account_name: account_name.to_owned(),
            eval_type: EvalMode::Fingerprint,
            trigger_kind,
            model: model.to_owned(),
            status: status.to_owned(),
            effort: String::new(),
            variant: String::new(),
            summary,
            duration_ms: started.elapsed().as_millis() as i64,
            input_tokens: total_usage.input_tokens as i64,
            output_tokens: total_usage.output_tokens as i64,
            reasoning_tokens: total_usage.reasoning_tokens as i64,
            error_message,
            created_at_ms: Utc::now().timestamp_millis(),
        },
        detail,
    }
}

fn build_fingerprint_summary(
    attr: Option<&FingerprintAttribution>,
    expected_canonical: &str,
    concurrency: u32,
) -> Value {
    match attr {
        Some(a) => json!({
            "detector_revision": detector_revision(),
            "expected_model_id": expected_canonical,
            "concurrency": concurrency,
            "decision": a.decision,
            "prediction": a.prediction,
            "prediction_name": a.prediction_name,
            "probability": a.probability,
            "used_outputs": a.used_outputs,
            "family_prediction_name": a.family_prediction_name,
            "sequence": a.sequence,
            "matched": !a.prediction.is_empty() && a.prediction == expected_canonical,
        }),
        None => json!({
            "detector_revision": detector_revision(),
            "expected_model_id": expected_canonical,
            "concurrency": concurrency,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_pelican_eval(
    host: &HostClient,
    client_key_id: &str,
    account_id: &str,
    account_name: &str,
    model: &str,
    effort: &str,
    variant: PelicanVariant,
    trigger_kind: TriggerKind,
    cancel: &CancelFlag,
) -> CompletedEvalRecord {
    let started = Instant::now();
    let mut rendered_page = String::new();
    let outcome = ask_codex(
        host,
        client_key_id,
        account_id,
        model,
        effort,
        "",
        variant.prompt,
        REQUEST_TIMEOUT,
        cancel,
        |answer| {
            rendered_page = build_pelican_page(answer, variant)?;
            Ok::<(), String>(())
        },
    )
    .await;

    let (status, error_message, attempts, usage) = match outcome {
        Ok((_, att, u)) => ("completed", String::new(), att, u),
        Err((err, att, u)) => {
            rendered_page.clear();
            let st = if cancel.is_cancelled() {
                "stopped"
            } else {
                "failed"
            };
            (st, truncate_chars(&err, 500), att, u)
        }
    };

    let summary = json!({
        "variant": variant.id,
        "variant_label": variant.label,
        "attempts": attempts,
        "size_bytes": rendered_page.len(),
    });
    let detail = json!({
        "variant": variant.id,
        "variant_label": variant.label,
        "attempts": attempts,
        "html": rendered_page,
    });

    CompletedEvalRecord {
        header: RecordHeader {
            id: Uuid::now_v7().to_string(),
            account_id: account_id.to_owned(),
            account_name: account_name.to_owned(),
            eval_type: EvalMode::Pelican,
            trigger_kind,
            model: model.to_owned(),
            status: status.to_owned(),
            effort: effort.to_owned(),
            variant: variant.id.to_owned(),
            summary,
            duration_ms: started.elapsed().as_millis() as i64,
            input_tokens: usage.input_tokens as i64,
            output_tokens: usage.output_tokens as i64,
            reasoning_tokens: usage.reasoning_tokens as i64,
            error_message,
            created_at_ms: Utc::now().timestamp_millis(),
        },
        detail,
    }
}

#[allow(clippy::too_many_arguments)]
async fn ask_codex<F, E>(
    host: &HostClient,
    client_key_id: &str,
    account_id: &str,
    model: &str,
    effort: &str,
    instructions: &str,
    prompt: &str,
    timeout: Duration,
    cancel: &CancelFlag,
    mut accept: F,
) -> Result<(String, usize, EvalUsage), (String, usize, EvalUsage)>
where
    F: FnMut(&str) -> Result<(), E>,
    E: std::fmt::Display,
{
    let mut attempts = 0usize;
    let mut total_usage = EvalUsage::default();
    let mut last_error = String::new();

    for attempt in 1..=MAX_ATTEMPTS {
        if cancel.is_cancelled() {
            return Err(("已停止检测".to_owned(), attempts, total_usage));
        }
        attempts = attempt;
        let body = match build_codex_turn_body(account_id, model, effort, instructions, prompt) {
            Ok(b) => b,
            Err(err) => return Err((err, attempts, total_usage)),
        };

        let result = tokio::select! {
            _ = cancel.cancelled() => {
                return Err(("已停止检测".to_owned(), attempts, total_usage));
            }
            res = tokio::time::timeout(
                timeout,
                host_calls::execute_openai_generate(host, cancel, client_key_id, account_id, model, body),
            ) => match res {
                Ok(inner) => inner,
                Err(_) => {
                    last_error = format!("请求超时（>{}s）", timeout.as_secs());
                    if attempt < MAX_ATTEMPTS {
                        backoff_sleep(attempt, cancel).await;
                    }
                    continue;
                }
            }
        };

        match result {
            Ok(outcome) => {
                total_usage.merge_max_or_add(&outcome.usage);
                if outcome.text.trim().is_empty() {
                    last_error = "模型返回空响应".to_owned();
                } else {
                    match accept(&outcome.text) {
                        Ok(()) => return Ok((outcome.text, attempts, total_usage)),
                        Err(reason) => last_error = reason.to_string(),
                    }
                }
            }
            Err(fault) => {
                last_error = truncate_chars(&fault.message, 320);
            }
        }

        if attempt < MAX_ATTEMPTS && !cancel.is_cancelled() {
            backoff_sleep(attempt, cancel).await;
        }
    }

    Err((last_error, attempts, total_usage))
}

async fn backoff_sleep(attempt: usize, cancel: &CancelFlag) {
    let base_ms = 750u64 << (attempt - 1);
    let jitter_ms = (Uuid::new_v4().as_u128() as u64) % 400;
    let delay = Duration::from_millis(base_ms + jitter_ms);
    tokio::select! {
        _ = cancel.cancelled() => {}
        _ = tokio::time::sleep(delay) => {}
    }
}

fn pick_fingerprint_challenges() -> Vec<FingerprintChallenge> {
    let suite = enrollment_suite();
    let mut indices: Vec<usize> = (0..suite.len()).collect();
    let mut seed = Uuid::new_v4().as_u128();
    for i in 0..FINGERPRINT_TARGET.min(indices.len()) {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let j = i + ((seed as usize) % (indices.len() - i));
        indices.swap(i, j);
    }
    indices
        .into_iter()
        .take(FINGERPRINT_TARGET)
        .map(|idx| suite[idx].clone())
        .collect()
}

/// 检查文本中是否存在前后均非数字的独立 "21"
#[must_use]
pub fn has_standalone_21(text: &str) -> bool {
    let b = text.as_bytes();
    for i in 0..b.len().saturating_sub(1) {
        if b[i] == b'2'
            && b[i + 1] == b'1'
            && (i == 0 || !b[i - 1].is_ascii_digit())
            && (i + 2 >= b.len() || !b[i + 2].is_ascii_digit())
        {
            return true;
        }
    }
    false
}

/// 从回答中抽取指定标记语言并构造安全展示 HTML
pub fn build_pelican_page(answer: &str, variant: PelicanVariant) -> Result<String, String> {
    let code = extract_fenced_code(answer, variant.language);
    let upper = variant.language.to_ascii_uppercase();
    if code.is_empty() {
        return Err(format!("回答中没有 {upper} 代码"));
    }
    if code.len() > PELICAN_MAX_CODE {
        return Err(format!(
            "{upper} 超过 {} KB，未保存",
            PELICAN_MAX_CODE >> 10
        ));
    }
    if variant.language == "svg" {
        Ok(format!(
            "<!doctype html><style>html, body {{ height: 100%; margin: 0; }} body > svg {{ display: block; width: 100%; height: 100%; }}</style>{code}"
        ))
    } else {
        Ok(code)
    }
}

/// 提取三反引号代码块；优先匹配 `language`，其次匹配首个 `<` 开头的块，再次裸 `<...>` 文本
#[must_use]
pub fn extract_fenced_code(answer: &str, language: &str) -> String {
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut lines = answer.lines().peekable();

    while let Some(line) = lines.next() {
        let trimmed = line.trim_start_matches([' ', '\t']);
        if let Some(rest) = trimmed.strip_prefix("```") {
            let lang: String = rest
                .trim_start_matches([' ', '\t'])
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            let mut body = Vec::new();
            let mut closed = false;
            for inner in lines.by_ref() {
                if inner.trim() == "```" {
                    closed = true;
                    break;
                }
                body.push(inner);
            }
            if closed {
                blocks.push((lang, body.join("\n")));
            }
        }
    }

    for (lang, body) in &blocks {
        if lang.eq_ignore_ascii_case(language) {
            return body.trim().to_owned();
        }
    }
    for (_, body) in &blocks {
        let trimmed = body.trim();
        if trimmed.starts_with('<') {
            return trimmed.to_owned();
        }
    }
    let trimmed = answer.trim();
    if blocks.is_empty() && trimmed.starts_with('<') {
        return trimmed.to_owned();
    }
    String::new()
}
