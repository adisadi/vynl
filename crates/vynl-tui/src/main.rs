//! Binary entry point for `vynl`.

use std::env;
use std::process::ExitCode;

use vynl_core::config::AppConfig;
use vynl_system::ipc::{IpcClient, IpcCommand};
use vynl_system::paths::AppPaths;
use vynl_tui::app::VynlApp;
use vynl_tui::auth::run_auth_command;
use vynl_tui::storage;
use vynl_tui::{
    CliAction, detect_env_protocol, format_doctor_report, parse_args, resolve_env_protocol,
    usage_text,
};

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
        Ok(CliAction::Auth(opts)) => match run_auth_command(&opts) {
            Ok(msg) => {
                println!("{msg}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("error: {err}");
                ExitCode::FAILURE
            }
        },
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
            let paths = AppPaths::resolve();
            let logger = vynl_tui::logging::FileLogger::new(&paths.log_file);
            let config = storage::load_config(&paths).unwrap_or_else(|err| {
                logger.warn("config", &format!("{err} — falling back to defaults"));
                eprintln!("warning: {err} — falling back to default config");
                AppConfig::default()
            });
            let proto = resolve_env_protocol(config.images.protocol);
            logger.info(
                "startup",
                &format!(
                    "launching vynl {} (mode={}, protocol={}, theme={})",
                    env!("CARGO_PKG_VERSION"),
                    if demo { "demo" } else { "live" },
                    proto.as_str(),
                    config.ui.theme
                ),
            );
            let app = if demo {
                VynlApp::new_demo_with_config(config, paths, proto)
            } else {
                VynlApp::new_live(config, paths, proto)
            };
            match app.run_interactive() {
                Ok(()) => {
                    logger.info("shutdown", "exited cleanly");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    logger.error("runtime", &format!("failed to run TUI: {err}"));
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
