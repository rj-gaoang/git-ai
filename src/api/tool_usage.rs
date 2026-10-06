//! API client for the independently versioned tool usage endpoint.

use crate::api::client::ApiClient;
use crate::error::GitAiError;
use crate::tool_usage::ToolUsageBatch;

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
}
