#[tokio::main]
async fn main() {
    use rig_tap::{TelemetryHook, TelemetryHookConfig};
    use rig::agent::PromptHook;
    let hook = TelemetryHook::<rig::providers::openai::CompletionModel>::new(TelemetryHookConfig::new("gpt-4", "conv"));
    hook.on_tool_call("fetch", None, "call-1", "{}").await;
    println!("Done");
}
