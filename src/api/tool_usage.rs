//! API client for the independently versioned tool usage endpoint.

use crate::api::client::ApiClient;
use crate::error::GitAiError;
use crate::tool_usage::ToolUsageBatch;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct UploadResponse {
    code: u16,
    data: Option<UploadCounts>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::client::ApiContext;

    #[test]
    fn tool_usage_full_url_checks_business_ack_and_keeps_legacy_route() {
        let mut server = mockito::Server::new();
        let client = ApiClient::new(ApiContext {
            base_url: server.url(),
            auth_token: None,
            api_key: None,
            author_identity: None,
            timeout_secs: Some(5),
        });
        let batch = ToolUsageBatch {
            schema_version: "tool_usage/v1".into(),
            events: vec![],
        };
        for (status, body, ok) in [
            (
                200,
                r#"{"code":200,"data":{"accepted":0,"duplicate":0,"rejected":0}}"#,
                true,
            ),
            (200, r#"{"code":500,"msg":"failed"}"#, false),
            (
                200,
                r#"{"code":200,"data":{"accepted":0,"duplicate":0,"rejected":1}}"#,
                false,
            ),
            (200, r#"{"code":200}"#, false),
            (200, "not-json", false),
            (500, "error", false),
        ] {
            let mock = server
                .mock("POST", "/api/public/worker/tool-usage/upload")
                .match_header("content-type", "application/json")
                .with_status(status)
                .with_body(body)
                .create();
            assert_eq!(
                client
                    .upload_tool_usage_at(
                        &format!("{}/api/public/worker/tool-usage/upload", server.url()),
                        &batch
                    )
                    .is_ok(),
                ok
            );
            mock.assert();
        }
        let mock = server
            .mock("POST", "/worker/tool-usage/upload")
            .with_status(200)
            .with_body(r#"{"code":200,"data":{"accepted":0,"duplicate":0,"rejected":0}}"#)
            .create();
        client.upload_tool_usage(&batch).unwrap();
        mock.assert();
    }
}

#[derive(Debug, Deserialize)]
struct UploadCounts {
    accepted: usize,
    duplicate: usize,
    rejected: usize,
}

fn validate_response(response: crate::http::Response, expected: usize) -> Result<(), GitAiError> {
    if !(200..300).contains(&response.status_code) {
        return Err(GitAiError::Generic(format!(
            "tool usage upload returned status {}",
            response.status_code
        )));
    }
    let result: UploadResponse = serde_json::from_slice(response.as_bytes())?;
    if result.code != 200 {
        return Err(GitAiError::Generic(format!(
            "tool usage upload returned business code {}",
            result.code
        )));
    }
    let counts = result
        .data
        .ok_or_else(|| GitAiError::Generic("tool usage upload missing acknowledgement".into()))?;
    if counts.rejected != 0 || counts.accepted + counts.duplicate != expected {
        return Err(GitAiError::Generic(format!(
            "tool usage upload incomplete: accepted={}, duplicate={}, rejected={}, expected={}",
            counts.accepted, counts.duplicate, counts.rejected, expected
        )));
    }
    Ok(())
}

pub const TOOL_USAGE_REMOTE_URL_ENV: &str = "GIT_AI_TOOL_USAGE_REMOTE_URL";
pub const DEFAULT_TOOL_USAGE_REMOTE_URL: &str =
    "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/worker/tool-usage/upload";

/// Return the complete URL for the ai-cr tool usage endpoint.
/// An environment override is supported for test, staging, and self-hosted
/// deployments. Empty values use the production default.
pub fn remote_url() -> String {
    std::env::var(TOOL_USAGE_REMOTE_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_TOOL_USAGE_REMOTE_URL.to_string())
}

impl ApiClient {
    pub fn upload_tool_usage(&self, batch: &ToolUsageBatch) -> Result<(), GitAiError> {
        let response = self
            .context()
            .post_json("/worker/tool-usage/upload", batch)?;
        validate_response(response, batch.events.len())
    }

    pub fn upload_tool_usage_at(
        &self,
        url: &str,
        batch: &ToolUsageBatch,
    ) -> Result<(), GitAiError> {
        let response = self.context().post_json_url(url, batch)?;
        validate_response(response, batch.events.len())
    }
}
