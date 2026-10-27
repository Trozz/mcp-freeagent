use rmcp::ErrorData as McpError;
use rmcp::model::{GetPromptResult, JsonObject, Prompt};

pub fn list() -> Vec<Prompt> {
    Vec::new()
}

pub fn get(name: &str, _a: Option<&JsonObject>, _p: &str) -> Result<GetPromptResult, McpError> {
    Err(mcp_server_core::prompt::not_found(name))
}
