#!/usr/bin/env bash

set -o nounset
set -o errexit
set -o errtrace

ktest_verbose=false
ktest_verbosearg=""
ktest_once=false

ssh_retry() {
    (
	set +o errexit

	while true; do
	    ssh -o 'ControlMaster=auto'			\
		-o 'ControlPath=~/.ssh/master-%r@%h:%p'	\
		-o 'ControlPersist=10m'			\
		-o 'ServerAliveInterval=10'		\
		"$@"
	    (($? == 0)) && break
	    sleep 1
	    tput cuu1
	    tput el
	done
    )
}

KTEST_DIR=$(dirname "$(readlink -e "$0")")/..

. $KTEST_DIR/lib/common.sh

usage() {
    echo "test-git-branch.sh: Connect to CI jobserver, run tests, upload results"
    echo "Usage: test-git-branch.sh [options] JOBSERVER"
    echo "      -o              run one test"
    echo "      -v              verbose"
    echo "      -h              display this help and exit"
}

while getopts "ovh" arg; do
    case $arg in
	o)
	    ktest_once=true
	    ;;
	v)
	    ktest_verbose=true
	    ktest_verbosearg=-v
	    ;;
	h)
	    usage
	    exit 0
	    ;;
    esac
done
shift $(( OPTIND - 1 ))

JOBSERVER=$1

source <(ssh_retry $JOBSERVER cat .ktestrc)

JOBSERVER_LINUX_REPO=ssh://$JOBSERVER/$JOBSERVER_HOME/linux
HOSTNAME=$(uname -n)
WORKDIR=$(basename $(pwd))

server_has_mem()
{
    readarray -t -d ' ' free_info < <(ssh_retry $JOBSERVER free|awk '/Mem:/ {print $2 " " $3}')

    local mem_total=${free_info[0]}
    local mem_used=${free_info[1]}

    ((mem_used < mem_total / 2 ))
}

wait_for_server_mem()
{
    while ! server_has_mem; do
	echo "waiting for server load to go down"
	sleep 1
    done
}

git_fetch() {
    local repo=$1
    shift

    (
	set +o errexit

	while true; do
	    wait_for_server_mem

	    git fetch ssh://$JOBSERVER/$JOBSERVER_HOME/$repo $@
	    ret=$?
	    (($ret == 0)) && break
	    (($ret == 1)) && exit 1
	    (($ret == 128)) && exit 1
	    echo "git fetch returned $ret"
	    sleep 1
	done
    )
}

sync_git_repos() {
    local repo

    for repo in ${JOBSERVER_GIT_REPOS[@]}; do
	(cd ~/$repo; git_fetch $repo && git checkout -f FETCH_HEAD) || true > /dev/null
    done
}

run_test_job() {
    BRANCH="$1"
    COMMIT="$2"
    TEST_PATH="$3"
    shift 3
    SUBTESTS=("$@")

    FULL_TEST_PATH=${KTEST_DIR}/tests/$TEST_PATH
    TEST_NAME=${TEST_PATH%%.ktest}
    TEST_NAME=$(echo "$TEST_NAME"|tr / .)

    OUTPUT=$JOBSERVER_OUTPUT_DIR/$COMMIT

    if [[ -z $BRANCH ]]; then
	echo "Error getting test job: need git branch"
	exit 1
    fi

    if [[ -z $COMMIT ]]; then
	echo "Error getting test job: need git commit"
	exit 1
    fi

    if [[ -z $FULL_TEST_PATH ]]; then
	echo "Error getting test job: need test to run"
	exit 1
    fi

    echo "Running test $TEST_NAME for branch $BRANCH and commit $COMMIT"

    run_quiet "Syncing git repos" sync_git_repos

    run_quiet "Fetching $COMMIT" git_fetch linux $COMMIT
    run_quiet "Checking out $COMMIT" git checkout -f FETCH_HEAD

    [[ $(git rev-parse HEAD) != $COMMIT ]] && exit 1

    for i in ktest-out/*; do
	[[ "$i" != ktest-out/kernel* ]] && rm -rf "$i"
    done

    mkdir -p ktest-out/out

    # Mark tests as not run:
    for t in ${SUBTESTS[@]}; do
	t=$(echo "$t"|tr / .)

	mkdir ktest-out/out/$TEST_NAME.$t
	echo "IN PROGRESS" > ktest-out/out/$TEST_NAME.$t/status
    done

    make -C "$KTEST_DIR/lib" supervisor

    while (( ${#SUBTESTS[@]} )); do
	rm -rf ktest-out/gcov.*

	FULL_LOG=$TEST_NAME.$HOSTNAME.$(date -Iseconds).log

	for t in ${SUBTESTS[@]}; do
	    FNAME=$(echo "$t"|tr / .)
	    ln -sfr "ktest-out/out/$FULL_LOG.br"			\
		"ktest-out/out/$TEST_NAME.$FNAME/full_log.br"
	done

	echo "Running test $TEST_PATH ${SUBTESTS[@]}"

	$KTEST_DIR/lib/supervisor -T 1200 -f "$FULL_LOG" -S -F	\
	    -b $TEST_NAME -o ktest-out/out				\
	    -- build-test-kernel run -P $FULL_TEST_PATH ${SUBTESTS[@]} &
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


	echo "Sending results to jobserver"
	mv ktest-out/out ktest-out/$COMMIT
	(
	    set +o errexit
	    cd ktest-out

	    while true; do
		tar --create --file - $COMMIT|
		    ssh_retry $JOBSERVER "(cd $JOBSERVER_OUTPUT_DIR; tar --extract --file -)"
		(($? == 0)) && break
		sleep 1
		tput cuu1
		tput el
	    done
	)
	mv ktest-out/$COMMIT ktest-out/out

	if [[ -d ktest-out/gcov.0 ]]; then
	    echo "Sending gcov results to jobserver"

	    LCOV=ktest-out/out/lcov.partial.$TEST_NAME.$HOSTNAME.$(date -Iseconds)
	    lcov --capture --quiet --directory ktest-out/gcov.0 --output-file $LCOV
	    sed -i -e "s_$(pwd)/__" $LCOV

	    scp $LCOV $JOBSERVER:$OUTPUT

	    ssh_retry $JOBSERVER "(cd $OUTPUT; touch lcov-stale)"
	    ssh_retry $JOBSERVER "update-lcov $COMMIT"
	fi

	ssh_retry $JOBSERVER gen-commit-summary $COMMIT

	SUBTESTS=( "${SUBTESTS_REMAINING[@]}" )
    done
}

while true; do
    echo "Getting test job"

    while true; do
	TEST_JOB=( $(ssh_retry $JOBSERVER get-test-job $ktest_verbosearg $HOSTNAME $WORKDIR) )

	[[ ${#TEST_JOB[@]} != 0 && ${TEST_JOB[0]} == TEST_JOB ]] && break

	echo "test-git-branch: No test job available"
	$ktest_once && exit 1

	sleep 10
    done

    TEST_JOB=("${TEST_JOB[@]:1}")
    echo "Got job ${TEST_JOB[@]}"

    (run_test_job "${TEST_JOB[@]}") || sleep 10

    $ktest_once && break
done
