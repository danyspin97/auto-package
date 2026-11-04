//! The `explain` sub-command: print what the work directory says about one
//! package. It reads the run files only: no model, no network, no writes.

use std::path::{Path, PathBuf};

use clap::Args;

#[derive(Args)]
pub(super) struct ExplainArgs {
    /// RPM package name, e.g. aws-c-http.
    name: String,

    #[arg(long, default_value = "ap-work")]
    work_dir: PathBuf,

    /// OBS project whose work directory to read (needed when the work
    /// directory holds several projects that know the package).
    #[arg(long)]
    project: Option<String>,

    #[arg(long, default_value = "x86_64")]
    arch: String,

    /// Run id to explain (default: the latest run that touched the package).
    #[arg(long)]
    run: Option<String>,

    /// Directory the package notes are kept in.
    #[arg(
        long = "notes",
        alias = "notes-dir",
        default_value = "../auto-package-notes"
    )]
    notes_dir: PathBuf,

    /// Print the same data as JSON.
    #[arg(long)]
    json: bool,
}

pub(super) fn run(args: &ExplainArgs) -> color_eyre::Result<()> {
    explain_command(
        &args.name,
        &args.work_dir,
        args.project.as_deref(),
        &args.arch,
        args.run.as_deref(),
        &args.notes_dir,
        args.json,
    )
}

/// `explain`: print what the work directory says about one package.
fn explain_command(
    name: &str,
    work_dir: &Path,
    project: Option<&str>,
    arch: &str,
    run: Option<&str>,
    notes_dir: &Path,
    json: bool,
) -> color_eyre::Result<()> {
    use std::io::IsTerminal;
    let err = |e: String| color_eyre::eyre::eyre!(e);
    let dir = ap_build::explain::resolve_project_dir(work_dir, project, arch, name).map_err(err)?;
    ap_build::package_notes::set_notes_dir(std::path::absolute(notes_dir)?);
    let package_notes = ap_build::package_notes::path_for(name)
        .map(|_| ap_build::package_notes::read(name))
        .unwrap_or_default();
    let ex = ap_build::explain::explain(&ap_build::explain::Query {
        project_dir: &dir,
        package: name,
        run,
        package_notes,
    })
    .map_err(err)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&ex)?);
        return Ok(());
    }
    let tty = std::io::stdout().is_terminal();
    let width = if tty {
        crossterm::terminal::size().map_or(100, |(w, _)| w as usize)
    } else {
        100
    };
    let color = tty && std::env::var_os("NO_COLOR").is_none();
    print!(
        "{}",
        ap_build::explain::ansi(&ap_build::explain::render(&ex, width.clamp(40, 140)), color)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::ExplainArgs;

    #[derive(Parser)]
    struct Wrapper {
        #[command(flatten)]
        args: ExplainArgs,
    }

    #[test]
    fn the_explain_flags_parse_with_their_defaults() {
        let w = Wrapper::try_parse_from(["x", "aws-c-http", "--json", "--run", "r1"]).unwrap();
        assert_eq!(w.args.name, "aws-c-http");
        assert!(w.args.json);
        assert_eq!(w.args.run.as_deref(), Some("r1"));
        assert_eq!(w.args.arch, "x86_64");
        assert_eq!(w.args.work_dir.to_str(), Some("ap-work"));
    }
}
