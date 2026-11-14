use codex_proxy_plugin_candy_eval::{PLUGIN_ID, manifest, plugin};
use gateway_plugin_sdk::client::{PluginSession, SessionConfig, SessionError};
use gateway_plugin_sdk::{CallBudgets, Stage};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut session = PluginSession::accept(
        tokio::io::stdin(),
        tokio::io::stdout(),
        SessionConfig::default(),
    )
    .await?;
    let manifest = manifest()?;
    if session.handshake().plugin_id != PLUGIN_ID
        || session.handshake().contributes != manifest.contributes
        || !session
            .handshake()
            .configuration
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
    {
        return Err(SessionError::Handshake.into());
    }
    session.set_call_budgets(CallBudgets::from([
        (Stage::Maintenance, 600_000),
        (Stage::Management, 120_000),
    ]))?;
    session.run(plugin()?).await?;
    Ok(())
}
