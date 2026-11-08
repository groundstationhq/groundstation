use clap::Parser;
use groundstation_api::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    groundstation_backend::service::init_tracing(
        "groundstation_api=info,groundstation_backend=info,warn",
    );
    groundstation_api::run(config).await
}
