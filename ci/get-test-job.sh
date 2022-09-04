#!/bin/bash

[[ -f ~/.ktestrc ]] && . ~/.ktestrc

cd $JOBSERVER_HOME/linux
flock --nonblock .git_fetch.lock git fetch --all > /dev/null

make -C ~/ktest/lib get-test-job 1>&2

~/ktest/lib/get-test-job -b ~/BRANCHES-TO-TEST -o $JOBSERVER_OUTPUT_DIR/c
