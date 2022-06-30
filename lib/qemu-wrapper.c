#define _GNU_SOURCE

#include <ctype.h>
#include <getopt.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

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

static pid_t child;
static int childfd;
static char *testname;

static void alarm_handler(int sig)
{
	char *msg = mprintf("========= FAILED TIMEOUT %s\n",
			    testname ?: "(no test)");

	if (write(childfd, msg, strlen(msg)) != strlen(msg))
		die("write error in alarm handler");
	free(msg);
}

static void usage(void)
{
	puts("qemu-wrapper - wrapper for qemu to catch test success/failure\n"
	     "Usage: qemu-wrapper [OPTIONS] -- <qemu-command>\n"
	     "\n"
	     "Options\n"
	     "      -S              Exit on success\n"
	     "      -F              Exit on failure\n"
	     "      -T TIMEOUT      Timeout after TIMEOUT seconds\n"
	     "      -b name         base name for log files\n"
	     "      -o dir          output directory for log files\n"
	     "      -h              Display this help and exit\n");
}

static char *log_path(const char *logdir, const char *basename, const char *testname)
{
	if (!basename)
		basename = "out";

	return !testname
		? mprintf("%s/%s", logdir, basename)
		: mprintf("%s/%s.%s", logdir, basename, testname);
}

static FILE *log_open(const char *logdir, const char *basename, const char *testname)
{
	char *path = log_path(logdir, basename, testname);

	FILE *f = fopen(path, "w");
	if (!f)
		die("error opening %s: %m", path);

	free(path);
	setlinebuf(f);
	return f;
}

static void strim(char *line)
{
	char *p = line;

	while (!iscntrl(*p))
		p++;
	*p = 0;
}

static const char *str_starts_with(const char *str, const char *prefix)
{
	unsigned len = strlen(prefix);

	if (strncmp(str, prefix, len))
		return NULL;
	return str + len;
}

static char *test_starts(const char *line)
{
	const char *testname = str_starts_with(line, "========= TEST   ");
	char *ret, *p;

	if (!testname)
		return NULL;

	ret = strdup(testname);

	while ((p = strchr(ret, '/')))
		*p = '.';

	return ret;
}

static FILE *popen_with_pid(char *argv[], pid_t *child)
{
	int pipefd[2];
	if (pipe(pipefd))
		die("error creating pipe: %m");

	*child = fork();
	if (*child < 0)
		die("fork error: %m");

	if (!*child) {
		if (dup2(pipefd[1], STDOUT_FILENO) < 0)
			die("dup2 error: %m");
		if (dup2(pipefd[1], STDERR_FILENO) < 0)
			die("dup2 error: %m");
		close(pipefd[1]);

		int devnull = open("/dev/null", O_RDONLY);
		if (devnull < 0)
			die("error opening /dev/null; %m");
		if (dup2(devnull, STDIN_FILENO) < 0)
			die("dup2 error: %m");
		close(devnull);

		execvp(argv[0], argv);
		die("error execing %s: %m", argv[0]);
	}

	childfd = pipefd[1];

	FILE *childf = fdopen(pipefd[0], "r");
	if (!childf)
		die("fdopen error: %m");

	return childf;
}

static void update_watchdog(const char *line)
{
	const char *new_watchdog = str_starts_with(line, "WATCHDOG ");
	if (new_watchdog)
		alarm(atoi(new_watchdog));
}

static char *output_line(const char *line, struct timespec start)
{
	struct timespec ts;

	if (clock_gettime(CLOCK_MONOTONIC, &ts))
		die("clock_gettime error: %m");

	unsigned long elapsed = ts.tv_sec - start.tv_sec;

	return mprintf("%.5lu %s\n", elapsed, line);
}

static bool test_ends(char *line)
{
	return  str_starts_with(line, "========= PASSED ") ||
		str_starts_with(line, "========= FAILED ");
}

int main(int argc, char *argv[])
{
	bool exit_on_success = false;
	bool exit_on_failure = false;
	unsigned long timeout = 0;
	int opt, ret = EXIT_FAILURE;
	struct timespec start;
	char *logdir = NULL;
	char *basename = NULL;

	setlinebuf(stdin);
	setlinebuf(stdout);

	if (clock_gettime(CLOCK_MONOTONIC, &start))
		die("clock_gettime error: %m");

	while ((opt = getopt(argc, argv, "SFT:b:o:h")) != -1) {
		switch (opt) {
		case 'S':
			exit_on_success = true;
			break;
		case 'F':
			exit_on_failure = true;
			break;
		case 'T':
			errno = 0;
			timeout = strtoul(optarg, NULL, 10);
			if (errno)
				die("error parsing timeout: %m");
			break;
		case 'b':
			basename = strdup(optarg);
			break;
		case 'o':
			logdir = strdup(optarg);
			break;
		case 'h':
			usage();
			exit(EXIT_SUCCESS);
		case '?':
			usage();
			exit(EXIT_FAILURE);
		}
	}

	if (!logdir)
		die("Required option -o missing");

	FILE *childf = popen_with_pid(argv + optind, &child);

	FILE *logfile = log_open(logdir, basename, NULL);
	FILE *test_logfile = NULL;

	size_t n = 0;
	ssize_t len;
	char *line = NULL;

	struct sigaction alarm_action = { .sa_handler = alarm_handler };
	if (sigaction(SIGALRM, &alarm_action, NULL))
		die("sigaction error: %m");

	if (timeout)
		alarm(timeout);
again:
	while ((len = getline(&line, &n, childf)) >= 0) {
		strim(line);

		char *output = output_line(line, start);

		update_watchdog(line);

		testname = test_starts(line);

		if (test_logfile && testname) {
			fclose(test_logfile);
			test_logfile = NULL;
			free(testname);
			testname = NULL;
		}

		if (test_logfile)
			fputs(output, test_logfile);
		fputs(output, logfile);
		fputs(output, stdout);

		if (test_logfile && test_ends(line)) {
			fclose(test_logfile);
			test_logfile = NULL;
		}

		if (testname) {
			test_logfile = log_open(logdir, basename, testname);
			free(testname);
			testname = NULL;
		}

		if (exit_on_failure && str_starts_with(line, "TEST FAILED"))
			break;

		if (exit_on_failure && strstr(line, "FAILED TIMEOUT"))
			break;

		if (exit_on_success && str_starts_with(line, "TEST SUCCESS")) {
			ret = 0;
			break;
		}

		if (exit_on_failure &&
		    (strstr(line, "Kernel panic") ||
		     strstr(line, "BUG")))
			alarm(5);

		free(output);
	}

	if (len == -1 && errno == EINTR) {
		clearerr(childf);
		goto again;
	}

	kill(child, SIGKILL);
	exit(ret);
}
