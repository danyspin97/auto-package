#
# spec file for package VulkanMemoryAllocator
#
# Copyright (c) 2026 SUSE LLC and contributors
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


Name:           VulkanMemoryAllocator
Version:        3.4.0
Release:        0
Summary:        Vulkan memory allocation library
License:        MIT
Group:          Development/Libraries/C and C++
URL:            https://github.com/GPUOpen-LibrariesAndSDKs/VulkanMemoryAllocator
Source:         https://github.com/GPUOpen-LibrariesAndSDKs/VulkanMemoryAllocator/archive/refs/tags/v%version.tar.gz
BuildRequires:  c++_compiler
BuildRequires:  cmake
BuildRequires:  vulkan-headers
BuildArch:      noarch

%description
This library can aid developers in managing memory allocations and
resource creation by offering some higher-level functions for Vulkan
memory management.

%package devel
Summary:        Vulkan memory allocation library
Requires:       vulkan-headers >= 1.3

%description devel
This library can aid developers in managing memory allocations and
resource creation by offering some higher-level functions for Vulkan
memory management.

* Functions that help to choose correct and optimal memory type
  based on intended usage of the memory.
* Functions that allocate memory blocks, reserve and return parts
  of them (VkDeviceMemory + offset + size) to the user.
* Functions that can create an image/buffer, allocate memory for
  it and bind them together - all in one call.

%prep
%autosetup

%build
%cmake
%cmake_build

%install
%cmake_install

%files devel
%license LICENSE.txt
%_includedir/*
%_datadir/cmake/

%changelog
