#define _GNU_SOURCE

#include <ctype.h>
#include <getopt.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#define HAVE_STATEMENT_EXPR 1
#include "darray/darray.h"

static char *outdir = NULL;
static char *branches_to_test = NULL;
static bool verbose = false;

#define die(msg, ...)					\
do {							\
	fprintf(stderr, msg "\n", ##__VA_ARGS__);	\
	exit(EXIT_FAILURE);				\
} while (0)

static char *mprintf(const char *fmt, ...)
{
	va_list args;
	char *str;
	int ret;

	va_start(args, fmt);
	ret = vasprintf(&str, fmt, args);
	va_end(args);

	if (ret < 0)
		die("insufficient memory");

	return str;
}

static void strim(char *line)
{
	char *p = line;

	while (isalnum(*p))
		p++;
	*p = 0;
}

static char *test_basename(const char *str)
{
	char *p = strrchr(str, '/');
	char *ret = strdup(p ? p + 1 : str);

	p = strstr(ret, ".ktest");
	if (p)
		*p = 0;
	return ret;
}

typedef darray(char *) strings;

static void strings_free(strings *strs)
{
	char **s;

	darray_foreach(s, *strs)
		free(*s);
	darray_free(*strs);

	memset(strs, 0, sizeof(*strs));
}

typedef struct {
	char		*branch;
	char		*commit;
	unsigned	age;
	char		*test;
	strings		subtests;
} test_job;

static void test_job_free(test_job *job)
{
	free(job->branch);
	free(job->commit);
	free(job->test);
	strings_free(&job->subtests);
	memset(job, 0, sizeof(*job));
}

static strings get_subtests(char *test_path)
{
	darray_char output;
	strings ret;
	size_t bytes_read;

	darray_init(output);
	darray_init(ret);

	char *cmd = mprintf("%s list-tests", test_path);
	FILE *f = popen(cmd, "r");
	free(cmd);

	if (!f)
		die("error executing %s", test_path);

	do {
		darray_make_room(output, 4096);

		bytes_read = fread(output.item + output.size,
				   1, 4096, f);
		output.size += bytes_read;
	} while (bytes_read);

	pclose(f);

	char *subtest, *p = output.item;
	while ((subtest = strtok(p, " \t\n"))) {
		darray_push(ret, strdup(subtest));
		p = NULL;
	}

	darray_free(output);

	if (darray_empty(ret))
		die("error getting subtests from %s", test_path);

	return ret;
}

static bool lockfile_exists(const char *commit,
			    const char *test_path,
			    const char *subtest,
			    bool create)
{
	char *test_name = test_basename(test_path);
	char *commitdir = mprintf("%s/%s", outdir, commit);
	char *testdir = mprintf("%s/%s.%s", commitdir, test_name, subtest);
	char *lockfile = mprintf("%s/status",
				 testdir, subtest);
	bool exists;

	if (!create) {
		exists = access(lockfile, F_OK) == 0;
	} else {
		mkdir(commitdir, 0755);
		mkdir(testdir, 0755);
		int fd = open(lockfile, O_RDWR|O_CREAT|O_EXCL, 0644);
		exists = fd < 0;
		if (!exists)
			close(fd);
	}

	free(lockfile);
	free(testdir);
	free(commitdir);
	free(test_name);

	return exists;
}

static test_job branch_get_next_test_job(char *branch,
					 char *test_path,
					 strings subtests)
{
	char *cmd = mprintf("git log --pretty=format:%H %s", branch);
	FILE *commits = popen(cmd, "r");
	char *commit = NULL;
	size_t n = 0;
	ssize_t len;
	test_job ret;

	memset(&ret, 0, sizeof(ret));

	while ((len = getline(&commit, &n, commits)) >= 0) {
		strim(commit);

		char **subtest;
		darray_foreach(subtest, subtests)
			if (!lockfile_exists(commit, test_path, *subtest, false)) {
				darray_push(ret.subtests, strdup(*subtest));
				if (darray_size(ret.subtests) > 20)
					break;
			}

		if (!darray_empty(ret.subtests)) {
			ret.branch	= strdup(branch);
			ret.commit	= strdup(commit);
			ret.test	= strdup(test_path);
			goto success;
		}

		ret.age++;
	}
	fprintf(stderr, "error looking up commits on branch %s\n", branch);
success:
	fclose(commits);
	free(commit);
	free(cmd);
	return ret;
}

static test_job get_best_test_job()
{
	FILE *branches = fopen(branches_to_test, "r");
	char *line = NULL;
	size_t n = 0;
	ssize_t len;
	test_job best;

	memset(&best, 0, sizeof(best));

	if (!branches)
		die("error opening %s: %m", branches_to_test);

	while ((len = getline(&line, &n, branches)) >= 0) {
		char *branch	= strtok(line, " \t\n");
		char *test_path	= strtok(NULL, " \t\n");

		if (!branch || !test_path)
			continue;

		strings subtests = get_subtests(test_path);

		if (verbose)
			fprintf(stderr, "branch %s test %s\n", branch, test_path);

		test_job job = branch_get_next_test_job(branch, test_path, subtests);

		strings_free(&subtests);

		if (!best.branch || job.age < best.age) {
			test_job_free(&best);
			best = job;
		} else {
			test_job_free(&job);
		}
	}

	if (!best.branch)
		die("Nothing found");

	fclose(branches);
	free(line);
	return best;
}

void usage(void)
{
	puts("get-test-job: get a test job and create lockfile\n"
	     "Usage: get-test-job [OPTIONS]\n"
	     "\n"
	     "Options\n"
	     "      -b file         List of branches to test and tests to run\n"
	     "      -o dir          Directory for tests results\n"
	     "      -v              Verbose\n"
	     "      -h              Display this help and exit");
	exit(EXIT_SUCCESS);
}

int main(int argc, char *argv[])
{
	int opt;
	test_job job;
	strings subtests;
	char **subtest;

	memset(&job, 0, sizeof(job));

	while ((opt = getopt(argc, argv, "b:o:vh")) != -1) {
		switch (opt) {
		case 'b':
			branches_to_test = strdup(optarg);
			break;
		case 'o':
			outdir = strdup(optarg);
			break;
		case 'v':
			verbose = true;
			break;
		case 'h':
			usage();
			exit(EXIT_SUCCESS);
		case '?':
			usage();
			exit(EXIT_FAILURE);
		}
	}

	if (!branches_to_test || !outdir)
		die("required argument missing");

	do {
		test_job_free(&job);
		job = get_best_test_job();

		if (verbose)
			fprintf(stderr, "got %s %s %s age %u\n",
				job.branch, job.commit, job.test, job.age);

		darray_init(subtests);

		darray_foreach(subtest, job.subtests)
			if (!lockfile_exists(job.commit, job.test, *subtest, true))
				darray_push(subtests, *subtest);
	} while (darray_empty(subtests));

	printf("%s %s %s", job.branch, job.commit, job.test);
	darray_foreach(subtest, subtests)
		printf(" %s", *subtest);
	printf("\n");
}
