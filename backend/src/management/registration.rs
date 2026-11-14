use gateway_plugin_sdk::call::management::{
    ManagementPage, ManagementRegistration, ManagementResource, ManagementRoute,
};

use super::response::JSON_CONTENT_TYPE;

#[must_use]
pub fn registration() -> ManagementRegistration {
    let get = |path: &str| ManagementRoute {
        method: "GET".to_owned(),
        path: path.to_owned(),
        request_content_types: Vec::new(),
        response_content_types: vec![JSON_CONTENT_TYPE.to_owned()],
    };
    let post = |path: &str| ManagementRoute {
        method: "POST".to_owned(),
        path: path.to_owned(),
        request_content_types: vec![JSON_CONTENT_TYPE.to_owned()],
        response_content_types: vec![JSON_CONTENT_TYPE.to_owned()],
    };
    ManagementRegistration {
        routes: vec![
            get("api/status"),
            get("api/records"),
            post("api/record-detail"),
            post("api/settings"),
            post("api/run"),
            post("api/step"),
            post("api/stop"),
            post("api/records/delete"),
        ],
        resources: ["web/index.html", "web/app.js", "web/app.css"]
            .into_iter()
            .map(|path| ManagementResource {
                path: path.to_owned(),
                public: path == "web/app.css",
            })
            .collect(),
        pages: vec![ManagementPage {
            id: "candy-eval".to_owned(),
            title: "降智检测".to_owned(),
            description: Some("糖果题、lm-detector 模型指纹与画鹈鹕检测".to_owned()),
            entry: "web/index.html".to_owned(),
            icon: None,
        }],
        callbacks: Vec::new(),
    }
}
