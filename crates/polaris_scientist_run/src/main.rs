//! The `polaris-scientist` command.
//!
//! This file only wires: it protects the process, reads the command line,
//! installs the signal handlers and hands over to [`commands::execute`].

use std::io::Write as _;
use std::process::ExitCode;

use clap::Parser as _;
use polaris_cosmos::flow::StopFlag;
use polaris_scientist_run::cli::Cli;
use polaris_scientist_run::commands::{self, Host, Io};
use polaris_scientist_run::exit::Exit;
use polaris_scientist_run::{hardening, signals};

fn main() -> ExitCode {
    // First, before any credential is read.
    if let Err(error) = hardening::make_undumpable() {
        eprintln!("polaris-scientist: the process cannot be protected from inspection: {error}");
        return Exit::Failed.into();
    }
    if let Err(error) = hardening::ensure_home() {
        eprintln!("polaris-scientist: the home directory cannot be created: {error}");
    }
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .init();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("polaris-scientist: the runtime cannot start: {error}");
            return Exit::Failed.into();
        }
    };
    let exit = runtime.block_on(async {
        let stop = StopFlag::default();
        let watcher = signals::install(stop.clone(), || {
            std::process::exit(i32::from(Exit::Interrupted.code()));
        });
        if let Err(error) = watcher {
            eprintln!("polaris-scientist: signals cannot be handled: {error}");
            return Exit::Failed;
        }
        let host = Host::for_process(stop);
        // Not locked for the whole run: the log writer shares standard error.
        let mut out = std::io::stdout();
        let mut err = std::io::stderr();
        let exit = commands::execute(
            &host,
            cli.command,
            &mut Io {
                out: &mut out,
                err: &mut err,
            },
        )
        .await;
        let _ = out.flush();
        exit
    });
    exit.into()
}
