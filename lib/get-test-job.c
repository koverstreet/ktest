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

static char *outdir = NULL;
static char *branches_to_test = NULL;

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

static char *test_basename(char *str)
{
	char *p = strrchr(str, '/');
	char *ret = strdup(p ? p + 1 : str);

	p = strstr(ret, ".ktest");
	if (p)
		*p = 0;
	return ret;
}

static void branch_get_next_commit_and_test(char *branch,
					    char **ret_commit,
					    char *test,
					    unsigned *age)
{
	char *cmd = mprintf("git log --pretty=format:%H %s", branch);
	FILE *commits = popen(cmd, "r");
	char *commit = NULL;
	size_t n = 0;
	ssize_t len;

	*age = 0;

	while ((len = getline(&commit, &n, commits)) >= 0) {
		strim(commit);

		char *lockfile = mprintf("%s/%s/%s", outdir, commit, test);
		bool exists = access(lockfile, F_OK) == 0;
		free(lockfile);

		if (!exists) {
			*ret_commit = strdup(commit);
			goto success;
		}

		(*age)++;
	}
	fprintf(stderr, "error looking up commits on branch %s\n", branch);
success:
	fclose(commits);
	free(commit);
	free(cmd);
}

static void get_best_branch_commit_test(char **ret_branch,
					char **ret_commit,
					char **ret_test)
{
	FILE *branches = fopen(branches_to_test, "r");
	char *line = NULL;
	size_t n = 0;
	ssize_t len;
	unsigned best_age = 0;

	if (!branches)
		die("error opening %s: %m", branches_to_test);

	*ret_branch	= NULL;
	*ret_commit	= NULL;
	*ret_test	= NULL;

	while ((len = getline(&line, &n, branches)) >= 0) {
		char *commit	= NULL;
		char *branch	= strtok(line, " \t\n");
		char *test	= strtok(NULL, " \t\n");
		char *testname;
		unsigned age;

		if (!branch || !test)
			continue;

		testname = test_basename(test);
		//fprintf(stderr, "branch %s test %s\n", branch, test);

		branch_get_next_commit_and_test(branch, &commit, testname, &age);
		free(testname);

		if (!*ret_branch || age < best_age) {
			free(*ret_branch);
			free(*ret_commit);
			free(*ret_test);

			*ret_branch	= strdup(branch);
			*ret_test	= strdup(test);
			*ret_commit	= commit;
			best_age	= age;
		}
	}

	if (!*ret_branch)
		die("Nothing found");

	fclose(branches);
	free(line);
}

void usage(void)
{
	puts("get-test-job: get a test job and create lockfile");
	exit(EXIT_SUCCESS);
}

int main(int argc, char *argv[])
{
	char *branch, *commit, *test, *testname;
	bool created;
	int opt;

	while ((opt = getopt(argc, argv, "b:o:")) != -1) {
		switch (opt) {
		case 'b':
			branches_to_test = strdup(optarg);
			break;
		case 'o':
			outdir = strdup(optarg);
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
		get_best_branch_commit_test(&branch, &commit, &test);

		//fprintf(stderr, "got %s %s %s\n", branch, commit, test);

		char *commitdir = mprintf("%s/%s", outdir, commit);
		mkdir(commitdir, 0755);

		testname = test_basename(test);

		char *lockfile = mprintf("%s/%s", commitdir, testname);

		//fprintf(stderr, "lockfile %s\n", lockfile);

		int fd = open(lockfile, O_RDWR|O_CREAT|O_EXCL, 0644);
		fprintf(stderr, "fd %i\n", fd);
		created = fd >= 0;

		free(commitdir);
		free(lockfile);
	} while (!created);

	printf("%s %s %s\n", branch, commit, test);
}
