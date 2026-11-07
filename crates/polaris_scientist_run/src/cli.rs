//! The command line of `polaris-scientist`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// What `--version` prints after the command name: the crate version and the
/// source revision, the `polaris_code` version, and the target.
pub const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("POLARIS_SCIENTIST_BUILD_ID"),
    ")\npolaris_code ",
    env!("POLARIS_SCIENTIST_CODE_VERSION"),
    "\ntarget ",
    env!("POLARIS_SCIENTIST_TARGET"),
);

/// Run research campaigns, one agent session per role call.
///
/// Exit status: 0 when the work is done (campaign `completed` or
/// `completed_no_winner`), 1 when it ran and failed (`failed`,
/// `paper_incomplete`), 2 when the command line, a configuration value or a
/// credential is unusable and nothing ran, 4 when a campaign was `interrupted`
/// and can be resumed.
#[derive(Debug, Parser)]
#[command(name = "polaris-scientist", version = VERSION, disable_help_subcommand = true)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start a campaign on a task.
    Run(RunArgs),
    /// Continue an interrupted or failed campaign.
    Resume(ResumeArgs),
    /// Forget the work from one phase on, so a resume repeats it.
    Reset(ResetArgs),
    /// Check a finished paper against its recorded claims. Uses no model, no
    /// network and no credential.
    CheckClaims(CheckClaimsArgs),
    /// Show the configuration a run would use.
    Config(ConfigArgs),
    /// Make one agent call, outside any campaign.
    AgentCall(AgentCallArgs),
}

/// Arguments of `run`.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// The task directory. It is read, never written.
    pub task_dir: PathBuf,
    /// The directory that holds the results of every run. Created if missing.
    #[arg(long)]
    pub results_root: PathBuf,
    /// A configuration file; may be repeated, later files win.
    #[arg(long = "config", value_name = "FILE")]
    pub config: Vec<PathBuf>,
    /// A configuration value, `key=value`; may be repeated, applied after the
    /// files.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub set: Vec<String>,
    /// The most the run may spend, in dollars, for example `13.5`. Required
    /// unless `POLARIS_SCIENTIST_OFFLINE=1`.
    #[arg(long, value_name = "USD")]
    pub max_usd: Option<String>,
    /// A name for the run; letters, digits, `_` and `-`. Defaults to the start
    /// time.
    #[arg(long)]
    pub run_id: Option<String>,
}

/// Arguments of `resume`.
#[derive(Debug, Args)]
pub struct ResumeArgs {
    /// The run directory.
    #[arg(long)]
    pub root: PathBuf,
    /// A configuration value to change, `key=value`; may be repeated. A change
    /// to the shape of the campaign is refused.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub set: Vec<String>,
    /// The most the run may spend in all, in dollars. Required unless
    /// `POLARIS_SCIENTIST_OFFLINE=1`.
    #[arg(long, value_name = "USD")]
    pub max_usd: Option<String>,
}

/// Arguments of `reset`.
#[derive(Debug, Args)]
pub struct ResetArgs {
    /// The run directory.
    #[arg(long)]
    pub root: PathBuf,
    /// The first phase to forget; it and every later phase run again.
    #[arg(long)]
    pub from: String,
}

/// Arguments of `check-claims`.
#[derive(Debug, Args)]
pub struct CheckClaimsArgs {
    /// The directory of the finished paper.
    #[arg(long)]
    pub paper_dir: PathBuf,
    /// Where to write the report as JSON; `-` writes it to standard output.
    #[arg(long)]
    pub out: PathBuf,
}

/// Arguments of `config`.
#[derive(Debug, Args)]
pub struct ConfigArgs {
    /// Print the merged configuration as TOML.
    #[arg(long, required = true)]
    pub print: bool,
    /// A configuration file; may be repeated, later files win.
    #[arg(long = "config", value_name = "FILE")]
    pub config: Vec<PathBuf>,
    /// A configuration value, `key=value`; may be repeated.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub set: Vec<String>,
}

/// Arguments of `agent-call`.
///
/// The call runs on the same agent as a campaign's role calls, with the same
/// tools and the same spend limit, and prints how it ended as JSON.
#[derive(Debug, Args)]
pub struct AgentCallArgs {
    /// The directory the call's tools and commands are confined to.
    #[arg(long)]
    pub results_root: PathBuf,
    /// The call's own folder, inside the results root.
    #[arg(long)]
    pub workspace: PathBuf,
    /// A file holding the role prompt.
    #[arg(long)]
    pub role_prompt: PathBuf,
    /// A file holding the task the agent is asked to do.
    #[arg(long)]
    pub task: PathBuf,
    /// The most turns the agent may take.
    #[arg(long, default_value_t = 50)]
    pub max_turns: u32,
    /// The most the call may spend, in dollars. Required.
    #[arg(long, value_name = "USD")]
    pub max_usd: Option<String>,
    /// How long the call may run, in seconds.
    #[arg(long, default_value_t = 900)]
    pub timeout_seconds: u64,
    /// Let the agent write files in its folder.
    #[arg(long)]
    pub edit: bool,
    /// A shell command pattern the agent may run; may be repeated. With none,
    /// it may run no commands.
    #[arg(long = "command", value_name = "GLOB")]
    pub command: Vec<String>,
}
