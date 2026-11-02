//! `shulker`: load a Bedrock pack, add-on or world and print every diagnostic, rustc style.

mod bundle;
mod help;
mod load;
mod locate;
mod names;
mod render;
mod status;
mod vanilla;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use bedrock::addon::diagnostic::{Diagnostic, DiagnosticCode, Severity};
use bedrock::addon::pack::LinkOptions;
use clap::{Parser, ValueEnum};

use crate::render::{Renderer, Tally};

#[cfg(vanilla)]
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (vanilla files of ",
    env!("SHULKER_VANILLA_LABEL"),
    ")"
);
#[cfg(not(vanilla))]
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (no vanilla files embedded)");

/// Check a Minecraft Bedrock pack, add-on or world and report what the game would complain about.
#[derive(Debug, Parser)]
#[command(version = VERSION, about)]
struct Cli {
    /// Pack folders, folders of packs, a world folder, or archives (.mcpack, .mcaddon, .mcworld,
    /// .zip). Everything given is checked together, so a pack can depend on another one given
    /// beside it.
    #[arg(required = true, value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// The lowest severity to print. `info` adds findings the game itself does not report:
    /// lints, and messages it logs only in some situations.
    #[arg(long, value_enum, default_value_t = MinSeverity::Warning)]
    min_severity: MinSeverity,

    /// Skip the link pass (references between definitions: textures, geometries, loot tables, …).
    #[arg(long)]
    no_link: bool,

    /// Check without the embedded vanilla packs: references to vanilla content then go
    /// unresolved.
    #[arg(long)]
    no_vanilla: bool,

    /// No status lines, only the diagnostics.
    #[arg(short, long)]
    quiet: bool,

    /// Leave out the link pass's lints (findings the game itself does not report).
    #[arg(long)]
    no_lints: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum MinSeverity {
    Debug,
    Info,
    Warning,
    Error,
}

impl From<MinSeverity> for Severity {
    fn from(value: MinSeverity) -> Self {
        match value {
            MinSeverity::Debug => Severity::Debug,
            MinSeverity::Info => Severity::Info,
            MinSeverity::Warning => Severity::Warning,
            MinSeverity::Error => Severity::Error,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(tally) if tally.errors > 0 => ExitCode::FAILURE,
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            render::fatal(&error);
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> anyhow::Result<Tally> {
    let start = Instant::now();
    status::set_quiet(cli.quiet);
    let loaded = load::load(&cli.paths, !cli.no_vanilla)?;

    // Linked before anything is printed, so the status lines come first.
    let links = (!cli.no_link).then(|| {
        status::status("Linking", "references between definitions");
        let options = LinkOptions {
            lints: !cli.no_lints,
            ..Default::default()
        };
        loaded.stacks.link_with(&options)
    });

    let mut renderer = Renderer::new(&loaded, cli.min_severity.into());

    // Packs that could not be opened at all, then each pack's manifest findings.
    for invalid in loaded.repository.invalid() {
        renderer.invalid_pack(invalid);
    }
    for (dir, error) in &loaded.unopened {
        renderer.unopened_pack(Some(dir), error);
    }
    for pack in loaded.packs() {
        for finding in pack.report().diagnostics.iter() {
            renderer.diagnostic(finding, Some(pack));
        }
    }

    // Everything the stacks load (pack selection, documents of every family), then the links.
    let loaded_findings = loaded.stacks.load_all();
    let findings = loaded_findings
        .iter()
        .filter(|finding| !is_base_game_absent(finding))
        .chain(links.iter().flat_map(|links| links.iter()));
    for finding in findings {
        renderer.diagnostic(finding, None);
    }

    let tally = renderer.finish();
    status::status(
        "Finished",
        format!("in {}", status::elapsed(start.elapsed())),
    );
    Ok(tally)
}

/// The stack asks for the game's own packs (the base-game layer), which are not installed here:
/// their absence is no finding about the packs being checked.
fn is_base_game_absent(finding: &Diagnostic) -> bool {
    matches!(finding.code, DiagnosticCode::PackNotFound { .. }) && finding.location.path.is_none()
}
