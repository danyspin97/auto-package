//! The build-recipe files of a checkout (`_multibuild`, `_constraints`): which of
//! them a spec has no use for.

/// Whether `name` is a build-recipe file (`_multibuild`, `_constraints`) that the
/// spec has no use for. `recipe` is the file's text. A `_multibuild` with a
/// `<package>` entry builds a sibling spec of the package as its own package, so
/// it is kept whatever the spec says; one that only declares flavors is unused
/// unless the spec reads `%flavor`. `_constraints` asks OBS for build
/// resources: a pure-Python (`BuildArch: noarch`) spec that does not mention
/// constraints or `%limit_build` compiles nothing that needs them, so one
/// inherited from a Factory recipe of another shape is a leftover. A spec that
/// builds native code keeps it: the agent writes it without touching the spec.
pub(super) fn recipe_file_unused(name: &str, spec_text: &str, recipe: &str) -> bool {
    match name {
        "_multibuild" => {
            !recipe.contains("<package>")
                && !["%{flavor}", "%flavor", "%{?flavor", "%{!?flavor"]
                    .iter()
                    .any(|m| spec_text.contains(m))
        }
        "_constraints" => {
            // Only the main package's own `BuildArch:` counts: a `noarch`
            // subpackage (mariadb's `-test`, a `-doc`) says nothing about
            // the build that `_constraints` sizes.
            let noarch = spec_text
                .lines()
                .take_while(|l| {
                    let l = l.trim_start();
                    !(l.starts_with("%package") || l.starts_with("%description"))
                })
                .any(|l| {
                    let l = l.trim();
                    l.get(..9)
                        .is_some_and(|p| p.eq_ignore_ascii_case("buildarch"))
                        && l.ends_with("noarch")
                });
            noarch && !spec_text.contains("constraints") && !spec_text.contains("%limit_build")
        }
        _ => false,
    }
}
