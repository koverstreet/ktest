#!/bin/bash

set -o nounset
set -o errexit
set -o errtrace

JOBSERVER_LINUX_REPO=ssh://$JOBSERVER/$JOBSERVER_HOME/linux

git_fetch()
{
    local repo=$1
    shift

    git fetch ssh://$JOBSERVER/$JOBSERVER_HOME/$repo $@
}

sync_git_repos()
{
    local repo

    for repo in ${JOBSERVER_GIT_REPOS[@]}; do
	(cd ~/$repo; git_fetch $repo; git checkout -f FETCH_HEAD) > /dev/null
    done
}

while true; do
    echo "Getting test job"

    TEST_JOB=( $(ssh $JOBSERVER get-test-job.sh) )

    BRANCH=${TEST_JOB[0]}
    COMMIT=${TEST_JOB[1]}
    TEST_PATH=${TEST_JOB[2]}
    TEST_NAME=$(basename -s .ktest $TEST_PATH)

    if [[ -z $BRANCH ]]; then
	echo "Error getting test job: need git branch"
	exit 1
    fi

    if [[ -z $COMMIT ]]; then
	echo "Error getting test job: need git commit"
	exit 1
    fi

    if [[ -z $TEST_PATH ]]; then
	echo "Error getting test job: need test to run"
	exit 1
    fi

    echo "Running test $TEST_PATH on commit $COMMIT from branch $BRANCH"

    sync_git_repos
    git_fetch linux $COMMIT
    git checkout FETCH_HEAD

    git_fetch linux ci-monkeypatch
    git merge --no-edit FETCH_HEAD

    mkdir -p ktest-out
    rm -rf ktest-out/out

    build-test-kernel run $TEST_PATH || true

    if [[ -f ktest-out/out/$TEST_NAME ]]; then
	echo "Test $TEST_NAME completed"
    else
	echo "Test $TEST_NAME failed to start"
	echo "TEST FAILED" > "ktest-out/out/$TEST_NAME"
    fi

    for log in $(find ktest-out/out -name log); do
	tail -n1 "$log" > $(dirname "$log")/status
	brotli --rm -9 "$log"
    done

    brotli --rm -9 ktest-out/out/$TEST_NAME

    OUTPUT=$JOBSERVER_OUTPUT_DIR/c/$COMMIT
    ssh $JOBSERVER mkdir -p $OUTPUT
    scp -r ktest-out/out/* $JOBSERVER:$OUTPUT

    echo "Running test-job-done.sh"
    ssh $JOBSERVER test-job-done.sh $BRANCH $COMMIT
done
