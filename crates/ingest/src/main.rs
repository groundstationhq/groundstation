use clap::Parser;
use groundstation_ingest::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    groundstation_backend::service::init_tracing(
        "groundstation_ingest=info,groundstation_backend=info,warn",
    );
    groundstation_ingest::run(config).await
}
