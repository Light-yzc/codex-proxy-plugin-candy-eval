use gateway_plugin_sdk::Manifest;

pub const PLUGIN_ID: &str = "light-yzc.candy-eval";
pub const STATE_NS_SETTINGS: &str = "candy_eval_settings";
pub const STATE_NS_QUEUE: &str = "candy_eval_queue";
pub const STATE_NS_RECORDS: &str = "candy_eval_records";
pub const STATE_NS_DETAIL: &str = "candy_eval_detail";

/// 解析并规范化编译期嵌入的 `plugin.json` 作者清单。
///
/// # Errors
///
/// 清单无法解析、不符合规范化约束或 `plugin_id` 与预期不一致时返回错误。
pub fn manifest() -> Result<Manifest, gateway_plugin_sdk::ManifestError> {
    let manifest = Manifest::from_author_slice(include_bytes!("../../plugin.json"))?;
    if manifest.plugin_id()? != PLUGIN_ID {
        return Err(gateway_plugin_sdk::ManifestError::Invalid);
    }
    Ok(manifest)
}
