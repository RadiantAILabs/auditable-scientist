//! In-process runtime for Polaris research campaigns.
//!
//! The library holds everything the `polaris-scientist` command is made of:
//!
//! - [`runner::SessionRunner`] runs every role call of a campaign as one
//!   isolated agent session;
//! - [`cli`] and [`commands`] are the command line and what each command does,
//!   written against a [`commands::Host`] so tests can stand in for the model,
//!   the claims check and the process;
//! - [`hardening`] and [`signals`] protect the process's credentials and turn
//!   `SIGTERM` and `SIGINT` into a cooperative stop;
//! - [`exit`] holds the exit codes.

pub mod claims_check;
pub mod cli;
pub mod commands;
pub mod exit;
pub mod hardening;
pub mod runner;
pub mod signals;
