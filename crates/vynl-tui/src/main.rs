//! Binary entry point for `vynl`.

use std::env;
use std::process::ExitCode;

use vynl_tui::{CliAction, format_doctor_report, parse_args, usage_text};

fn main() -> ExitCode {
    match parse_args(env::args().skip(1)) {
        Ok(CliAction::Help) => {
            println!("{}", usage_text());
            ExitCode::SUCCESS
        }
        Ok(CliAction::Version) => {
            println!("vynl {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(CliAction::Doctor) => {
            println!("{}", format_doctor_report());
            ExitCode::SUCCESS
        }
        Ok(CliAction::LaunchTui) => {
            println!(
                "vynl {} — Phase 0 workspace bootstrap ready.\n\
                 Run `vynl doctor` for terminal diagnostics or see docs/adr/ for the Phase 1 implementation plan.",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        Ok(CliAction::Ipc(cmd)) => {
            eprintln!(
                "IPC command {cmd:?} requires a running vynl instance (implemented in Phase 3)."
            );
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
