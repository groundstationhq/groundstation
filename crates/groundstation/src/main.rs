//! `groundstation`: the Ground Station CLI.

mod client;
mod render;
mod update;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Parser, Subcommand, ValueEnum};
use groundstation_adapter_claude_code as claude_code;
use groundstation_adapter_codex as codex;
use groundstation_adapter_opencode as opencode;
use groundstation_adapter_pi as pi;
use groundstation_api::{HookEnvelope, SpoolItem};
use groundstation_hooks_json::HookSet;
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
    /// Open the UI in a browser, starting gsd first if needed.
    Ui {
        /// Print the URL instead of opening a browser.
        #[arg(long)]
        no_open: bool,
    },
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
    /// Re-read an agent's transcripts from the start, so trajectories already
    /// stored pick up what this version derives from them (e.g. model call durations).
    Resync { agent: AgentName },
    /// Print config and data locations and the effective configuration.
    Config,
    /// Install the latest release over this install and restart gsd.
    Update {
        /// Only report whether a newer release exists.
        #[arg(long)]
        check: bool,
        /// Install this version instead of the latest, e.g. 0.1.1.
        #[arg(long, value_name = "VERSION")]
        to: Option<String>,
    },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Start gsd in the background.
    Start,
    /// Stop a running gsd.
    Stop,
    /// Stop and start gsd, e.g. after upgrading.
    Restart,
    /// Run gsd in the foreground.
    Run,
    /// Same as `groundstation status`.
    Status,
}

#[derive(Clone, Copy, ValueEnum)]
enum AgentName {
    ClaudeCode,
    Codex,
    /// OpenCode v2 or newer.
    Opencode,
    /// pi (pi.dev).
    Pi,
}

/// How an agent is connected.
enum Integration {
    /// Command hooks in a `hooks.json`-shaped config (Claude Code, Codex).
    Hooks(HookSet),
    /// A generated file the agent loads (OpenCode plugin, pi extension).
    Generated(Generated),
}

/// A generated plugin/extension file and the adapter functions that manage it.
struct Generated {
    /// What the agent calls it: "plugin", "extension".
    kind: &'static str,
    events: &'static [&'static str],
    render: fn(&str, &Path) -> String,
    install: fn(&Path, &str) -> Result<bool>,
    uninstall: fn(&Path) -> Result<bool>,
}

impl AgentName {
    /// The adapter name gsd knows this agent by.
    fn adapter(self) -> &'static str {
        match self {
            Self::ClaudeCode => claude_code::NAME,
            Self::Codex => codex::NAME,
            Self::Opencode => opencode::NAME,
            Self::Pi => pi::NAME,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Opencode => "OpenCode",
            Self::Pi => "pi",
        }
    }

    fn integration(self) -> Integration {
        match self {
            Self::ClaudeCode => Integration::Hooks(claude_code::settings::HOOKS),
            Self::Codex => Integration::Hooks(codex::settings::HOOKS),
            Self::Opencode => Integration::Generated(Generated {
                kind: "plugin",
                events: opencode::settings::EVENTS,
                render: opencode::settings::render,
                install: opencode::settings::install,
                uninstall: opencode::settings::uninstall,
            }),
            Self::Pi => Integration::Generated(Generated {
                kind: "extension",
                events: pi::settings::EVENTS,
                render: pi::settings::render,
                install: pi::settings::install,
                uninstall: pi::settings::uninstall,
            }),
        }
    }

    /// The file `connect` and `disconnect` edit.
    fn config_path(self, scope: Scope) -> Result<PathBuf> {
        match self {
            Self::ClaudeCode => claude_code::settings::settings_path(match scope {
                Scope::User => claude_code::settings::Scope::User,
                Scope::Project => claude_code::settings::Scope::Project,
                Scope::Local => claude_code::settings::Scope::Local,
            }),
            Self::Codex => codex::settings::hooks_path(match scope {
                Scope::User => codex::settings::Scope::User,
                Scope::Project => codex::settings::Scope::Project,
                Scope::Local => {
                    bail!("Codex has no local hooks file; use --scope project or --scope user")
                }
            }),
            Self::Opencode => opencode::settings::plugin_path(match scope {
                Scope::User => opencode::settings::Scope::User,
                Scope::Project => opencode::settings::Scope::Project,
                Scope::Local => {
                    bail!(
                        "OpenCode has no local plugin directory; use --scope project or --scope user"
                    )
                }
            }),
            Self::Pi => pi::settings::extension_path(match scope {
                Scope::User => pi::settings::Scope::User,
                Scope::Project => pi::settings::Scope::Project,
                Scope::Local => {
                    bail!(
                        "pi has no local extension directory; use --scope project or --scope user"
                    )
                }
            }),
        }
    }
}

