use gateway_plugin_sdk::{PluginFault, call::management::ManagementResponse, client::TypedReply};
use serde::Serialize;
use serde_json::json;

pub const JSON_CONTENT_TYPE: &str = "application/json";

pub type ApiResult = Result<TypedReply<ManagementResponse>, ApiError>;

#[derive(Debug)]
pub struct ApiError {
    status: u16,
    code: &'static str,
    message: String,
}

impl ApiError {
    #[must_use]
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(400, "invalid_request", message)
    }

    pub fn into_reply(self) -> Result<TypedReply<ManagementResponse>, PluginFault> {
        let body = serde_json::to_vec(&json!({
            "error": {
                "code": self.code,
                "message": self.message,
            }
        }))
        .unwrap_or_else(|_| b"{}".to_vec());
        Ok(TypedReply::new(ManagementResponse {
            status: self.status,
            content_type: JSON_CONTENT_TYPE.to_owned(),
            headers: Vec::new(),
        })
        .with_payload(body))
    }
}

impl From<PluginFault> for ApiError {
    fn from(value: PluginFault) -> Self {
        Self::new(502, "host_callback", value.message)
    }
}

pub fn json_reply<T: Serialize>(value: &T) -> ApiResult {
    let body = serde_json::to_vec(value)
        .map_err(|_| ApiError::new(500, "serialization", "响应编码失败"))?;
    Ok(TypedReply::new(ManagementResponse {
        status: 200,
        content_type: JSON_CONTENT_TYPE.to_owned(),
        headers: Vec::new(),
    })
    .with_payload(body))
}
