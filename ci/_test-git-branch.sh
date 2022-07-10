#!/bin/bash

set -o nounset
set -o errexit
set -o errtrace

KTEST_DIR=$(dirname "$(readlink -e "$0")")/..
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

echo "Getting test job"

TEST_JOB=( $(ssh $JOBSERVER get-test-job.sh) )

BRANCH=${TEST_JOB[0]}
COMMIT=${TEST_JOB[1]}
TEST_PATH=${TEST_JOB[2]}
TEST_NAME=$(basename -s .ktest $TEST_PATH)
SUBTESTS=( "${TEST_JOB[@]:3}" )

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

sync_git_repos
git_fetch linux $COMMIT
git checkout FETCH_HEAD

git_fetch linux ci-monkeypatch
git merge --no-edit FETCH_HEAD || git reset --hard

rm -rf ktest-out/out
mkdir -p ktest-out/out

# Mark tests as not run:
for t in ${SUBTESTS[@]}; do
    t=$(echo "$t"|tr / .)

    mkdir ktest-out/out/$TEST_NAME.$t
    echo "========= NOT STARTED" > ktest-out/out/$TEST_NAME.$t/status
done

make -C "$KTEST_DIR/lib" supervisor

while (( ${#SUBTESTS[@]} )); do
    FULL_LOG=$TEST_NAME.$(hostname).$(date -Iseconds).log

    for t in ${SUBTESTS[@]}; do
	FNAME=$(echo "$t"|tr / .)
	ln -sfr "ktest-out/out/$FULL_LOG.br"			\
	    "ktest-out/out/$TEST_NAME.$FNAME/full_log.br"
    done

    $KTEST_DIR/lib/supervisor -T 1200 -f "$FULL_LOG" -S -F	\
	-b $TEST_NAME -o ktest-out/out				\
	build-test-kernel run $TEST_PATH ${SUBTESTS[@]} || true

    SUBTESTS_REMAINING=()

    for t in ${SUBTESTS[@]:1}; do
	FNAME=$(echo "$t"|tr / .)

	if grep -q "NOT STARTED" ktest-out/out/$TEST_NAME.$FNAME/status; then
	    SUBTESTS_REMAINING+=($t)
	fi
    done

    SUBTESTS=( "${SUBTESTS_REMAINING[@]}" )
done

find ktest-out/out -type f -name \*log -print0|xargs -0 brotli --rm -9

OUTPUT=$JOBSERVER_OUTPUT_DIR/c/$COMMIT
ssh $JOBSERVER mkdir -p $OUTPUT
scp -r ktest-out/out/* $JOBSERVER:$OUTPUT

echo "Running test-job-done.sh"
ssh $JOBSERVER test-job-done.sh $BRANCH $COMMIT
