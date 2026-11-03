use super::*;

/// The VulkanMemoryAllocator spec of X11:Wayland, with its `Group:` line.
const VMA: &str = "Name:           VulkanMemoryAllocator\nVersion:        3.4.0\n\
    Release:        0\nSummary:        Vulkan memory allocation library\n\
    License:        MIT\nGroup:          Development/Libraries/C and C++\n\
    URL:            https://github.com/GPUOpen-LibrariesAndSDKs/VulkanMemoryAllocator\n\
    BuildRequires:  cmake\nBuildArch:      noarch\n\n%description\nA library.\n\n\
    %package devel\nSummary:        Vulkan memory allocation library\n\
    Requires:       vulkan-headers >= 1.3\n\n%description devel\nA library.\n\n\
    %prep\n%autosetup\n\n%build\n%cmake\n%cmake_build\n\n%install\n%cmake_install\n\n\
    %files devel\n%license LICENSE.txt\n\n%changelog\n";

#[test]
fn the_group_line_of_the_preamble_goes_and_nothing_else_moves() {
    let out = drop_group_tags(VMA).unwrap();
    assert_eq!(
        out,
        VMA.replace("Group:          Development/Libraries/C and C++\n", "")
    );
    assert_eq!(drop_group_tags(&out), None);
}

#[test]
fn a_group_in_a_subpackage_preamble_goes_too() {
    let spec = "Name: x\n%package devel\nGroup: Development/Libraries/C\nSummary: s\n\
                %description devel\nd\n%files devel\n/a\n%changelog\n";
    assert_eq!(
        drop_group_tags(spec).unwrap(),
        "Name: x\n%package devel\nSummary: s\n%description devel\nd\n%files devel\n/a\n%changelog\n"
    );
}

#[test]
fn a_group_inside_a_conditional_is_left() {
    let spec = "Name: x\n%if 0%{?suse_version} < 1315\nGroup: System/Libraries\n%endif\n\
                %if 0%{?fedora}\n%else\nGroup: Other\n%endif\n";
    assert_eq!(drop_group_tags(spec), None);
}

#[test]
fn a_group_after_a_conditional_is_dropped_and_the_block_stays() {
    let spec = "%if 0%{?a}\nRequires: b\n%endif\nGroup: System/Libraries\nSummary: s\n";
    assert_eq!(
        drop_group_tags(spec).unwrap(),
        "%if 0%{?a}\nRequires: b\n%endif\nSummary: s\n"
    );
}

#[test]
fn a_group_text_in_a_description_comment_or_changelog_is_left() {
    let spec = "Name: x\n# Group: not a tag\n%description\nGroup: text\n\
                %changelog\n- Group: changed\n";
    assert_eq!(drop_group_tags(spec), None);
}

#[test]
fn a_spec_without_a_group_is_none() {
    assert_eq!(drop_group_tags("Name: x\nSummary: s\n"), None);
    assert_eq!(drop_group_tags(""), None);
}

#[test]
fn a_group_at_the_end_of_the_file_without_a_newline_is_dropped() {
    assert_eq!(drop_group_tags("Name: x\nGroup: y").unwrap(), "Name: x");
}

#[test]
fn the_buildroot_tag_goes_from_the_preamble_and_a_conditional_keeps_it() {
    let spec = "Name: x\nBuildRoot:   %{_tmppath}/%{name}\nSummary: s\n\
                %package devel\nBuildRoot: %{_tmppath}/%{name}-devel\n\
                %description devel\nd\n";
    let out = drop_buildroot_tags(spec).unwrap();
    assert_eq!(
        out,
        "Name: x\nSummary: s\n%package devel\n%description devel\nd\n"
    );
    assert_eq!(drop_buildroot_tags(&out), None);
    let kept = "%if 0%{?a}\nBuildRoot: x\n%endif\n";
    assert_eq!(drop_buildroot_tags(kept), None);
}

#[test]
fn a_buildroot_in_a_description_is_left() {
    let spec = "%description\nBuildRoot: text\n";
    assert_eq!(drop_buildroot_tags(spec), None);
}

#[test]
fn an_if_inside_a_multi_line_macro_does_not_open_a_block() {
    let spec = "%define m() \\\n%if 0%{?a} x %endif\nName: x\nGroup: y\n%changelog\n";
    assert_eq!(
        drop_group_tags(spec).unwrap(),
        "%define m() \\\n%if 0%{?a} x %endif\nName: x\n%changelog\n"
    );
    let block = "%global m \\\n%if 0%{?a}\\\n  x \\\n%endif\nName: x\nGroup: y\n";
    assert_eq!(
        drop_group_tags(block).unwrap(),
        "%global m \\\n%if 0%{?a}\\\n  x \\\n%endif\nName: x\n"
    );
}

