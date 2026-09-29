use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

/// Ground Station local daemon: collects agent telemetry on this machine.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Config file [default: ~/.config/groundstation/config.toml]
    #[arg(long)]
    config: Option<PathBuf>,
    /// Address to listen on, overriding the config file.
    #[arg(long)]
    listen: Option<SocketAddr>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    gsd::init_tracing();
    let mut config = gsd::config::Config::load(args.config.as_deref())?;
    if let Some(listen) = args.listen {
        config.daemon.listen = listen;
    }
    gsd::run(config).await
}
