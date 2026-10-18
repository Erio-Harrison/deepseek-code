use std::sync::{Arc, Mutex};

use deepseek_code::agent::Agent;
use deepseek_code::deepseek::DeepSeekClient;
use deepseek_code::settings;
use deepseek_code::stdio_transport::spawn_reader;
use deepseek_code::tools::ToolRegistry;
use deepseek_code::tools::bash::BashTool;
use deepseek_code::tools::read_file::ReadFileTool;
use deepseek_code::tools::write_file::WriteFileTool;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .map_err(|_| anyhow::anyhow!("DEEPSEEK_API_KEY environment variable is required"))?;
    let model = std::env::var("DEEPSEEK_MODEL").unwrap_or_else(|_| "deepseek-chat".to_string());

    // The workspace is always the process's cwd (the host is expected to launch
    // us there), same assumption the sandboxed tools already make.
    let workspace = std::env::current_dir()?;
    let grants = Arc::new(Mutex::new(settings::load_initial_grants(&workspace)));

    let mut tools = ToolRegistry::new();
    tools.register(Box::new(ReadFileTool));
    tools.register(Box::new(WriteFileTool::new(grants.clone())));
    tools.register(Box::new(BashTool::new(grants.clone())));

    let client = DeepSeekClient::new(api_key, model);
    let router = spawn_reader(tokio::io::stdin());
    let mut agent = Agent::new(client, tools, tokio::io::stdout(), router.pending, grants);

    // On error the host still holds stdin open, and dropping the runtime would
    // wait forever on the blocking stdin read. Exit explicitly instead.
    if let Err(err) = agent.run(router.user_rx).await {
        eprintln!("deepseek-code: {err:#}");
        std::process::exit(1);
    }
    Ok(())
}
