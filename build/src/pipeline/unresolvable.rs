//! The round for unresolvable dependencies.

use super::*;
use crate::pipeline::agent::*;
use crate::pipeline::archive::*;
use crate::pipeline::rpm_deps::*;

/// OBS reported unresolvable dependencies for the build.
pub(super) fn unresolvable_deps_round(
    ctx: &RunCtx,
    st: &mut RunState,
    build_step: &StepDir,
    build_log: &Path,
    deps: Vec<String>,
    detail: Vec<String>,
) -> Result<RoundEnd, PipelineResult> {
    let RunCtx {
        config,
        reporter,
        ws,
        rpm_name,
        spec_path,
        ..
    } = ctx;
    let reporter = *reporter;
    reporter.report(&Event::MissingPackages {
        step: build_step.id,
        names: deps.clone(),
        source: ap_core::event::MissingSource::BuildLog,
    });
    // A rich dependency (`(x >= 1 with x < 2)`) carries no `(got version ...)`:
    // read the distro's version into it, so a bound it misses is relaxed like
    // any other (convention 59) instead of the package being queued as missing.
    let detail = osc::annotate_rich_misses(detail, &|dep| {
        linked_shipped_version(ctx, build_step.id, dep)
    });
    // "have choice for libnccl.so.2 needed by X: python313-nvidia-nccl-cu13
    // python314-nvidia-nccl-cu13" names nothing missing: the solver cannot pick
    // between the flavors of one package that all provide what is needed. An
    // explicit BuildRequires for that package settles it. (This is what kept
    // flashinfer retrying the same spec eleven times.)
    let choice_requires: Vec<String> = {
        let mut stems: Vec<String> = Vec::new();
        for c in osc::provider_choices(&detail) {
            let mut bare = c.providers.iter().map(|p| osc::strip_rpm_prefix(p));
            let Some(first) = bare.next() else { continue };
            if first.is_empty() || !bare.all(|b| b == first) {
                continue;
            }
            // The name goes into a BuildRequires line verbatim: OBS text is not
            // trusted to hold only a package name.
            if !is_provider_stem(first) {
                reporter.report(&Event::Warning {
                    step: Some(build_step.id),
                    message: format!(
                        "not adding a BuildRequires for the OBS provider {first:?}: not a package name"
                    ),
                });
                continue;
            }
            let req = format!("%{{python_module {first}}}");
            if !stems.contains(&req) {
                stems.push(req);
            }
        }
        stems
    };
    if !choice_requires.is_empty()
        && let Ok(mut text) = std::fs::read_to_string(spec_path)
    {
        let mut added = Vec::new();
        for req in &choice_requires {
            if let Some(next) = crate::spec::add_build_requires(&text, req) {
                text = next;
                added.push(req.clone());
            }
        }
        if !added.is_empty() {
            write_or_warn(spec_path, &text);
            reporter.report(&Event::Warning {
                step: Some(build_step.id),
                message: format!(
                    "OBS cannot choose between the flavors that provide what the build needs; \
                     adding BuildRequires {} to settle it",
                    added.join(", ")
                ),
            });
            st.attempts += 1;
            persist_attempts(ws, &config.run_id, st.attempts);
            if st.attempts > config.max_attempts {
                return Err(ctx.failed(format!(
                    "OBS still cannot choose a provider after {} attempt(s); log: {}",
                    st.attempts,
                    build_log.display()
                )));
            }
            return Ok(RoundEnd::Retry);
        }
    }

    // Filter out deps already satisfied by an RPM in rpms_dir so the
    // agent only sees packages that are genuinely not available yet.
    // The names carry no version bound, so an RPM of any version would satisfy
    // them: a package OBS says is too old for a bound (`got version 4.7.1`
    // against `>= 4.8.0`) is outdated, and is brought up to date, not skipped
    // because the old build is here.
    let outdated_keys: Vec<String> = osc::outdated_packages(&detail)
        .into_iter()
        .map(|(n, _)| osc::fuzzy_key(&n))
        .collect();
    let unresolvable_deps: Vec<String> = deps
        .into_iter()
        .filter(|dep| {
            let satisfied =
                rpm_satisfies_dep(&config.rpms_dir, dep) && !is_outdated_dep(&outdated_keys, dep);
            if satisfied {
                reporter.report(&Event::Warning {
                    step: Some(build_step.id),
                    message: format!(
                        "OBS reports {dep} unresolvable, but an RPM built here already \
                         satisfies it in {}: not queuing it again",
                        config.rpms_dir.display()
                    ),
                });
            }
            !satisfied
        })
        .collect();

    // A name the distro publishes under another spelling (`huggingface_hub`
    // against `huggingface-hub`) is not a package to build: the spec copied the
    // PyPI spelling. It is respelled in place and the build runs again.
    let respelled: Vec<(String, String)> = {
        let emit = Emit::new(reporter, build_step.id);
        let mut found: Vec<(String, String)> = Vec::new();
        for dep in &unresolvable_deps {
            if osc::is_rpm_capability(dep)
                || osc::project_ships_binary(&config.project, &config.arch, dep, emit)
                    != Some(false)
            {
                continue;
            }
            let from = osc::strip_rpm_prefix(dep).to_string();
            let Some(to) = osc::published_respelling(&config.project, &config.arch, dep, emit)
                .or_else(|| linked_package_spelling(ctx, build_step.id, &from))
            else {
                continue;
            };
            if !found.iter().any(|(f, _)| *f == from) {
                found.push((from, to));
            }
        }
        found
    };
    if !respelled.is_empty()
        && let Ok(text) = std::fs::read_to_string(spec_path)
    {
        let mut next = text.clone();
        for (from, to) in &respelled {
            next = crate::spec::rewrite_dependency_name(&next, from, to);
        }
        if next != text {
            write_or_warn(spec_path, &next);
            reporter.report(&Event::Warning {
                step: Some(build_step.id),
                message: format!(
                    "the distro publishes {} under another spelling; respelling the spec \
                     instead of building a second copy",
                    respelled
                        .iter()
                        .map(|(from, to)| format!("{from} as {to}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
            st.attempts += 1;
            persist_attempts(ws, &config.run_id, st.attempts);
            if st.attempts > config.max_attempts {
                return Err(ctx.failed(format!(
                    "OBS still reports a respelled dependency missing after {} attempt(s); log: {}",
                    st.attempts,
                    build_log.display()
                )));
            }
            return Ok(RoundEnd::Retry);
        }
    }

    // A name nothing publishes for this target, while similar names are
    // published (`ffmpeg-8-devel` against `ffmpeg-8-mini-devel`), is a package
    // the distro has under another name or flavor, not one to build: the spec
    // asked for the wrong one. The agent is told what exists.
    let mut alternatives: Vec<(String, Vec<String>)> = Vec::new();
    let unresolvable_deps: Vec<String> = unresolvable_deps
        .into_iter()
        .filter(|dep| {
            if osc::is_rpm_capability(dep) {
                return true;
            }
            let emit = Emit::new(reporter, build_step.id);
            if osc::project_ships_binary(&config.project, &config.arch, dep, emit) != Some(false) {
                return true;
            }
            let similar = osc::similar_published_binaries(&config.project, &config.arch, dep, emit);
            if similar.is_empty() {
                return true;
            }
            alternatives.push((dep.clone(), similar));
            false
        })
        .collect();
    // The alternatives are reported whether or not other dependencies are
    // still unresolvable: the agent needs the distro's names either way.
    let alternatives_context = (!alternatives.is_empty()).then(|| {
        let lines: Vec<String> = alternatives
            .iter()
            .map(|(dep, similar)| {
                format!(
                    "{dep} is not published for {}/{} by the projects {} builds against, \
                     but these are: {}",
                    config.distro,
                    config.arch,
                    config.project,
                    similar.join(", ")
                )
            })
            .collect();
        reporter.report(&Event::Warning {
            step: Some(build_step.id),
            message: format!(
                "not queuing {} as missing: the distro has it under another name\n{}",
                alternatives
                    .iter()
                    .map(|(d, _)| d.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                lines.join("\n")
            ),
        });
        format!(
            "OBS cannot install what the spec asks for, but the distro has it under another \
             name or flavor; it is not a package to build. Change the BuildRequires/Requires \
             to a name that is published, or drop the requirement if nothing published \
             serves it (a codec-complete ffmpeg, for example, is not in the distro: use the \
             `-mini` flavor and skip what needs the rest). Do not report these in \
             `unresolvable_packages`.\n\n{}",
            crate::agent::fenced_data(&lines.join("\n"))
        )
    });
    if unresolvable_deps.is_empty()
        && let Some(context) = &alternatives_context
    {
        return fix_after_build(ctx, st, build_step, build_log, Some(context));
    }

    // A package OBS has at a version NEWER than a bound the spec wrote is not
    // missing: its bound is dropped (the dependency stays) and the build retried.
    let overshoot: Vec<osc::VersionMiss> = osc::version_misses(&detail)
        .into_iter()
        .filter(|m| m.too_new)
        .collect();
    let unresolvable_deps: Vec<String> = if overshoot.is_empty() {
        unresolvable_deps
    } else {
        update_state(ws, reporter, |state| {
            for m in &overshoot {
                state.relax_dependency(&m.name, &m.got);
            }
        });
        reporter.report(&Event::Warning {
            step: Some(build_step.id),
            message: format!(
                "{} {} newer than the bound the spec asks for: keeping the package and \
                 dropping the bound (convention 59)",
                overshoot
                    .iter()
                    .map(|m| format!("{} ({})", m.name, m.got))
                    .collect::<Vec<_>>()
                    .join(", "),
                if overshoot.len() == 1 { "is" } else { "are" }
            ),
        });
        unresolvable_deps
            .into_iter()
            .filter(|d| {
                let key = osc::fuzzy_key(osc::strip_rpm_prefix(d));
                !overshoot.iter().any(|m| osc::fuzzy_key(&m.name) == key)
            })
            .collect()
    };

    // A library of the NVIDIA driver (`libcuda.so.1()(64bit)`) is nothing to
    // build: the requiring package leaves it out of its Requires, which is
    // done here, deterministically, and the build retried.
    let (driver_deps, unresolvable_deps): (Vec<String>, Vec<String>) = unresolvable_deps
        .into_iter()
        .partition(|d| crate::depclass::is_driver_soname(d));
    let mut driver_fixed = false;
    // The spec as it was BEFORE this edit: the guard in `run_stages` rejects a
    // retry whose spec still hashes to this, so it must be the pre-edit hash.
    let mut driver_before: Option<String> = None;
    if !driver_deps.is_empty()
        && let Ok(current) = std::fs::read_to_string(spec_path)
    {
        let fixed = crate::spec::with_driver_requires_exclude(&current);
        if fixed != current {
            driver_before = sha256_file(spec_path).ok();
            write_or_warn(spec_path, &fixed);
            driver_fixed = true;
            ctx.reporter.report(&Event::Warning {
                step: Some(build_step.id),
                message: format!(
                    "{} is a library of the NVIDIA driver: added it to \
                     `%global __requires_exclude` instead of requiring it",
                    driver_deps.join(", ")
                ),
            });
        }
    }
    if driver_fixed {
        st.relaxed_from_spec = driver_before;
    }

    if unresolvable_deps.is_empty() {
        // The driver libraries were seen but the spec already excludes them:
        // a retry would build the same spec and get the same answer.
        if !driver_deps.is_empty() && !driver_fixed {
            return Err(ctx.failed(format!(
                "OBS reports {} unresolvable, but the spec already has them in \
                 `%global __requires_exclude` (the NVIDIA driver relaxation is present) \
                 and OBS still reports them; log: {}",
                driver_deps.join(", "),
                build_log.display()
            )));
        }
        // Every dep has a local RPM, so OBS should pick them up on
        // retry. Count the round anyway — if OBS keeps saying
        // unresolvable we would otherwise spin here forever.
        st.attempts += 1;
        persist_attempts(ws, &config.run_id, st.attempts);
        if st.attempts > config.max_attempts {
            return Err(ctx.failed(format!(
                "OBS keeps reporting unresolvable dependencies after \
                 {} attempt(s) even though every one has a local \
                 RPM; log: {}",
                st.attempts,
                build_log.display()
            )));
        }
        if !overshoot.is_empty() {
            st.relaxed_from_spec = sha256_file(spec_path).ok();
        }
        return Ok(RoundEnd::Retry);
    }

    // Unresolvable deps cannot be fixed by spec edits — they
    // name a package that genuinely doesn't exist yet, not a
    // wrong constraint in this spec. Skip the FixAfterBuild
    // spec-fix loop and queue them for packaging directly, but
    // give the agent one single, read-only, cheap-model lookup
    // first to translate OBS's RPM-wrapped names
    // (`libtrantor-devel`) into the real upstream project name
    // (`trantor`) — mechanical lib/-devel stripping alone misses
    // cases like that one. "Once" is enforced by
    // `unresolvable_review_hash`: a later invocation of this
    // package that hits the exact same dep set reuses the cached
    // translation instead of asking again.
    reporter.report(&Event::BuildFailed {
        step: build_step.id,
        classification: ap_core::event::BuildFailure::UnresolvableDeps {
            packages: unresolvable_deps.clone(),
        },
        log: build_log.to_path_buf(),
    });

    let mut sorted_deps = unresolvable_deps.clone();
    sorted_deps.sort_unstable();
    sorted_deps.dedup();

    let prior_state = PkgState::load(ws);
    let already_reviewed = prior_state.already_reviewed_unresolvable(&sorted_deps);
    let mut translated = if already_reviewed {
        prior_state.unresolvable_translated.clone()
    } else {
        Vec::new()
    };

    if !already_reviewed && !config.no_agent && st.spent_usd < config.budget_usd {
        let turn = agent_turn(
            TurnCtx {
                label: "translate-deps",
                task: crate::agent::Task::TranslateUnresolvableDeps,
                event_task: ap_core::event::Task::TranslateUnresolvableDeps,
                attempt: 0,
                hint: None,
                unresolvable_deps: &sorted_deps,
                unresolvable_detail: &detail,
                checkout_dir: None,
                from_repo_archive: st.from_repo_archive,
                resume: false,
                context: alternatives_context.as_deref(),
            },
            ctx.turn_env(),
            &mut st.spent_usd,
            &mut st.session_id,
            &mut st.step_attempts,
        );
        let turn_succeeded = turn.is_ok();
        if let Ok((_, outcome)) = turn {
            translated = outcome.unresolvable_packages;
        }

        // A translated name that merely respells a bare name
        // already in this spec (`huggingface_hub` ->
        // `huggingface-hub` — same project, OBS's literal
        // requires-dist spelling vs. the dash form the
        // dependency actually gets packaged and published
        // under) leaves this package's own
        // `%{python_module ...}` line asking for a name
        // nothing will ever provide, even once the translated
        // dependency is built. Rewrite it now — see
        // `spec::rewrite_python_module_name`'s doc comment for
        // why nothing else ever revisits this.
        if !translated.is_empty() {
            let bare_names: Vec<String> = sorted_deps
                .iter()
                .map(|d| osc::strip_devel_wrapping(osc::strip_rpm_prefix(d)).to_string())
                .collect();
            if let Ok(current) = std::fs::read_to_string(spec_path) {
                let mut rewritten = current.clone();
                for t in &translated {
                    if let Some(bare) = bare_names
                        .iter()
                        .find(|b| osc::fuzzy_key(b) == osc::fuzzy_key(t) && *b != t)
                    {
                        rewritten = crate::spec::rewrite_dependency_name(&rewritten, bare, t);
                    }
                }
                if rewritten != current {
                    write_or_warn(spec_path, &rewritten);
                }
            }
        }

        // A failed turn (rate limit, timeout, budget) asked nothing, so the
        // dep set must stay un-reviewed for the next invocation to retry.
        if turn_succeeded {
            update_state(ws, reporter, |state| {
                state.unresolvable_review_hash = Some(hash_deps(&sorted_deps));
                state.unresolvable_translated = translated.clone();
            });
        }
    }

    if !translated.is_empty() {
        reporter.report(&Event::MissingPackages {
            step: build_step.id,
            names: translated.clone(),
            source: ap_core::event::MissingSource::Agent,
        });
    }
    let source_list = if translated.is_empty() {
        &unresolvable_deps
    } else {
        &translated
    };
    // OBS says the target has nothing providing these, so the local distro is
    // not asked: it would only second-guess the repository being built for.
    let (to_build, skipped) = crate::depclass::triage_all(source_list, &|_| false);
    warn_skipped(ctx, build_step.id, &skipped);
    if to_build.is_empty() {
        // Nothing here is something to build: reporting an empty list would
        // have the caller retry this package forever.
        let why: Vec<String> = skipped
            .iter()
            .map(|(name, s)| format!("{name}: {}", s.reason()))
            .collect();
        return Err(ctx.failed(format!(
            "OBS reports {} unresolvable, but none of them is a package this tool can \
             build:\n{}",
            source_list.join(", "),
            why.join("\n")
        )));
    }
    let native = crate::depclass::native_among(&unresolvable_deps, &to_build);
    let outdated: Vec<(String, Option<String>)> = osc::outdated_packages(&detail)
        .into_iter()
        .filter(|(n, _)| {
            to_build
                .iter()
                .any(|b| osc::fuzzy_key(b) == osc::fuzzy_key(n))
        })
        .collect();
    Err(PipelineResult::MissingDeps {
        rpm_name: rpm_name.clone(),
        deps: to_build,
        patches: Vec::new(),
        native,
        outdated,
    })
}
