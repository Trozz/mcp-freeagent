use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    mcp_freeagent::server().main().await
}
