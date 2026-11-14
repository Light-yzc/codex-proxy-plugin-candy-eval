use std::sync::Arc;
use std::time::{Duration, Instant};

use gateway_plugin_sdk::client::{AuthorError, ComposedPlugin, PluginBuilder, methods};

use crate::coordinator::EvalCoordinator;
use crate::management;

/// 组装插件处理器：注册 Management v2 管理页面/API 与 Maintenance v1 后台定时队列任务。
///
/// # Errors
///
/// 清单声明与注册处理器不匹配时返回错误。
pub fn plugin() -> Result<ComposedPlugin, AuthorError> {
    let coordinator = Arc::new(EvalCoordinator::default());
    let mgmt_coord = Arc::clone(&coordinator);
    let recon_coord = Arc::clone(&coordinator);

    PluginBuilder::from_json(include_bytes!("../../plugin.json"))?
        .management(management::registration(), move |call| {
            let coord = Arc::clone(&mgmt_coord);
            async move { management::handle(&coord, call).await }
        })?
        .on(methods::RECONCILE, move |call| {
            let coord = Arc::clone(&recon_coord);
            async move {
                let budget = call
                    .host
                    .remaining_budget()
                    .unwrap_or_else(|| Duration::from_secs(540));
                let deadline = Instant::now() + budget;
                coord.reconcile(&call.host, deadline).await?;
                Ok(gateway_plugin_sdk::client::TypedReply::new(
                    gateway_plugin_sdk::client::Empty {},
                ))
            }
        })?
        .build()
}