/// Which config `connect` and `disconnect` edit.
#[derive(Clone, Copy, ValueEnum)]
enum Scope {
    /// Every project on this machine (~/.claude/settings.json, ~/.codex/hooks.json,
    /// ~/.config/opencode/plugins/).
    User,
    /// This project, shared with the team (.claude/settings.json, .codex/hooks.json,
    /// .opencode/plugins/).
    Project,
    /// This project, just for you (.claude/settings.local.json). Claude Code only.
    Local,
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
            agent,
            scope,
            dry_run,
            no_start,
        } => connect(&config, config_path, agent, scope, dry_run, no_start),
        Command::Disconnect { agent, scope } => {
            let path = agent.config_path(scope)?;
            let shown = render::tilde(&path.to_string_lossy());
            match agent.integration() {
                Integration::Hooks(hooks) => {
                    let mut settings = groundstation_hooks_json::read(&path)?;
                    let removed = hooks.uninstall(&mut settings);
                    if removed == 0 {
                        println!("No Ground Station hooks in {shown}");
                    } else {
                        groundstation_hooks_json::write(&path, &settings)?;
                        println!("Removed {removed} hooks from {shown}");
                    }
                }
                Integration::Generated(file) => {
                    if (file.uninstall)(&path)? {
                        println!("Removed the Ground Station {} {shown}", file.kind);
                    } else {
                        println!("No Ground Station {} at {shown}", file.kind);
                    }
                }
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
            action: DaemonAction::Restart,
        } => block_on(async {
            stop_daemon(&config).await?;
            start_daemon(&config, config_path).await
        }),
        Command::Daemon {
            action: DaemonAction::Status,
        }
        | Command::Status => block_on(status(&config)),
        Command::Ui { no_open } => block_on(ui(&config, config_path, no_open)),
        Command::Trajectories { limit, json } => block_on(async {
            let rows = Client::new(&config)?.trajectories(limit).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else if rows.is_empty() {
                println!(
                    "No trajectories yet. Connect an agent: `groundstation connect claude-code|codex|opencode|pi`."
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
        Command::Resync { agent } => block_on(resync(&config, agent)),
        Command::Update { check, to } => block_on(update_cmd(&config, check, to)),
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

fn connect(
    config: &Config,
    config_path: Option<&Path>,
    agent: AgentName,
    scope: Scope,
    dry_run: bool,
    no_start: bool,
) -> Result<ExitCode> {
    let exe = std::env::current_exe().context("locating the groundstation binary")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let path = agent.config_path(scope)?;
    let shown = render::tilde(&path.to_string_lossy());
    match agent.integration() {
        Integration::Hooks(hooks) => {
            let mut settings = groundstation_hooks_json::read(&path)?;
            hooks.install(&mut settings, &hooks.command(&exe))?;
            if dry_run {
                println!("{}", serde_json::to_string_pretty(&settings)?);
                return Ok(ExitCode::SUCCESS);
            }
            groundstation_hooks_json::write(&path, &settings)?;
            println!("Connected {} to Ground Station", agent.title());
            println!("  hooks in  {shown}");
            let events: Vec<&str> = hooks.events.iter().map(|h| h.event).collect();
            println!("  events    {}", events.join(", "));
        }
        Integration::Generated(file) => {
            // OpenCode v1 can't load the v2 plugin; check before writing it.
            let opencode = match agent {
                AgentName::Opencode => Some(opencode_version()?),
                _ => None,
            };
            let source = (file.render)(&config.base_url(), &exe);
            if dry_run {
                print!("{source}");
                return Ok(ExitCode::SUCCESS);
            }
            let replaced = (file.install)(&path, &source)?;
            println!("Connected {} to Ground Station", agent.title());
            match opencode {
                Some(Some(v)) => println!("  opencode  {v}"),
                Some(None) => {
                    println!("  opencode  not found on PATH; install OpenCode 2 or newer")
                }
                None => {}
            }
            let verb = if replaced { "updated" } else { "written" };
            println!("  {:<9} {shown} ({verb})", file.kind);
            println!("  events    {}", file.events.join(", "));
        }
    }
    println!();
    if no_start {
        println!(
            "Start the daemon with `groundstation daemon start`; until then events are spooled to disk."
        );
    } else {
        block_on(ensure_daemon_for(config, config_path, agent))?;
    }
    if let AgentName::Codex = agent {
        println!();
        println!("Codex runs new hooks only after you trust them: open Codex, run /hooks,");
        println!("and trust the `groundstation hook codex` entries (once per change).");
    }
    if let AgentName::Pi = agent {
        println!();
        println!("pi loads extensions at startup: restart running pi sessions, or run /reload.");
        if let Scope::Project = scope {
            println!("Project extensions load only when pi trusts the project.");
        }
    }
    if let AgentName::Opencode = agent {
        println!();
        println!("OpenCode loads plugins when its server starts: restart open sessions,");
        println!("or run `opencode service restart` if the background service is running.");
    }
    println!(
        "New {} sessions will appear in `groundstation trajectories`.",
        agent.title()
    );
    Ok(ExitCode::SUCCESS)
}

/// The installed OpenCode version, or `None` if `opencode` isn't on PATH.
/// Fails for versions older than the plugin API the adapter targets.
fn opencode_version() -> Result<Option<String>> {
    let output = match std::process::Command::new("opencode")
        .arg("--version")
        .output()
    {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("running `opencode --version`"),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    opencode::settings::check_version(&text).map(Some)
}

/// Adapters this CLI can connect that the running gsd doesn't know: it
/// predates them and must be restarted to accept their hooks.
fn missing_adapters(health: &groundstation_api::Health) -> Vec<&'static str> {
    AgentName::value_variants()
        .iter()
        .map(|a| a.adapter())
        .filter(|a| !health.adapters.iter().any(|h| h == a))
        .collect()
}

/// Starts gsd, or restarts it if the running one can't accept `agent`'s hooks.
async fn ensure_daemon_for(
    config: &Config,
    config_path: Option<&Path>,
    agent: AgentName,
) -> Result<ExitCode> {
    if let Ok(health) = Client::new(config)?.health().await
        && !health.adapters.iter().any(|a| a == agent.adapter())
    {
        println!(
            "gsd (pid {}) predates {} support; restarting it.",
            health.pid,
            agent.title()
        );
        stop_daemon(config).await?;
    }
    start_daemon(config, config_path).await
}

async fn start_daemon(config: &Config, config_path: Option<&Path>) -> Result<ExitCode> {
    let client = Client::new(config)?;
    if let Ok(health) = client.health().await {
        println!(
            "gsd is already running (pid {}) on {}",
            health.pid, config.daemon.listen
        );
        let missing = missing_adapters(&health);
        if !missing.is_empty() {
            println!(
                "  it doesn't know {}; run `groundstation daemon restart` to load this build",
                missing.join(", ")
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    gsd::perms::create_dir_all(&config.data_dir())?;
    let log_path = config.log_file();
    let log = gsd::perms::open_options()
        .create(true)
        .append(true)
        .open(&log_path)?;
    gsd::perms::restrict(&log_path, 0o600)?;
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

async fn resync(config: &Config, agent: AgentName) -> Result<ExitCode> {
    let adapter = agent.adapter();
    let r = Client::new(config)?.resync(adapter).await?;
    if r.transcripts == 0 && r.failed.is_empty() && r.unmatched.is_empty() {
        println!("No {adapter} transcripts to re-read.");
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "Re-read {} {adapter} transcript{}; {} event{} updated.",
        r.transcripts,
        if r.transcripts == 1 { "" } else { "s" },
        r.events,
        if r.events == 1 { "" } else { "s" },
    );
    for f in &r.failed {
        println!("  ⚠ {}: {}", render::tilde(&f.path), f.error);
    }
    if !r.unmatched.is_empty() {
        println!(
            "  {} skipped: no stored hook links them to a trajectory",
            r.unmatched.len()
        );
        for path in &r.unmatched {
            println!("    {}", render::tilde(path));
        }
    }
    Ok(if r.failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Opens the embedded UI, starting gsd if it isn't running.
async fn ui(config: &Config, config_path: Option<&Path>, no_open: bool) -> Result<ExitCode> {
    let client = Client::new(config)?;
    let health = match client.health().await {
        Ok(h) => h,
        Err(_) => {
            let code = start_daemon(config, config_path).await?;
            if code != ExitCode::SUCCESS {
                return Ok(code);
            }
            client.health().await?
        }
    };
    let url = format!("{}/", config.base_url());
    if !health.ui {
        println!(
            "gsd {} was built without the UI; the API is up at {url}v1/health.",
            health.version
        );
        println!(
            "Install a release build (https://groundstation.sh/install) or build ui/ and rebuild gsd."
        );
        return Ok(ExitCode::FAILURE);
    }
    println!("Ground Station UI: {url}");
    if !no_open && let Err(err) = open::that_detached(&url) {
        println!("couldn't open a browser ({err}); open the URL yourself.");
    }
    Ok(ExitCode::SUCCESS)
}

/// `groundstation update [--check] [--to VERSION]`.
async fn update_cmd(config: &Config, check: bool, to: Option<String>) -> Result<ExitCode> {
    let layout = update::Layout::from_env();
    let current =
        update::Version::parse(update::CURRENT).context("parsing this build's version")?;
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("groundstation/", env!("CARGO_PKG_VERSION")))
        .build()?;

    let latest = update::latest(&http, &layout).await?;
    let wanted = match &to {
        Some(v) => update::Version::parse(v).with_context(|| format!("not a version: {v:?}"))?,
        None => latest.clone(),
    };
    if check {
        match latest.cmp(&current) {
            std::cmp::Ordering::Greater => {
                println!("groundstation {current} → {latest} available; run `groundstation update`")
            }
            std::cmp::Ordering::Equal => println!("groundstation {current} is the latest release"),
            std::cmp::Ordering::Less => {
                println!("groundstation {current} is ahead of the latest release ({latest})")
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    if to.is_none() && wanted <= current {
        if wanted == current {
            println!("groundstation {current} is already the latest release");
        } else {
            println!(
                "groundstation {current} is ahead of the latest release ({latest}); use --to to install it anyway"
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    let exe = std::env::current_exe().context("locating the groundstation binary")?;
    match update::detect(&exe, &layout) {
        update::Install::Managed => {}
        update::Install::Cargo => {
            println!("groundstation {current} → {wanted}");
            println!("This groundstation was installed with cargo. Upgrade it with:");
            println!(
                "  cargo install --git https://github.com/{} --tag v{wanted} gsd groundstation",
                update::REPO
            );
            return Ok(ExitCode::FAILURE);
        }
        update::Install::Other => {
            println!("groundstation {current} → {wanted}");
            println!(
                "{} wasn't put there by the Ground Station installer, so update won't replace it.",
                render::tilde(&exe.to_string_lossy())
            );
            println!("Upgrade the way it was installed, or run the installer:");
            println!("  curl -fsSL https://groundstation.sh/install | sh");
            return Ok(ExitCode::FAILURE);
        }
    }

    let say = |line: &str| println!("==> {line}");
    let release_dir = update::install(&http, &layout, &wanted, &say).await?;
    update::link(&layout, &release_dir)?;
    say(&format!(
        "Linked {} → {}",
        render::tilde(&layout.bin.to_string_lossy()),
        render::tilde(&release_dir.to_string_lossy())
    ));

    // The daemon still runs the old binary until restarted. Let the new CLI
    // do it, so the gsd it starts is the one just installed.
    if Client::new(config)?.health().await.is_ok() {
        let mut cmd = std::process::Command::new(release_dir.join("groundstation"));
        cmd.args(["daemon", "restart"]);
        if let Some(path) = std::env::var_os("GROUNDSTATION_CONFIG") {
            cmd.arg("--config").arg(path);
        }
        let status = cmd.status().context("restarting gsd with the new binary")?;
        if !status.success() {
            bail!("the new groundstation could not restart gsd ({status})");
        }
    }
    println!();
    println!("groundstation {current} → {wanted} installed.");
    Ok(ExitCode::SUCCESS)
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
    if health.version != update::CURRENT {
        println!(
            "  ⚠ this CLI is {}; run `groundstation daemon restart` so gsd matches",
            update::CURRENT
        );
    }
    let missing = missing_adapters(&health);
    if !missing.is_empty() {
        println!(
            "  ⚠ outdated: doesn't accept {} hooks (they wait on disk); run `groundstation daemon restart`",
            missing.join(", ")
        );
    }
    println!("  schema    {}", health.schema);
    if health.ui {
        println!("  ui        {}/", config.base_url());
    }
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
