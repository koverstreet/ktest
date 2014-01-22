Summary: ktest kernel testing tool
Name: ktest
Version: 0.1
Release: %{?release:%{release}}%{!?release:eng}
Source0: %{name}-%{version}.tar.gz
License: Datera
Group: tools
BuildRoot: %{_tmppath}/%{name}-root
Requires: realpath, minicom, genisoimage, socat
BuildRequires: qemu, kvm, qemu-kvm, linux-bcache, dfs-server

%description
kernel testing tool
