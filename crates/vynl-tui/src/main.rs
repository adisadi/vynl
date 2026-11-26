//! Binary entry point for `vynl`.

use std::env;
use std::process::ExitCode;

use vynl_core::state::{ActiveModal, AppEvent, reduce};
use vynl_system::ipc::{IpcClient, IpcCommand};
use vynl_tui::app::VynlApp;
use vynl_tui::{CliAction, detect_env_protocol, format_doctor_report, parse_args, usage_text};

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
        Ok(CliAction::RenderDemo {
            cols,
            rows,
            protocol,
            plain,
        }) => {
            let proto = protocol.unwrap_or_else(detect_env_protocol);
            let mut app = VynlApp::new_demo(proto);
            let frame = app.render_snapshot(cols, rows);
            if plain {
                println!("{}", frame.to_plain_text());
            } else {
                print!("{}", frame.diff_ansi(None));
            }
            ExitCode::SUCCESS
        }
        Ok(CliAction::LaunchTui { demo }) => {
            let proto = detect_env_protocol();
            let mut app = VynlApp::new_demo(proto);
            if !demo && app.config.spotify.client_id.is_none() {
                let _ = reduce(
                    &mut app.state,
                    AppEvent::OpenModal(ActiveModal::SetupWizard {
                        client_id_input: String::new(),
                    }),
                );
            }
            match app.run_interactive() {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("error: failed to run TUI: {err}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(CliAction::Ipc(cmd)) => {
            let json_mode = matches!(cmd, IpcCommand::Status { json: true });
            let client = IpcClient::default_client();
            match client.send(&cmd) {
                Ok(resp) => {
                    println!("{}", resp.format_cli_output(json_mode));
                    if resp.ok {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::FAILURE
                    }
                }
                Err(err) => {
                    eprintln!("error: could not reach running vynl instance: {err}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
