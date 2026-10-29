//! The `edit-bytes` sub-command: a byte-exact edit of a file under the new
//! sources, which a refresh turn runs for a file its Edit tool refuses as not
//! UTF-8. The turn is granted this one command shape (see the agent's grant);
//! the paths are checked by the library, not trusted to the grant.

use std::path::{Path, PathBuf};

use clap::Args;

#[derive(Args)]
pub(super) struct EditBytesArgs {
    /// File to edit, relative to the workspace (the current directory), under
    /// `source/`.
    file: PathBuf,

    /// File, relative to the workspace, holding the exact text to replace.
    #[arg(long)]
    old_file: PathBuf,

    /// File, relative to the workspace, holding the text that replaces it.
    #[arg(long)]
    new_file: PathBuf,
}

pub(super) fn run(args: &EditBytesArgs) -> color_eyre::Result<()> {
    run_in(&std::env::current_dir()?, args)
}

/// [`run`] for the workspace `ws`: the paths of `args` are relative to it.
fn run_in(ws: &Path, args: &EditBytesArgs) -> color_eyre::Result<()> {
    ap_build::byte_edit::request::edit_in_workspace(
        ws,
        &args.file,
        &args.old_file,
        &args.new_file,
    )?;
    println!("edited {}: one occurrence replaced", args.file.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Commands};
    use clap::Parser;

    fn parse(args: &[&str]) -> Result<EditBytesArgs, clap::Error> {
        let argv = std::iter::once("auto-package").chain(args.iter().copied());
        match Cli::try_parse_from(argv)?.command {
            Commands::EditBytes(args) => Ok(args),
            _ => panic!("not the edit-bytes sub-command"),
        }
    }

    #[test]
    fn the_documented_shape_parses() {
        let args = parse(&[
            "edit-bytes",
            "source/main/mysqldump.result",
            "--old-file",
            "byte-edit/old.txt",
            "--new-file",
            "byte-edit/new.txt",
        ])
        .unwrap();
        assert_eq!(args.file, PathBuf::from("source/main/mysqldump.result"));
        assert_eq!(args.old_file, PathBuf::from("byte-edit/old.txt"));
        assert_eq!(args.new_file, PathBuf::from("byte-edit/new.txt"));
    }

    #[test]
    fn extra_arguments_and_other_flags_are_refused() {
        let base = [
            "edit-bytes",
            "source/x",
            "--old-file",
            "byte-edit/o",
            "--new-file",
            "byte-edit/n",
        ];
        let with = |extra: &[&'static str]| {
            let mut v = base.to_vec();
            v.extend_from_slice(extra);
            parse(&v)
        };
        assert!(with(&["source/y"]).is_err(), "a second file");
        assert!(with(&["--force"]).is_err(), "an unknown flag");
        assert!(
            with(&["--old-file", "byte-edit/p"]).is_err(),
            "a repeated flag"
        );
        assert!(with(&["--", "x"]).is_err(), "after the end of options");
        assert!(parse(&["edit-bytes", "source/x", "--old-file", "o"]).is_err());
        assert!(parse(&["edit-bytes"]).is_err());
    }

    #[test]
    fn a_refused_request_is_an_error_and_a_good_one_edits() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::create_dir_all(ws.join("sdist/top")).unwrap();
        std::os::unix::fs::symlink("sdist/top", ws.join("source")).unwrap();
        std::fs::write(ws.join("sdist/top/a.txt"), b"\xe9 old\n").unwrap();
        std::fs::create_dir_all(ws.join("byte-edit")).unwrap();
        std::fs::write(ws.join("byte-edit/old.txt"), "old").unwrap();
        std::fs::write(ws.join("byte-edit/new.txt"), "new").unwrap();
        let req = |file: &str| EditBytesArgs {
            file: PathBuf::from(file),
            old_file: PathBuf::from("byte-edit/old.txt"),
            new_file: PathBuf::from("byte-edit/new.txt"),
        };
        assert!(run_in(ws, &req("/etc/passwd")).is_err());
        assert!(run_in(ws, &req("source/../byte-edit/old.txt")).is_err());
        assert_eq!(
            std::fs::read(ws.join("sdist/top/a.txt")).unwrap(),
            b"\xe9 old\n"
        );
        run_in(ws, &req("source/a.txt")).unwrap();
        assert_eq!(
            std::fs::read(ws.join("sdist/top/a.txt")).unwrap(),
            b"\xe9 new\n"
        );
    }
}
