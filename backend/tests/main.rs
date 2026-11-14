use std::collections::BTreeMap;

use codex_proxy_plugin_candy_eval::codex_turn::build_codex_turn_body;
use codex_proxy_plugin_candy_eval::eval_runner::{
    PELICAN_VARIANTS, extract_fenced_code, has_standalone_21,
};
use codex_proxy_plugin_candy_eval::lm_detector::{
    FingerprintOutputInput, analyze_fingerprint, canonical_fingerprint_model_id,
};
use codex_proxy_plugin_candy_eval::state_store::{
    CandyEvalSettings, QueueState, RecordDetail, RecordsIndex,
};
use codex_proxy_plugin_candy_eval::{manifest, plugin};
use serde::Deserialize;
use serde_json::{Value, json};

#[test]
fn manifest_and_plugin_builder_validate() {
    let parsed = manifest().expect("valid author manifest");
    assert_eq!(parsed.plugin_id().expect("id"), "light-yzc.candy-eval");
    let _composed = plugin().expect("plugin builder compiles cleanly");

    // 验证全部 state JSON Schema 均可离线编译且与默认值匹配
    let validators: BTreeMap<String, jsonschema::Validator> = parsed
        .state
        .iter()
        .map(|s| {
            let v = jsonschema::validator_for(&s.schema).expect("offline state schema valid");
            (s.namespace.clone(), v)
        })
        .collect();

    let settings_val = serde_json::to_value(CandyEvalSettings::default()).unwrap();
    assert!(
        validators["candy_eval_settings"].is_valid(&settings_val),
        "default settings must satisfy schema"
    );

    let queue_val = serde_json::to_value(QueueState::default()).unwrap();
    assert!(
        validators["candy_eval_queue"].is_valid(&queue_val),
        "default queue must satisfy schema"
    );

    let index_val = serde_json::to_value(RecordsIndex::default()).unwrap();
    assert!(
        validators["candy_eval_records"].is_valid(&index_val),
        "default records index must satisfy schema"
    );

    let detail_val = serde_json::to_value(RecordDetail {
        id: "rec_1".to_owned(),
        detail: json!({"items": []}),
    })
    .unwrap();
    assert!(
        validators["candy_eval_detail"].is_valid(&detail_val),
        "sample record detail must satisfy schema"
    );
}

#[test]
fn lm_detector_golden_vectors_match() {
    #[derive(Deserialize)]
    struct GoldenSample {
        text: String,
        expected_count: usize,
    }
    #[derive(Deserialize)]
    struct GoldenExpected {
        prediction: String,
        decision: String,
        probability: Option<f64>,
    }
    #[derive(Deserialize)]
    struct GoldenCase {
        outputs: Vec<GoldenSample>,
        expected: GoldenExpected,
    }

    let cases: Vec<GoldenCase> =
        serde_json::from_str(include_str!("../testdata/lm-detector-golden.json"))
            .expect("valid golden json");
    assert!(!cases.is_empty());

    for case in cases {
        let inputs: Vec<FingerprintOutputInput> = case
            .outputs
            .into_iter()
            .map(|s| FingerprintOutputInput {
                text: s.text,
                expected_count: s.expected_count,
            })
            .collect();
        let attr = analyze_fingerprint(&inputs).expect("attribution succeeds");
        assert_eq!(attr.prediction, case.expected.prediction);
        assert_eq!(attr.decision, case.expected.decision);
        if let Some(exp_prob) = case.expected.probability {
            assert!((attr.probability.unwrap_or_default() - exp_prob).abs() < 1e-6);
        }
    }
    assert_eq!(
        canonical_fingerprint_model_id("openai/GPT-5.3-Codex"),
        "gpt-5.3-codex"
    );
    assert_eq!(
        canonical_fingerprint_model_id("gpt-6.1-sol"),
        "gpt-6-astra"
    );
}

#[test]
fn codex_turn_body_contains_expected_envelope() {
    let body_bytes = build_codex_turn_body("acct_1", "gpt-5.4", "high", "", "你好")
        .expect("build codex turn body");
    let body: Value = serde_json::from_slice(&body_bytes).expect("valid json");
    assert_eq!(body["model"], "gpt-5.4");
    assert_eq!(body["reasoning"]["effort"], "high");
    assert!(body["client_metadata"]["x-codex-installation-id"].is_string());
    assert!(body["input"].as_array().is_some_and(|arr| !arr.is_empty()));
}

#[test]
fn helper_parsers_match_expected_behavior() {
    assert!(has_standalone_21("最少需要 21 个糖果"));
    assert!(!has_standalone_21("121 210 3214"));
    assert_eq!(
        extract_fenced_code("```html\n<!doctype html><b>ok</b>\n```", "html"),
        "<!doctype html><b>ok</b>"
    );
    assert_eq!(PELICAN_VARIANTS.len(), 2);
}
