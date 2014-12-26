Summary: ktest kernel testing tool
Name: %{package_name}
Version: %{datera_version}
Release: %{?release:%{release}}%{!?release:eng}
Source0: %{name}-%{version}.tar.gz
License: Datera
Group: tools
BuildRoot: %{_tmppath}/%{name}-root
Requires: minicom, genisoimage, socat
BuildRequires: qemu, kvm, qemu-kvm, qemu-system-x86, linux-bcache, vde2, vde2-slirp, lwipv6

%description
kernel testing tool

%install
make DESTDIR=%buildroot INSTALL=/usr/bin/install -C /bld/$RPM_PACKAGE_NAME install


%files
%_bindir/vm-start
%_bindir/testy
%_bindir/vm-start-new