#[test]
fn a_crlf_spec_keeps_its_line_endings_when_a_pass_applies() {
    let spec = "Name: x\r\nGroup: y\r\nSummary: s\r\n%description\r\nd\r\n";
    assert_eq!(
        drop_group_tags(spec).unwrap(),
        "Name: x\r\nSummary: s\r\n%description\r\nd\r\n"
    );
    let tags = "Name: x\r\nGroup: y\r\nBuildRoot: z\r\n%description\r\nd\r\n";
    assert_eq!(
        drop_buildroot_tags(tags).unwrap(),
        "Name: x\r\nGroup: y\r\n%description\r\nd\r\n"
    );
}

#[test]
fn a_crlf_spec_without_a_construct_is_none_and_a_missing_final_newline_stays_missing() {
    assert_eq!(drop_group_tags("Name: x\r\nSummary: s\r\n"), None);
    assert_eq!(drop_group_tags("Name: x\r\nGroup: y").unwrap(), "Name: x");
}

#[test]
fn a_clean_that_only_removes_the_build_root_goes_with_its_separator() {
    let spec = "%install\nmake\n\n%clean\nrm -rf %{buildroot}\n\n%files\n/a\n";
    assert_eq!(
        drop_clean_sections(spec).unwrap(),
        "%install\nmake\n\n%files\n/a\n"
    );
    let env = "%install\nmake\n%clean\nrm -rf $RPM_BUILD_ROOT\n\n%files\n/a\n";
    assert_eq!(
        drop_clean_sections(env).unwrap(),
        "%install\nmake\n\n%files\n/a\n"
    );
}

#[test]
fn an_empty_clean_header_goes_even_with_nothing_after_it() {
    assert_eq!(
        drop_clean_sections("%install\nmake\n%clean\n\n%files\n/a\n").unwrap(),
        "%install\nmake\n\n%files\n/a\n"
    );
    assert_eq!(
        drop_clean_sections("%install\nmake\n\n%clean\n").unwrap(),
        "%install\nmake\n"
    );
    assert_eq!(
        drop_clean_sections("%install\nmake\n%clean\n%changelog\n- x\n").unwrap(),
        "%install\nmake\n%changelog\n- x\n"
    );
}

#[test]
fn a_clean_at_the_end_of_the_file_keeps_no_trailing_blank() {
    assert_eq!(
        drop_clean_sections("%install\nmake\n\n%clean\nrm -rf %{buildroot}\n").unwrap(),
        "%install\nmake\n"
    );
}

#[test]
fn a_clean_with_other_commands_a_comment_or_a_conditional_stays() {
    for body in [
        "rm -rf %{buildroot}\necho done\n",
        "# keep the tree\nrm -rf %{buildroot}\n",
        "%if 0%{?a}\nrm -rf %{buildroot}\n%endif\n",
        "rm -rf %{buildroot}/cache\n",
        "rm -rf %{buildroot} # all\n",
        "rm -fr %{buildroot}\n",
    ] {
        let spec = format!("%install\nmake\n%clean\n{body}%files\n/a\n");
        assert_eq!(drop_clean_sections(&spec), None, "{body}");
    }
}

#[test]
fn a_clean_inside_a_conditional_is_left() {
    let spec = "%if 0%{?a}\n%clean\nrm -rf %{buildroot}\n%endif\n%files\n/a\n";
    assert_eq!(drop_clean_sections(spec), None);
}

#[test]
fn a_trigger_ends_a_clean_section_and_its_body_is_not_part_of_it() {
    let spec = "%install\nmake\n%clean\nrm -rf %{buildroot}\n%triggerin -- x\necho\n";
    assert_eq!(
        drop_clean_sections(spec).unwrap(),
        "%install\nmake\n%triggerin -- x\necho\n"
    );
}

#[test]
fn the_build_root_removal_that_starts_install_goes() {
    let spec = "%install\nrm -rf %{buildroot}\nmake install\n%files\n/a\n";
    assert_eq!(
        drop_install_buildroot_rm(spec).unwrap(),
        "%install\nmake install\n%files\n/a\n"
    );
    let env = "%install\nrm -rf $RPM_BUILD_ROOT\n%cmake_install\n";
    assert_eq!(
        drop_install_buildroot_rm(env).unwrap(),
        "%install\n%cmake_install\n"
    );
}

#[test]
fn a_build_root_removal_that_is_not_the_first_line_stays() {
    for spec in [
        "%install\n\nrm -rf %{buildroot}\nmake\n",
        "%install\nmake\nrm -rf %{buildroot}\n",
        "%install\nrm -rf %{buildroot}/x\n",
        "%install\n%if 0%{?a}\nrm -rf %{buildroot}\n%endif\n",
    ] {
        assert_eq!(drop_install_buildroot_rm(spec), None, "{spec}");
    }
}

