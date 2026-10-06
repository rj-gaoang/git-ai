//! API client for the independently versioned tool usage endpoint.

use crate::api::client::ApiClient;
use crate::error::GitAiError;
use crate::tool_usage::ToolUsageBatch;

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
        if (200..300).contains(&response.status_code) {
            Ok(())
        } else {
            // Tool usage is optional and must never affect checkpoint/commit.
            Err(GitAiError::Generic(format!(
                "tool usage upload returned status {}",
                response.status_code
            )))
        }
    }

    pub fn upload_tool_usage_at(
        &self,
        url: &str,
        batch: &ToolUsageBatch,
    ) -> Result<(), GitAiError> {
        let response = self.context().post_json_url(url, batch)?;
        if (200..300).contains(&response.status_code) {
            Ok(())
        } else {
            Err(GitAiError::Generic(format!(
                "tool usage upload returned status {}",
                response.status_code
            )))
        }
    }
}
