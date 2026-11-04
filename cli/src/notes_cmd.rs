//! `notes`: print what auto-package has learned about a package.

use std::path::Path;

pub(crate) fn notes_command(name: &str, notes_dir: &Path) -> color_eyre::Result<()> {
    ap_build::package_notes::set_notes_dir(std::path::absolute(notes_dir)?);
    if ap_build::package_notes::path_for(name).is_none() {
        return Err(color_eyre::eyre::eyre!(
            "{name:?} is not a valid package name"
        ));
    }
    let text = ap_build::package_notes::read(name);
    if text.is_empty() {
        println!("no notes for {name} yet in {}", notes_dir.display());
    } else {
        print!("{text}");
    }
    Ok(())
}
