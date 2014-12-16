Summary: ktest kernel testing tool
Name: ktest
Version: 0.1
Release: %{?release:%{release}}%{!?release:eng}
Source0: %{name}-%{version}.tar.gz
License: Datera
Group: tools
BuildRoot: %{_tmppath}/%{name}-root
Requires: minicom, genisoimage, socat, lwipv6, libvdeplug3-devel
BuildRequires: qemu, kvm, qemu-kvm, qemu-system-x86, linux-bcache, vde2, vde2-slirp

%description
kernel testing tool
