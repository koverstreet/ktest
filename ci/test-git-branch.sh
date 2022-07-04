#!/bin/bash

JOBSERVER=$1

source <(ssh $JOBSERVER cat .ktestrc bin/_test-git-branch.sh)
