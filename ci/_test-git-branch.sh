#!/usr/bin/env bash

set -o nounset
set -o errexit
set -o errtrace

ktest_verbose=0

KTEST_DIR=$(dirname "$(readlink -e "$0")")/..
JOBSERVER_LINUX_REPO=ssh://$JOBSERVER/$JOBSERVER_HOME/linux

. $KTEST_DIR/lib/common.sh

ssh() {
    (
	set +o errexit

	while true; do
	    env ssh "$@"
	    (($? == 0)) && break
	    sleep 1
	    tput cuu1
	    tput el
	done
    )
}

git_fetch()
{
    local repo=$1
    shift

    (
	set +o errexit

	while true; do
	    git fetch ssh://$JOBSERVER/$JOBSERVER_HOME/$repo $@
	    (($? == 0)) && break
	    sleep 1
	done
    )
}

sync_git_repos()
{
    local repo

    for repo in ${JOBSERVER_GIT_REPOS[@]}; do
	(cd ~/$repo; git_fetch $repo && git checkout -f FETCH_HEAD) || true > /dev/null
    done
}

echo "Getting test job"

while true; do
    TEST_JOB=( $(ssh $JOBSERVER get-test-job) )

    [[ ${#TEST_JOB[@]} != 0 ]] && break

    sleep 30
done

BRANCH=${TEST_JOB[0]}
COMMIT=${TEST_JOB[1]}
TEST_PATH=${TEST_JOB[2]}
TEST_NAME=$(basename -s .ktest $TEST_PATH)
SUBTESTS=( "${TEST_JOB[@]:3}" )
OUTPUT=$JOBSERVER_OUTPUT_DIR/$COMMIT

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

echo "Running test $TEST_NAME for branch $BRANCH and commit $COMMIT"

run_quiet "Syncing git repos" sync_git_repos

run_quiet "Fetching $COMMIT" git_fetch linux $COMMIT
run_quiet "Checking out $COMMIT" git checkout FETCH_HEAD

rm -rf ktest-out/out
mkdir -p ktest-out/out

# Mark tests as not run:
for t in ${SUBTESTS[@]}; do
    t=$(echo "$t"|tr / .)

    mkdir ktest-out/out/$TEST_NAME.$t
    echo "IN PROGRESS" > ktest-out/out/$TEST_NAME.$t/status
done

make -C "$KTEST_DIR/lib" supervisor

while (( ${#SUBTESTS[@]} )); do
    FULL_LOG=$TEST_NAME.$(hostname).$(date -Iseconds).log

    for t in ${SUBTESTS[@]}; do
	FNAME=$(echo "$t"|tr / .)
	ln -sfr "ktest-out/out/$FULL_LOG.br"			\
	    "ktest-out/out/$TEST_NAME.$FNAME/full_log.br"
    done

    echo "Running test $TEST_NAME ${SUBTESTS[@]}"

    $KTEST_DIR/lib/supervisor -T 1200 -f "$FULL_LOG" -S -F	\
	-b $TEST_NAME -o ktest-out/out				\
	-- build-test-kernel run $TEST_PATH ${SUBTESTS[@]} > /dev/null &
    wait

    SUBTESTS_REMAINING=()

    t=${SUBTESTS[0]}
    FNAME=$(echo "$t"|tr / .)

    if grep -q "IN PROGRESS" ktest-out/out/$TEST_NAME.$FNAME/status; then
	echo "NOT STARTED" > ktest-out/out/$TEST_NAME.$FNAME/status
    fi

    for t in ${SUBTESTS[@]:1}; do
	FNAME=$(echo "$t"|tr / .)

	if grep -q "IN PROGRESS" ktest-out/out/$TEST_NAME.$FNAME/status; then
	    SUBTESTS_REMAINING+=($t)
	fi
    done

    echo "Compressing output"
    find ktest-out/out -type f -name \*log -print0|xargs -0 brotli --rm -9

    ssh $JOBSERVER mkdir -p $OUTPUT

    echo "Sending results to jobserver"
    (cd ktest-out/out; tar --create --file - *)|
	ssh $JOBSERVER "(cd $OUTPUT; tar --extract --file -)"

    ssh $JOBSERVER gen-commit-summary $COMMIT

    SUBTESTS=( "${SUBTESTS_REMAINING[@]}" )
done
