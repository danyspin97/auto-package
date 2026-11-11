//! The `explain` sub-command: print what the work directory says about one
//! package. It reads the run files only: no model, no network, no writes.

use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};

#[derive(Args)]
pub(super) struct ExplainArgs {
    /// RPM package name, e.g. aws-c-http. Without one, the tries of every
    /// package of the run (`--run`, default the latest run).
    name: Option<String>,

    #[arg(long, default_value = "ap-work")]
    work_dir: PathBuf,

    /// OBS project whose work directory to read (needed when the work
    /// directory holds several projects that know the package).
    #[arg(long)]
    project: Option<String>,

    /// The branch project the package was built in, as `update` names it
    /// (for example `home:<user>:branches:Base:System`). Read instead of
    /// `--project`, which is the source project.
    #[arg(long, conflicts_with = "project")]
    branch_project: Option<String>,

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

    /// How the explanation is printed on stdout.
    #[arg(long, value_enum, default_value = "text")]
    format: OutputFormat,

    /// The same as `--format json`.
    #[arg(long, hide = true)]
    json: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

pub(super) fn run(args: &ExplainArgs) -> color_eyre::Result<()> {
    let project = args.branch_project.as_deref().or(args.project.as_deref());
    let json = args.json || matches!(args.format, OutputFormat::Json);
    match &args.name {
        Some(name) => explain_command(
            name,
            &args.work_dir,
            project,
            &args.arch,
            args.run.as_deref(),
            &args.notes_dir,
            json,
        ),
        None => run_command(
            &args.work_dir,
            project,
            &args.arch,
            args.run.as_deref(),
            json,
        ),
    }
}

/// The width and colour of a report on stdout.
fn terminal_style() -> (usize, bool) {
    use std::io::IsTerminal;
    let tty = std::io::stdout().is_terminal();
    let width = if tty {
        crossterm::terminal::size().map_or(100, |(w, _)| w as usize)
    } else {
        100
    };
    (
        width.clamp(40, 140),
        tty && std::env::var_os("NO_COLOR").is_none(),
    )
}

/// `explain` without a package: the tries of every package of one run.
fn run_command(
    work_dir: &Path,
    project: Option<&str>,
    arch: &str,
    run: Option<&str>,
    json: bool,
) -> color_eyre::Result<()> {
    let err = |e: String| color_eyre::eyre::eyre!(e);
    let dir = ap_build::explain::resolve_run_dir(work_dir, project, arch).map_err(err)?;
    if project.is_some() {
        ap_core::sink::eprint_coloured(&format!("note: reading {}", dir.display()));
    }
    let rt = ap_build::explain::run_tries(&dir, run).map_err(err)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&rt)?);
        return Ok(());
    }
    let (width, color) = terminal_style();
    print!(
        "{}",
        ap_build::explain::ansi(&ap_build::explain::render_run(&rt, width), color)
    );
    Ok(())
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
    let err = |e: String| color_eyre::eyre::eyre!(e);
    let dir = ap_build::explain::resolve_project_dir(work_dir, project, arch, name).map_err(err)?;
    if project.is_some() {
        // Says which directory was read: after `update` it is the branch's.
        ap_core::sink::eprint_coloured(&format!("note: reading {}", dir.display()));
    }
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
    let (width, color) = terminal_style();
    print!(
        "{}",
        ap_build::explain::ansi(&ap_build::explain::render(&ex, width), color)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{ExplainArgs, OutputFormat};

    #[derive(Parser)]
    struct Wrapper {
        #[command(flatten)]
        args: ExplainArgs,
    }

    #[test]
    fn the_explain_flags_parse_with_their_defaults() {
        let w = Wrapper::try_parse_from(["x", "aws-c-http", "--json", "--run", "r1"]).unwrap();
        assert_eq!(w.args.name.as_deref(), Some("aws-c-http"));
        assert!(w.args.json);
        assert_eq!(w.args.run.as_deref(), Some("r1"));
        assert_eq!(w.args.arch, "x86_64");
        assert_eq!(w.args.work_dir.to_str(), Some("ap-work"));
    }

    #[test]
    fn without_a_package_explain_is_the_whole_run() {
        let w = Wrapper::try_parse_from(["x", "--run", "r1"]).unwrap();
        assert!(w.args.name.is_none());
        assert_eq!(w.args.run.as_deref(), Some("r1"));
    }

    #[test]
    fn a_branch_project_is_read_instead_of_the_source_project() {
        let w =
            Wrapper::try_parse_from(["x", "pkg", "--branch-project", "home:u:branches:B"]).unwrap();
        assert_eq!(w.args.branch_project.as_deref(), Some("home:u:branches:B"));
        assert!(
            Wrapper::try_parse_from(["x", "pkg", "--project", "B", "--branch-project", "C"])
                .is_err()
        );
    }

    #[test]
    fn the_format_is_text_unless_asked_and_json_is_an_alias() {
        let text = Wrapper::try_parse_from(["x", "aws-c-http"]).unwrap();
        assert!(matches!(text.args.format, OutputFormat::Text));
        assert!(!text.args.json);
        let json = Wrapper::try_parse_from(["x", "aws-c-http", "--format", "json"]).unwrap();
        assert!(matches!(json.args.format, OutputFormat::Json));
        assert!(Wrapper::try_parse_from(["x", "aws-c-http", "--format", "yaml"]).is_err());
    }
}
