#!/bin/bash

[[ -f ~/.ktestrc ]] && . ~/.ktestrc

# Clean stale test jobs:
cd $JOBSERVER_OUTPUT_DIR
find -size 0 -cmin +180 |xargs rm -f > /dev/null

cd $JOBSERVER_HOME/linux
flock --nonblock .git_fetch.lock git fetch --all > /dev/null

make -C ~/ktest/lib get-test-job 1>&2

~/ktest/lib/get-test-job -b ~/BRANCHES-TO-TEST -o ~/web/c
