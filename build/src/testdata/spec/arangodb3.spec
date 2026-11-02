#
# spec file for package arangodb3
#
# Copyright (c) 2021 SUSE LLC
#
# All modifications and additions to the file contributed by third parties
# remain the property of their copyright owners, unless otherwise agreed
# upon. The license for this file, and modifications and additions to the
# file, is the same license as for the pristine package itself (unless the
# license for the pristine package is not an Open Source License, in which
# case the license is the MIT License). An "Open Source License" is a
# license that conforms to the Open Source Definition (Version 1.9)
# published by the Open Source Initiative.

# Please submit bugfixes or comments via https://bugs.opensuse.org/
#


# "backport"
%{!?_initddir:    %{expand: %%global _initddir    %{_initrddir}}}
%{!?_rundir:      %{expand: %%global _rundir      /run}}
# these two are broken on SLE 11... so just "correct" them enough to work...
%{!?_tmpfilesdir: %{expand: %%global _tmpfilesdir %{_prefix}/lib/tmpfiles.d}}
%{!?_sysctldir:   %{expand: %%global _sysctldir   %{_prefix}/lib/sysctl.d}}

Name:           arangodb3
%define srcname arangodb
Version:        3.10.4
%define srcversion v%{version}
Release:        0
Summary:        The multi-model NoSQL database
License:        Apache-2.0
Group:          Productivity/Databases
Url:            https://github.com/%{srcname}/%{srcname}
Source0:        https://github.com/%{srcname}/%{srcname}/archive/%{srcversion}/%{srcname}-%{srcversion}.tar.gz
# embedded V8 doesn't build anywhere else :-(
ExclusiveArch:  x86_64
Patch1:         0001-return-value-from-non-void-function.patch
Patch2:         0002-Fix-pidfile-path.patch
Patch3:         0003-Add-inline-with-SNAPPY_ATTRIBUTE_ALWAYS_INLINE.patch
Patch4:         0004-expression-is-not-const.patch
Patch5:         0005-ShellConsoleFeature-is-incomplete-class.patch
Patch6:         0006-EncryptionFeature-is-enterprise-only.patch
Patch7:         0007-drop-static_assert-as-they-re-non-const.patch
Patch100:       0001-drop-constexpr.patch

BuildRoot:      %{_tmppath}/%{name}-%{version}-build
BuildRequires:  cmake
BuildRequires:  fdupes

BuildRequires:  gcc
BuildRequires:  gcc-c++
BuildRequires:  git
BuildRequires:  libunwind-devel
BuildRequires:  openssl-devel
BuildRequires:  systemd
%{?systemd_requires}
BuildRequires:  xz-devel
BuildRequires:  zlib-devel

Requires:       logrotate

%description
ArangoDB is a native multi-model database with flexible data models for
documents, graphs, and key-values. Build high performance applications
using a convenient SQL-like query language or JavaScript extensions. 


%prep
%setup -q -n %{srcname}-%{srcversion}
%patch1 -p1
%patch2 -p1
%patch3 -p1
%patch4 -p1
%patch5 -p1
%patch6 -p1
%patch7 -p1
cd 3rdParty/iresearch
%patch100 -p1
cd ../..

# some tests check for "g++" in $PATH
mkdir -p ~/bin
ln -sf /usr/bin/gcc-1* ~/bin/gcc
ln -sf /usr/bin/g++-1* ~/bin/g++

%build
export PATH=~/bin:$PATH
%cmake -DUSE_FAIL_ON_WARNINGS=off
%cmake_build

%install
export PATH=~/bin:$PATH
%cmake_install

# fix install locations, defaults to /usr/usr/... resp. /usr/var/...

mkdir -p %{buildroot}%{_unitdir}
mv %{buildroot}/usr%{_unitdir}/%{name}.service %{buildroot}%{_unitdir}/%{name}.service
rm -rf %{buildroot}/usr/usr

# /usr/var -> /var
mkdir -p %{buildroot}%{_localstatedir}/lib
mkdir -p %{buildroot}%{_localstatedir}/log
mv %{buildroot}/usr%{_localstatedir}/lib/* %{buildroot}%{_localstatedir}/lib
mv %{buildroot}/usr%{_localstatedir}/log/* %{buildroot}%{_localstatedir}/log
rm -rf %{buildroot}/usr%{_localstatedir}

# licenses in /usr/share/doc/arangodb3

rm -rf %{buildroot}%{_datadir}/doc/%{name}

%fdupes %{buildroot}%{_datadir}/%{name}

%pre
%service_add_pre %{name}.service

## create %%{srcname} group and user
getent group %{srcname} >/dev/null || groupadd -r %{srcname}
getent passwd %{srcname} >/dev/null || useradd -r -g %{srcname} -d %{_localstatedir}/lib/%{name} -s /sbin/nologin -c "service user for %{srcname}" %{srcname}
exit 0

%post
%service_add_post %{name}.service


%preun
%service_del_preun %{name}.service

%postun
%service_del_postun %{name}.service

# only execute in case of package removal, not on upgrade
if [ $1 -eq 0 ] ; then
  getent passwd %{srcname} > /dev/null
  if [ "$?" == "0" ] ; then
    userdel %{srcname}
  fi

  getent group %{srcname} >/dev/null
  if [ "$?" == "0" ] ; then
    groupdel %{srcname}
  fi
fi

%files
%defattr(-,root,root)

%license LICENSE
%license LICENSES-OTHER-COMPONENTS.md
%doc README
%doc README.md

%dir %{_datadir}/%{name}
%{_datadir}/%{name}

%dir %{_localstatedir}/lib/%{name}
%dir %{_localstatedir}/lib/%{name}-apps
%{_localstatedir}/lib/%{name}
%{_localstatedir}/lib/%{name}-apps

%dir %{_localstatedir}/log/%{name}

%config(noreplace) %{_sysconfdir}/logrotate.d/%{name}
%dir /usr%{_sysconfdir}
%dir /usr%{_sysconfdir}/%{name}
%config(noreplace) /usr%{_sysconfdir}/%{name}/*

%{_unitdir}/%{name}.service
%{_bindir}/*
%{_sbindir}/*

%{_mandir}/man1/*
%{_mandir}/man8/*

%dir %attr(755,%{srcname},%{srcname}) %{_localstatedir}/lib/%{name}
%dir %attr(755,%{srcname},%{srcname}) %{_localstatedir}/log/%{name}

%changelog
