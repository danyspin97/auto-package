#
# spec file for package galera-clustercheck
#
# Copyright (c) 2015 SUSE LINUX Products GmbH, Nuernberg, Germany.
#
# All modifications and additions to the file contributed by third parties
# remain the property of their copyright owners, unless otherwise agreed
# upon. The license for this file, and modifications and additions to the
# file, is the same license as for the pristine package itself (unless the
# license for the pristine package is not an Open Source License, in which
# case the license is the MIT License). An "Open Source License" is a
# license that conforms to the Open Source Definition (Version 1.9)
# published by the Open Source Initiative.

# Please submit bugfixes or comments via http://bugs.opensuse.org/
#

%if 0%{?suse_version} > 1230
%bcond_without systemd
%else
%bcond_with    systemd
%endif

Name:           galera-clustercheck
Version:        0.0.2+git0.8cafd6f
Release:        0
License:        MIT
Summary:        Galera Clustercheck in Ruby - of course!
Url:            https://github.com/darix/galera-clustercheck.0.0+git1444039416.
Group:          Internal
Source:         %{name}-%{version}.tar.xz
#
%define rb_build_versions %{rb_default_ruby}
%define rb_build_abi      %{rb_default_ruby_abi}
#
BuildRequires:  %{rubygem unicorn}
Requires:       rubygem(%{rb_build_abi}:unicorn)
#
BuildRequires:  %{rubygem mysql2}
Requires:       rubygem(%{rb_build_abi}:mysql2)
BuildArch:      noarch
BuildRequires:  xz
%if %{with systemd}
BuildRequires:  pkgconfig(systemd)
%{?systemd_requires}
%endif
BuildRoot:      %{_tmppath}/%{name}-%{version}-build
%define home_dir /var/lib/%{name}

%description
Galera Clustercheck in Ruby - of course!

Run with ruby galera-clustercheck.rb

No cmdline or config handling yet. All needs to be changed in the source.

%prep
%setup -q

%build

%install
install -D -m 0750 galera-clustercheck.rb %{buildroot}%{_sbindir}/galera-clustercheck.rb
install -d -m 0750 %{buildroot}%{_sysconfdir}/%{name}/
install    -m 0640 config.yml %{buildroot}%{_sysconfdir}/%{name}/config.yml
%if %{with systemd}
install -D -m 644 %{name}.service %{buildroot}%{_unitdir}/%{name}.service
ln -s %{_sbindir}/service %{buildroot}%{_sbindir}/rc%{name}
%endif
install -d -m 0750 %{buildroot}%{home_dir}

%pre
/usr/sbin/groupadd -r %{name} >/dev/null 2>&1 || :
/usr/sbin/useradd -g %{name} -s /bin/false -r -c "Galera Cluster Check" -d %{home_dir} %{name} >/dev/null 2>&1 || :
%if %{with systemd}
%service_add_pre %{name}.service

%preun
%service_del_preun %{name}.service

%post
%service_add_post %{name}.service

%postun
%service_del_postun %{name}.service
%endif

%files
%defattr(-,root,root)
%doc README.md
%config(noreplace) %attr(-,root,%{name}) %{_sysconfdir}/%{name}/
%attr(750,root,%{name}) %{_sbindir}/galera-clustercheck.rb
%dir %attr(700,%{name},%{name}) %{home_dir}
%{_sbindir}/rc%{name}
%{_unitdir}/%{name}.service
