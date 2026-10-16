# Porting verdicts for commits not reachable from the tip

All were checked against the tip (28d0e19). Every change is already present in rewritten form, so nothing was ported.

| Commit | Verdict | Evidence in tip |
|---|---|---|
| 59113ef0 sdist budgets | PRESENT | ecosystem/src/sdist.rs: MAX_ENTRIES, MAX_TOTAL_BYTES, MAX_EXPANSION, read_manifest cap |
| 6b0e7e34 forge names | PRESENT | build/src/sources.rs: owner_and_repo_must_be_plain_names |
| 09e26d76 strip control sequences | PRESENT | core/src/sink.rs: printable, strip_controls |
| da7365dc vouched discovery | PRESENT | pipeline.rs: is_vouched_for, discovery_vouching_tests |
| b8202ea2 fence log excerpts | PRESENT | agent.rs: fenced_data |
| 2df90c87 commit only package files | PRESENT | osc.rs: publish_plan |
| fb5a45f7 repack without links | PRESENT | pipeline.rs: repack_tarball with follow_symlinks(false) |
| 8cf4b9c7 refuse escaping links | PRESENT | sdist.rs: check_link, escaping_link |
| 7e6262e7 pip wheels only, cleared env | PRESENT | pip.rs: --only-binary=:all:, env_clear, pip_env |
| ffa966df rpmspec in bwrap | PRESENT | osc.rs: rpmspec_sandbox_works, sandbox_prefix |
| c041be56 narrow tools, scrub env | PRESENT | agent.rs: turn_allowed_tools, scrub_env_for_agent |
| 61897b7 README threat model | PRESENT | README.md "Threat model" |
| 80d4134f dependency-driven scheduling | PRESENT | cli/src/main.rs: done_this_round, select after each finish |
| 22423d8f stream archives | PRESENT | http.rs: download_file, stream_to |
| 4f59499c partial sdist unpack | PRESENT | sdist::extract_where used in pipeline.rs |
| 37106da8 editable-only snapshot | PRESENT | pipeline.rs: CheckoutSnapshot, is_agent_editable |
| 8f1fb78, b9dcb8f, df2b40e, 9e63c3c, 0d89340 | PRESENT | git cherry reports them patch-equivalent in the tip |
| 6322d54c style | SKIPPED | formatting only |
| 1d3b7158 404 text | PRESENT | osc.rs: is_not_found |
| 61e8c5fb keep stream tail | PRESENT | proc.rs: push_tail |
| f2c52c2e bounded up-recovery | PRESENT | osc.rs: move_aside |
| c726fa97, d9679adc only BuildFailed to agent, empty log | PRESENT | osc.rs "no usable build log"; pipeline.rs matches osc::Error::BuildFailed |
| 29312f14 OBS spelling | PRESENT | osc.rs: obs_name |
| d7cfe3d1 listing errors | PRESENT | osc.rs: try_list_project_packages, get_project_repo_paths |
| 49193075 Arc listing | PRESENT | osc.rs: Arc<HashSet<String>> cache |
| 02de6956 libc::kill | PRESENT | core/src/pgroup.rs |
| aad35ee8 column order | PRESENT | osc.rs: parse_single_build_status(repo, arch, pkg) |
| 12614015 stale verdicts | PRESENT | osc.rs: status_from_osc_text "stale:" |
| 4b6b98fe create on 404 only | PRESENT | osc.rs: is_not_found |
| c08a708f MemorySink totals | PRESENT | sink.rs: usage(), finished(), serialize_warned |