#[test]
fn the_removals_are_idempotent_and_do_not_touch_each_other() {
    let spec = "%install\nrm -rf %{buildroot}\nmake\n\n%clean\nrm -rf %{buildroot}\n\n%files\n/a\n";
    let once = drop_install_buildroot_rm(spec).unwrap();
    assert_eq!(
        drop_clean_sections(spec).unwrap(),
        "%install\nrm -rf %{buildroot}\nmake\n\n%files\n/a\n"
    );
    let twice = drop_clean_sections(&once).unwrap();
    assert_eq!(twice, "%install\nmake\n\n%files\n/a\n");
    assert_eq!(drop_install_buildroot_rm(&twice), None);
    assert_eq!(drop_clean_sections(&twice), None);
}

#[test]
fn the_default_defattr_of_every_files_list_goes() {
    let spec = "%files\n%defattr(-,root,root)\n/a\n\n%files -n libx\n\
                %defattr(-,root,root,-)\n/lib/x.so\n%changelog\n";
    assert_eq!(
        drop_default_defattr(spec).unwrap(),
        "%files\n/a\n\n%files -n libx\n/lib/x.so\n%changelog\n"
    );
    assert_eq!(
        drop_default_defattr(&drop_default_defattr(spec).unwrap()),
        None
    );
}

#[test]
fn a_defattr_with_other_modes_stays() {
    for line in [
        "%defattr(-,root,root,0755)",
        "%defattr(0644,root,root,0755)",
        "%defattr(-,root,nobody,-)",
        "%defattr(-,root,root) # why",
    ] {
        let spec = format!("%files\n{line}\n/a\n");
        assert_eq!(drop_default_defattr(&spec), None, "{line}");
    }
}

#[test]
fn a_defattr_outside_files_or_in_a_conditional_stays() {
    let outside = "%prep\n%defattr(-,root,root)\n%install\n%defattr(-,root,root)\n";
    assert_eq!(drop_default_defattr(outside), None);
    let cond = "%files\n%if 0%{?a}\n%defattr(-,root,root)\n%endif\n/a\n";
    assert_eq!(drop_default_defattr(cond), None);
}

#[test]
fn a_default_defattr_after_a_mode_set_in_the_same_files_list_stays() {
    let spec = "%files\n%defattr(0755,root,root,-)\n/opt/d/a\n%defattr(-,root,root,-)\n/opt/d/b\n";
    assert_eq!(drop_default_defattr(spec), None);
    let attr = "%files\n%attr(0755,root,root) /a\n%defattr(-,root,root)\n/b\n";
    assert_eq!(drop_default_defattr(attr), None);
    let cond =
        "%files\n%if 0%{?a}\n%defattr(0755,root,root,-)\n%endif\n%defattr(-,root,root,-)\n/b\n";
    assert_eq!(drop_default_defattr(cond), None);
}

#[test]
fn a_mode_set_applies_to_its_own_files_list_only() {
    let spec = "%files\n%defattr(0755,root,root,-)\n/a\n\n\
                %files -n libx\n%defattr(-,root,root,-)\n/lib/x.so\n%changelog\n";
    assert_eq!(
        drop_default_defattr(spec).unwrap(),
        "%files\n%defattr(0755,root,root,-)\n/a\n\n%files -n libx\n/lib/x.so\n%changelog\n"
    );
    let attr = "%files\n%attr(0755,root,root) /a\n%files -n libx\n%defattr(-,root,root)\n/b\n";
    assert_eq!(
        drop_default_defattr(attr).unwrap(),
        "%files\n%attr(0755,root,root) /a\n%files -n libx\n/b\n"
    );
}

#[test]
fn only_default_forms_after_default_forms_all_go() {
    let spec = "%files\n%defattr(-,root,root)\n/a\n%defattr(-,root,root,-)\n/b\n";
    assert_eq!(drop_default_defattr(spec).unwrap(), "%files\n/a\n/b\n");
}

#[test]
fn a_trigger_script_with_a_defattr_text_is_not_in_the_files_list() {
    let spec = "%files\n/a\n%triggerin -- x\n%defattr(-,root,root)\necho\n%changelog\n";
    assert_eq!(drop_default_defattr(spec), None);
}

/// The hyprlang spec of X11:Wayland: the hand-written pair for libhyprlang2.
const HYPRLANG: &str = "%prep\n%autosetup -p1\n\n%build\n%cmake\n\n%check\n%ctest\n\n\
    %post   -n lib%{name}%{sover} -p /sbin/ldconfig\n\
    %postun -n lib%{name}%{sover} -p /sbin/ldconfig\n\n\
    %files devel\n%doc README.md\n\n%files -n lib%{name}%{sover}\n%{_libdir}/lib%{name}.so.*\n\n\
    %changelog\n";

