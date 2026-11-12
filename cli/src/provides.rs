//! The `provides` sub-command: which package provides a pkg-config module, a
//! CMake package, a linker library or a file, from the repository data on this
//! machine. A turn is granted this one command shape (see the agent's grant);
//! the words are checked by the library, not trusted to the grant.

use clap::Args;

#[derive(Args)]
pub(super) struct ProvidesArgs {
    /// What the name is: `pkgconfig`, `cmake`, `lib` (as in `-lpam`) or `file`.
    kind: String,

    /// The module, package or library name, or a file path without its leading `/`.
    name: String,
}

pub(super) fn run(args: &ProvidesArgs) -> color_eyre::Result<()> {
    let kind = ap_build::provides::Kind::parse(&args.kind)
        .ok_or_else(|| color_eyre::eyre::eyre!("kind must be pkgconfig, cmake, lib or file"))?;
    let text =
        ap_build::provides::lookup(kind, &args.name).map_err(|e| color_eyre::eyre::eyre!(e))?;
    print!("{text}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};
    #[test]
    fn the_provides_helper_is_hidden_and_takes_a_kind_and_a_name() {
        let cmd = crate::Cli::command();
        assert!(cmd.find_subcommand("provides").unwrap().is_hide_set());
        assert!(crate::Cli::try_parse_from(["auto-package", "provides", "lib", "pam"]).is_ok());
        assert!(crate::Cli::try_parse_from(["auto-package", "provides", "lib"]).is_err());
    }
}
