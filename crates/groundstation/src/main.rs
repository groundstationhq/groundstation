//! `groundstation`: the Ground Station CLI.

mod client;
mod render;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Parser, Subcommand, ValueEnum};
use groundstation_adapter_claude_code::{self as claude_code, settings as claude_settings};
use gsd::api::{HookEnvelope, SpoolItem};
use gsd::config::Config;
use uuid::Uuid;

use crate::client::Client;

#[derive(Parser)]
#[command(version, about = "Ground Station: observability for AI agents")]
struct Cli {
    /// Config file [default: ~/.config/groundstation/config.toml]
    #[arg(long, global = true, env = "GROUNDSTATION_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Send an agent's telemetry to Ground Station.
    Connect {
        agent: AgentName,
        #[arg(long, value_enum, default_value = "user")]
        scope: Scope,
        /// Print the resulting settings instead of writing them.
        #[arg(long)]
        dry_run: bool,
        /// Don't start gsd after connecting.
        #[arg(long)]
        no_start: bool,
    },
    /// Stop sending an agent's telemetry to Ground Station.
    Disconnect {
        agent: AgentName,
        #[arg(long, value_enum, default_value = "user")]
        scope: Scope,
    },
    /// Forward a hook payload from stdin to gsd. Invoked by the agent, not by hand.
    #[command(hide = true)]
    Hook { agent: AgentName },
    /// Manage the local daemon (gsd).
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Show daemon and collection status.
    Status,
    /// List recent trajectories.
    #[command(alias = "ls")]
    Trajectories {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Show one trajectory as a timeline.
    Show {
        /// Trajectory id, or a unique prefix of one.
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Print config and data locations and the effective configuration.
    Config,
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Start gsd in the background.
    Start,
    /// Stop a running gsd.
    Stop,
    /// Run gsd in the foreground.
    Run,
    /// Same as `groundstation status`.
    Status,
}

#[derive(Clone, Copy, ValueEnum)]
enum AgentName {
    ClaudeCode,
}

impl AgentName {
    /// The adapter name gsd knows this agent by.
    fn adapter(self) -> &'static str {
        match self {
            Self::ClaudeCode => claude_code::NAME,
        }
    }
}

/// Which Claude Code settings file `connect` and `disconnect` edit.
#[derive(Clone, Copy, ValueEnum)]
enum Scope {
    /// ~/.claude/settings.json: every project on this machine.
    User,
    /// .claude/settings.json: this project, committed for the whole team.
    Project,
    /// .claude/settings.local.json: this project, just for you.
    Local,
}

impl From<Scope> for claude_settings::Scope {
    fn from(scope: Scope) -> Self {
        match scope {
            Scope::User => Self::User,
            Scope::Project => Self::Project,
            Scope::Local => Self::Local,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // The hook runs inside the agent's loop: it must stay silent (stdout can
    // end up in the model's context) and must never fail the agent.
    if let Command::Hook { agent } = cli.command {
        if let Err(e) = hook(cli.config, agent)
            && std::env::var_os("GROUNDSTATION_DEBUG").is_some()
        {
            eprintln!("groundstation hook: {e:#}");
        }
        return ExitCode::SUCCESS;
    }

    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let config_path = cli.config.as_deref();
    let config = Config::load(config_path)?;
    match cli.command {
        Command::Connect {
            agent: AgentName::ClaudeCode,
            scope,
            dry_run,
            no_start,
        } => connect_claude_code(&config, config_path, scope, dry_run, no_start),
        Command::Disconnect {
            agent: AgentName::ClaudeCode,
            scope,
        } => {
            let path = claude_settings::settings_path(scope.into())?;
            let mut settings = claude_settings::read(&path)?;
            let removed = claude_settings::uninstall(&mut settings);
            if removed == 0 {
                println!(
                    "No Ground Station hooks in {}",
                    render::tilde(&path.to_string_lossy())
                );
            } else {
                claude_settings::write(&path, &settings)?;
                println!(
                    "Removed {removed} hooks from {}",
                    render::tilde(&path.to_string_lossy())
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Hook { .. } => unreachable!("handled in main"),
        Command::Daemon {
            action: DaemonAction::Run,
        } => {
            gsd::init_tracing();
            tokio::runtime::Runtime::new()?.block_on(gsd::run(config))?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Daemon {
            action: DaemonAction::Start,
        } => block_on(start_daemon(&config, config_path)),
        Command::Daemon {
            action: DaemonAction::Stop,
        } => block_on(stop_daemon(&config)),
        Command::Daemon {
            action: DaemonAction::Status,
        }
        | Command::Status => block_on(status(&config)),
        Command::Trajectories { limit, json } => block_on(async {
            let rows = Client::new(&config)?.trajectories(limit).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else if rows.is_empty() {
                println!(
                    "No trajectories yet. Connect an agent with `groundstation connect claude-code`."
                );
            } else {
                print!("{}", render::trajectory_table(&rows));
            }
            Ok(ExitCode::SUCCESS)
        }),
        Command::Show { id, json } => block_on(async {
            let detail = Client::new(&config)?.trajectory(&id).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&detail)?);
            } else {
                print!("{}", render::trajectory(&detail));
            }
            Ok(ExitCode::SUCCESS)
        }),
        Command::Config => {
            let mut shown = config.clone();
            // Fail here, not at daemon start, on a bad exclude or env glob.
            gsd::privacy::Privacy::new(&config)?;
            if shown.transport.token.is_some() {
                shown.transport.token = Some("********".into());
            }
            println!("# config file: {}", gsd::config::config_file().display());
            println!("# data dir:    {}", config.data_dir().display());
            print!("{}", toml::to_string_pretty(&shown)?);
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn block_on(f: impl Future<Output = Result<ExitCode>>) -> Result<ExitCode> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(f)
}

fn hook(config_path: Option<PathBuf>, agent: AgentName) -> Result<()> {
    let observed_at = Utc::now();
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let payload: serde_json::Value =
        serde_json::from_slice(&input).context("hook payload is not JSON")?;
    let config = Config::load(config_path.as_deref()).unwrap_or_default();
    let envelope = HookEnvelope {
        id: Uuid::now_v7(),
        observed_at,
        payload,
    };

    let adapter = agent.adapter();
    let sent = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            Client::for_hooks(&config)?
                .post(&format!("/v1/adapters/{adapter}"), &envelope)
                .await
        });
    if let Err(e) = sent {
        // gsd is down or busy: keep the payload for it to pick up later.
        let item = SpoolItem::Hook {
            adapter: adapter.to_string(),
            envelope,
        };
        gsd::spool::write(&config.spool_dir(), &item)
            .with_context(|| format!("spooling after send failed ({e:#})"))?;
    }
    Ok(())
}

fn connect_claude_code(
    config: &Config,
    config_path: Option<&Path>,
    scope: Scope,
    dry_run: bool,
    no_start: bool,
) -> Result<ExitCode> {
    let exe = std::env::current_exe().context("locating the groundstation binary")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let path = claude_settings::settings_path(scope.into())?;
    let mut settings = claude_settings::read(&path)?;
    claude_settings::install(&mut settings, &claude_settings::hook_command(&exe))?;
    if dry_run {
        println!("{}", serde_json::to_string_pretty(&settings)?);
        return Ok(ExitCode::SUCCESS);
    }
    let backup = claude_settings::write(&path, &settings)?;

    println!("Connected Claude Code to Ground Station");
    println!("  settings  {}", render::tilde(&path.to_string_lossy()));
    if let Some(backup) = backup {
        println!("  backup    {}", render::tilde(&backup.to_string_lossy()));
    }
    let events: Vec<&str> = claude_settings::HOOK_EVENTS
        .iter()
        .map(|(e, _)| *e)
        .collect();
    println!("  hooks     {}", events.join(", "));
    println!();
    if no_start {
        println!(
            "Start the daemon with `groundstation daemon start`; until then events are spooled to disk."
        );
    } else {
        block_on(start_daemon(config, config_path))?;
    }
    println!("New Claude Code sessions will appear in `groundstation trajectories`.");
    Ok(ExitCode::SUCCESS)
}

async fn start_daemon(config: &Config, config_path: Option<&Path>) -> Result<ExitCode> {
    let client = Client::new(config)?;
    if let Ok(health) = client.health().await {
        println!(
            "gsd is already running (pid {}) on {}",
            health.pid, config.daemon.listen
        );
        return Ok(ExitCode::SUCCESS);
    }

    std::fs::create_dir_all(config.data_dir())?;
    let log_path = config.log_file();
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let exe = std::env::current_exe()?;
    // Prefer the dedicated `gsd` binary installed alongside this one.
    let sibling = exe.with_file_name(format!("gsd{}", std::env::consts::EXE_SUFFIX));
    let mut cmd = if sibling.is_file() {
        std::process::Command::new(sibling)
    } else {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["daemon", "run"]);
        cmd
    };
    if let Some(path) = config_path {
        cmd.arg("--config").arg(std::path::absolute(path)?);
    }
    cmd.stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().context("starting gsd")?;

    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(health) = client.health().await {
            println!(
                "gsd started (pid {}) on {}",
                health.pid, config.daemon.listen
            );
            println!("  logs  {}", render::tilde(&log_path.to_string_lossy()));
            return Ok(ExitCode::SUCCESS);
        }
        if let Some(status) = child.try_wait()? {
            bail!("gsd exited with {status}; see {}", log_path.display());
        }
    }
    bail!("gsd did not become ready; see {}", log_path.display())
}

async fn stop_daemon(config: &Config) -> Result<ExitCode> {
    let client = Client::new(config)?;
    if client.health().await.is_err() {
        println!("gsd is not running");
        return Ok(ExitCode::SUCCESS);
    }
    client.shutdown().await?;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if client.health().await.is_err() {
            println!("gsd stopped");
            return Ok(ExitCode::SUCCESS);
        }
    }
    bail!("gsd did not stop within 5s")
}

async fn status(config: &Config) -> Result<ExitCode> {
    let health = match Client::new(config)?.health().await {
        Ok(h) => h,
        Err(_) => {
            println!("gsd is not running on {}", config.daemon.listen);
            let spooled = gsd::spool::pending(&config.spool_dir()).len();
            if spooled > 0 {
                println!("  {spooled} payloads spooled, waiting for gsd");
            }
            println!("Start it with `groundstation daemon start`.");
            return Ok(ExitCode::FAILURE);
        }
    };
    println!(
        "gsd {} running (pid {}) on {}",
        health.version, health.pid, config.daemon.listen
    );
    println!("  schema    {}", health.schema);
    println!("  data      {}", render::tilde(&health.data_dir));
    println!(
        "  stored    {} trajectories, {} events",
        health.trajectories, health.events
    );
    println!("  spool     {} pending", health.spool_pending);
    match health.upload.endpoint {
        Some(endpoint) => println!(
            "  transport {} → {endpoint} ({} pending)",
            health.upload.mode, health.upload.pending
        ),
        None => println!(
            "  transport {} (nothing leaves this machine)",
            health.upload.mode
        ),
    }
    Ok(ExitCode::SUCCESS)
}