#[test]
fn the_hyprlang_pair_is_dropped() {
    assert_eq!(
        drop_ldconfig_pair(HYPRLANG).unwrap(),
        "%prep\n%autosetup -p1\n\n%build\n%cmake\n\n%check\n%ctest\n\n\
         %files devel\n%doc README.md\n\n%files -n lib%{name}%{sover}\n%{_libdir}/lib%{name}.so.*\n\n\
         %changelog\n"
    );
    let once = drop_ldconfig_pair(HYPRLANG).unwrap();
    assert_eq!(drop_ldconfig_pair(&once), None);
}

#[test]
fn the_main_package_pair_is_dropped() {
    let spec = "%install\nmake\n%post -p /sbin/ldconfig\n%postun -p /sbin/ldconfig\n\
                %files\n/usr/lib/a.so\n";
    assert_eq!(
        drop_ldconfig_pair(spec).unwrap(),
        "%install\nmake\n%files\n/usr/lib/a.so\n"
    );
}

#[test]
fn a_pair_that_differs_in_name_or_is_not_ldconfig_stays() {
    for spec in [
        "%post -n libA -p /sbin/ldconfig\n%postun -n libB -p /sbin/ldconfig\n",
        "%post -n libA -p /sbin/ldconfig\n%postun -p /sbin/ldconfig\n",
        "%post -n libA -p /sbin/ldconfig\n%postun -n libA\n",
        "%post -n libA\n%postun -n libA -p /sbin/ldconfig\n",
        "%post -n libA -p /sbin/ldconfig\n%postun -n libA -p /bin/true\n",
        "%post -n libA -p /sbin/ldconfig -q\n%postun -n libA -p /sbin/ldconfig\n",
    ] {
        assert_eq!(drop_ldconfig_pair(spec), None, "{spec}");
    }
}

#[test]
fn a_scriptlet_with_a_body_or_a_second_scriptlet_stays() {
    let body = "%post -n libA -p /sbin/ldconfig\necho hi\n%postun -n libA -p /sbin/ldconfig\n";
    assert_eq!(drop_ldconfig_pair(body), None);
    let second = "%post -n libA -p /sbin/ldconfig\n%post -n libA\necho x\n\
                  %postun -n libA -p /sbin/ldconfig\n";
    assert_eq!(drop_ldconfig_pair(second), None);
    let only_post = "%post -n libA -p /sbin/ldconfig\n%files\n/a\n";
    assert_eq!(drop_ldconfig_pair(only_post), None);
}

#[test]
fn a_pair_inside_a_conditional_or_after_changelog_stays() {
    let cond =
        "%if 0%{?a}\n%post -n libA -p /sbin/ldconfig\n%postun -n libA -p /sbin/ldconfig\n%endif\n";
    assert_eq!(drop_ldconfig_pair(cond), None);
    let log = "%files\n/a\n%changelog\n%post -p /sbin/ldconfig\n%postun -p /sbin/ldconfig\n";
    assert_eq!(drop_ldconfig_pair(log), None);
}

#[test]
fn a_pair_split_by_a_blank_line_goes_without_a_double_blank() {
    let spec =
        "%post -n libA -p /sbin/ldconfig\n\n%postun -n libA -p /sbin/ldconfig\n\n%files\n/a\n";
    assert_eq!(drop_ldconfig_pair(spec).unwrap(), "%files\n/a\n");
}

#[test]
fn the_repositories_that_define_the_ldconfig_macro_are_the_known_new_ones() {
    for (repo, defined) in [
        ("openSUSE_Tumbleweed", true),
        ("openSUSE_Factory", true),
        ("openSUSE_Slowroll", true),
        ("openSUSE_Leap_16.0", true),
        ("SUSE_SLE-16.0", true),
        ("openSUSE_Leap_15.6", false),
        ("SUSE_SLE-15-SP7", false),
        ("openSUSE_Leap_42.3", false),
        ("SUSE_SLE-12-SP5", false),
        ("", false),
    ] {
        assert_eq!(ldconfig_macro_defined(repo), defined, "{repo}");
    }
}

#[test]
fn the_ldconfig_pair_is_a_construct_only_for_a_target_that_defines_the_macro() {
    let spec = "%post -p /sbin/ldconfig\n%postun -p /sbin/ldconfig\n%files\n/a\n";
    let found = constructs(spec, true);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].pass, Pass::Ldconfig);
    assert!(constructs(spec, false).is_empty());
}
