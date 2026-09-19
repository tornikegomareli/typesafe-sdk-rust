use serde_json::Value;

use crate::client::TypeSafeClient;
use crate::errors::{Error, Result};
use crate::response::ApiRequest;
use crate::types::{ModelCard, RequestOptions};

/// Access to the Models API resource.
pub struct Models<'a> {
    pub(crate) client: &'a TypeSafeClient,
}

impl Models<'_> {
    /// List the models available to the account.
    pub fn list(&self, options: RequestOptions) -> ApiRequest<Vec<ModelCard>> {
        self.client
            .request(reqwest::Method::GET, "/v1/models", None, options)
            .map(unwrap_models)
    }
}

/// Model list response from `GET /v1/models`.
fn unwrap_models(wire: Option<Value>) -> Result<Vec<ModelCard>> {
    wire.as_ref()
        .and_then(|wire| wire.get("models"))
        .and_then(|models| serde_json::from_value(models.clone()).ok())
        .ok_or_else(|| Error::TypeSafe("Unexpected response shape from GET /v1/models; expected { models: [...] }.".to_string()))
}
