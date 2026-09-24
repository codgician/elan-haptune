mod device;
mod elan;
mod error;
mod protocol;

#[cfg(test)]
mod tests;

use clap::{Args, Parser, Subcommand, ValueEnum};
use error::{Error, Result};
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    process::ExitCode,
};

#[derive(Parser)]
#[command(
    name = "elan-haptune",
    version,
    about = "One-shot ELAN2703 haptic touchpad settings",
    after_help = "Raw threshold units are device-specific unsigned 16-bit integers. Both releases must be below press; firmware limits are unknown.\nQueries use SET_FEATURE but do not mutate parameters. No persistence across suspend/reboot is promised."
)]
struct Cli {
    /// Emit schema-versioned JSON (diagnostics remain on stderr)
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Discover via sysfs only; does not open hidraw or query protocol identity
    List,
    /// Check identity and read supported settings and capabilities
    Get(Selection),
    /// Apply explicit changes with readback and best-effort rollback
    Set(Set),
}

#[derive(Args)]
struct Selection {
    /// Absolute hidraw path or stable sysfs: selector printed by list
    #[arg(long)]
    device: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Switch {
    On,
    Off,
}

#[derive(Args)]
struct Set {
    #[command(flatten)]
    selection: Selection,
    /// Query and validate, but never issue parameter writes
    #[arg(long)]
    dry_run: bool,
    /// Press threshold in raw units (unsigned 16-bit integer)
    #[arg(long)]
    press_threshold: Option<u16>,
    /// Regular release in raw units (must be below press)
    #[arg(long)]
    release_threshold: Option<u16>,
    /// Independent drag release in raw units (must be below press)
    #[arg(long)]
    drag_release_threshold: Option<u16>,
    /// Vendor-documented feedback enable bit; preserve all other control bits
    #[arg(long, value_enum)]
    haptics: Option<Switch>,
}

fn execute(command: Command) -> Result<(Value, Option<Error>)> {
    match command {
        Command::List => Ok((json!({"devices": device::discover()?}), None)),
        Command::Get(selection) => {
            let (mut device, mut io) = device::select(selection.device.as_deref())?;
            device.support = "compatible";
            let settings = elan::State::read(&mut io)?.settings();
            Ok((
                json!({"device": device, "settings": settings, "capabilities": elan::capabilities()}),
                None,
            ))
        }
        Command::Set(set) => {
            let update = elan::Update {
                thresholds: [
                    set.press_threshold,
                    set.release_threshold,
                    set.drag_release_threshold,
                ],
                haptics: set.haptics.map(|v| matches!(v, Switch::On)),
            };
            update.validate()?; // Reject empty input before device access.
            let (mut device, mut io) = device::select(set.selection.device.as_deref())?;
            device.support = "compatible";
            let report = elan::apply(&mut io, &update, set.dry_run)?;
            let error = if report.status == "failed" {
                Some(Error::new(
                    1,
                    "update_failed",
                    format!(
                        "{}; rollback: {}",
                        report
                            .error
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                        report
                            .rollback
                            .as_ref()
                            .map(|r| r.status)
                            .unwrap_or("not_attempted")
                    ),
                ))
            } else {
                None
            };
            Ok((json!({"device": device, "update": report}), error))
        }
    }
}

fn emit(json_output: bool, command: &str, data: Value, error: Option<Error>) -> ExitCode {
    let code = error.as_ref().map_or(0, |e| e.exit_code);
    if let Some(e) = &error {
        eprintln!("elan-haptune: {}: {e}", e.kind);
    }
    let document = json!({"schema_version": 1, "command": command, "ok": error.is_none(), "data": data, "error": error});
    let mut out = io::BufWriter::new(io::stdout().lock());
    let output = if json_output {
        serde_json::to_writer(&mut out, &document)
            .map_err(io::Error::other)
            .and_then(|_| writeln!(out))
    } else if document["data"].is_null() {
        Ok(())
    } else {
        human(&mut out, command, &document["data"])
    };
    if let Err(e) = output.and_then(|_| out.flush()) {
        eprintln!("elan-haptune: output: {e}");
        return ExitCode::from(1);
    }
    ExitCode::from(code)
}

fn human(out: &mut impl Write, command: &str, data: &Value) -> io::Result<()> {
    if command == "list" {
        let devices = data["devices"].as_array().expect("device array");
        if devices.is_empty() {
            writeln!(out, "No hidraw devices found.")?;
        }
        for d in devices {
            writeln!(
                out,
                "{}  {}:{}  {}  [{}]\n  {}",
                d["path"].as_str().unwrap_or(""),
                d["vendor_id"].as_str().unwrap_or(""),
                d["product_id"].as_str().unwrap_or(""),
                d["name"].as_str().unwrap_or(""),
                d["support"].as_str().unwrap_or(""),
                d["selector"].as_str().unwrap_or("")
            )?;
        }
        return Ok(());
    }
    writeln!(out, "{}", data["device"]["path"].as_str().unwrap_or(""))?;
    if command == "get" {
        for (key, value) in data["settings"].as_object().expect("settings object") {
            writeln!(out, "{key}: {value}")?;
        }
        writeln!(
            out,
            "Raw unsigned 16-bit units (0..65535); firmware ranges unknown. Both releases must be below press."
        )?;
        writeln!(
            out,
            "Haptics: vendor bit 8 (hardware behavior unverified here). Haptic level: unavailable; physical interpretation unverified."
        )
    } else {
        let report = &data["update"];
        writeln!(out, "{}", report["status"].as_str().unwrap_or(""))?;
        for change in report["planned"].as_array().expect("changes array") {
            writeln!(
                out,
                "  {}: {} -> {} (raw)",
                change["setting"].as_str().unwrap_or(""),
                change["before_raw"],
                change["after_raw"]
            )?;
        }
        if report["status"] == "dry_run" {
            writeln!(out, "Query feature reports only; no parameter mutations.")?;
        }
        if !report["rollback"].is_null() {
            writeln!(
                out,
                "Rollback: {}",
                report["rollback"]["status"].as_str().unwrap_or("unknown")
            )?;
        }
        if report["status"] == "failed" {
            writeln!(
                out,
                "Verified forward changes: {}. Observed after recovery: {}",
                report["verified"].as_array().map_or(0, Vec::len),
                report["observed_after"]
            )?;
            if !report["rollback"]["error"].is_null() {
                writeln!(
                    out,
                    "Rollback detail: {}",
                    report["rollback"]["error"]["message"]
                        .as_str()
                        .unwrap_or("")
                )?;
            }
        }
        Ok(())
    }
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    let json_output = args.iter().any(|arg| arg == "--json");
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e)
            if matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            let _ = e.print();
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            return emit(
                json_output,
                "parse",
                Value::Null,
                Some(Error::invalid(e.to_string())),
            );
        }
    };
    let command = match &cli.command {
        Command::List => "list",
        Command::Get(_) => "get",
        Command::Set(_) => "set",
    };
    match execute(cli.command) {
        Ok((data, error)) => emit(cli.json, command, data, error),
        Err(error) => emit(cli.json, command, Value::Null, Some(error)),
    }
}
