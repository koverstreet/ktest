KERNEL VIRTUAL MACHINE TESTING TOOLS:
=====================================

This repository contains some infrastructure for running Linux kernel tests
inside a virtual machine, primarily for bcachefs.

The tools will launch a virtual machine (using qemu), complete with networking
and scratch block devices, run the test(s) inside the vm with test output on
standard output, and then kill and cleanup the VM when the test(s) complete (in
noninteractive mode) or when killed via ctrl-C.

Tests themselves are bash scripts that declare their dependencies (kernel config
options, scratch devices, timeouts, etc).

DEPENDENCIES:
=============

 * standand build tools (gcc, make)
 * qemu
 * minicom
 * socat
 * vde2 (for vde_swich and slirpvde, used for user mode networking)
 * liblwipv6 (optional dependency for user mode networking)

You'll need an ssh key in $HOME/.ssh for build-test-kernel ssh to work; it adds
your public key to the vm's authorized_keys.

ktest should work on any Linux distribution.

GETTING STARTED:
================

You'll need to build a root filesystem image for the virtual machines. As root,
run:

```
root_image create
```

This creates a root image and sticks it in /var/lib/ktest.

Then, to build a kernel and run some tests, from your linux kernel source tree
run

```
build-test-kernel run -I ~/ktest/tests/bcachefs/single_device.ktest
```

While virtual machine is running, you can interact with it by running various
other build-test-kernel subcommands from same directory, e.g.:

```
build-test-kernel ssh
build-test-kernel kgdb
```

TOOLS:
------

Symlink the ones you're using somewhere into your path - there's no install
procedure, they just expect to be run out of the git repository.

 * build-test-kernel

   This is what you'll use most of the time for interactive kernel development.
   It expects to be run from a Linux kernel source tree - it builds it, and runs
   the specified test.


Normal usage:

```
$ build-test-kernel run -I ~/ktest/tests/bcache/xfstests.ktest
```

   run builds a kernel and runs the specified test; there are other subcommands
   for interacting with a running test VM (sshing in, using kgdb, etc.).

   -I enables interactive mode (disables timeouts, enables kgdb instead of crash
   dumps)

 * ktest

   This is what build-test-kernel calls into after building a kernel. ktest
   takes a kernel image and a test, and parses the test and figures out the
   correct parameters for running the virtual machine

 * testy

   Simple tool for parsing "test lists" - files that specify which tests apply
   to which source files/directories, and optionally running the appropriate
   tests.

TESTS:
======

Tests are bash scripts; they specify dependencies/requirements (test libraries,
binaries, kernel config options, etc.) by calling various predefined bash
functions.

By default, tests are shell functions that start with test. You can define tests
differently by defining the shell functions list_tests and run_test.



AUTOMATION:
===========

Output logs go in ktest-out/out/; full output from a run of a .ktest test file
goes in
  ktest-out/out/$basename

Individual tests go in
  ktest-out/out/$basename.$testname

Success or failure will be the very last line of the per-test logfiles.
