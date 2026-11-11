use clap::Parser;
use groundstation_worker::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    groundstation_backend::service::init_tracing(
        "groundstation_worker=info,groundstation_backend=info,warn",
    );
    groundstation_worker::run(config).await
}
