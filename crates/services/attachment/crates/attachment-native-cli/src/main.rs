#![forbid(unsafe_code)]

use attachment_native_host::{AttachmentHost, AttachmentHostConfig, ProvidedAttachment};
use attachment_native_types::{
    AttachmentGraph, AttachmentReceipt, CanonicalArtifact, PreparationPlan, TargetCapabilities,
};
use clap::{Parser, Subcommand};
use serde::Serialize;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Parser)]
#[command(name = "attachment-native")]
#[command(about = "Safe attachment inspection and model-preparation oracle")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect and canonicalize one caller-selected local file.
    Inspect {
        path: PathBuf,
        #[arg(long)]
        media_type: Option<String>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Inspect, canonicalize, and plan delivery for an exact target profile.
    Plan {
        path: PathBuf,
        #[arg(long)]
        target: PathBuf,
        #[arg(long)]
        media_type: Option<String>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

#[derive(Debug, Serialize)]
struct InspectOutput {
    graph: AttachmentGraph,
    artifacts: Vec<CanonicalArtifact>,
    receipt: AttachmentReceipt,
}

#[derive(Debug, Serialize)]
struct PlanOutput {
    graph: AttachmentGraph,
    artifacts: Vec<CanonicalArtifact>,
    plan: PreparationPlan,
    receipt: AttachmentReceipt,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "{error}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Inspect {
            path,
            media_type,
            config,
        } => {
            let config = load_config(config.as_deref())?;
            let input =
                open_attachment(&path, media_type, config.inspection.limits.max_root_bytes)?;
            let host = AttachmentHost::new(config).map_err(|error| error.to_string())?;
            let output = host
                .inspect_and_canonicalize(input)
                .map_err(|error| error.to_string())?;
            print_json(&InspectOutput {
                graph: output.bundle.graph,
                artifacts: output.bundle.artifacts,
                receipt: output.receipt,
            })
        }
        Command::Plan {
            path,
            target,
            media_type,
            config,
        } => {
            let config = load_config(config.as_deref())?;
            let input =
                open_attachment(&path, media_type, config.inspection.limits.max_root_bytes)?;
            let target: TargetCapabilities = load_json(&target, "target capabilities")?;
            let host = AttachmentHost::new(config).map_err(|error| error.to_string())?;
            let output = host
                .process(input, &target)
                .map_err(|error| error.to_string())?;
            print_json(&PlanOutput {
                graph: output.bundle.graph,
                artifacts: output.bundle.artifacts,
                plan: output.plan,
                receipt: output.receipt,
            })
        }
    }
}

fn load_config(path: Option<&Path>) -> Result<AttachmentHostConfig, String> {
    path.map_or_else(
        || Ok(AttachmentHostConfig::default()),
        |path| load_json(path, "attachment host configuration"),
    )
}

fn load_json<T: serde::de::DeserializeOwned>(path: &Path, label: &str) -> Result<T, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("failed to open {label} {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read {label} {}: {error}", path.display()))?;
    let length = u64::try_from(bytes.len())
        .map_err(|_| format!("{label} {} length overflowed", path.display()))?;
    if length > MAX_CONFIG_BYTES {
        return Err(format!(
            "{label} {} exceeds the {MAX_CONFIG_BYTES}-byte configuration limit",
            path.display()
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("failed to parse {label} {}: {error}", path.display()))
}

fn open_attachment(
    path: &Path,
    media_type: Option<String>,
    max_root_bytes: u64,
) -> Result<ProvidedAttachment, String> {
    let file = File::open(path)
        .map_err(|error| format!("failed to open attachment {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("failed to inspect attachment {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "attachment {} is not a regular file",
            path.display()
        ));
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("attachment")
        .to_string();
    ProvidedAttachment::read_bounded(name, media_type, file, max_root_bytes)
        .map_err(|error| error.to_string())
}

fn print_json(value: &impl Serialize) -> Result<(), String> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer_pretty(&mut lock, value)
        .map_err(|error| format!("failed to encode output: {error}"))?;
    writeln!(lock).map_err(|error| format!("failed to write output: {error}"))
}
