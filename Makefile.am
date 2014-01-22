# Build rules

MAKEFLAGS = --jobs=3
# don't want to overdue it with vms running in parallel

export VERBOSE=1
bin_SCRIPTS=ktest
check_SCRIPTS=runtest 

TESTS = $(check_SCRIPTS) \
	tests/bcache/bcache.ktest
# note: test scripts must end with .ktest to get called correctly

TEST_EXTENSIONS=.ktest
KTEST_LOG_COMPILER=$(srcdir)/ktest.testrunner
