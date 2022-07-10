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

static struct timespec xclock_gettime(clockid_t clockid)
{
	struct timespec ts;

	if (clock_gettime(clockid, &ts))
		die("clock_gettime error: %m");
	return ts;
}

static pid_t child;
static int childfd;

static unsigned long	default_timeout;
static unsigned long	timeout;

static char		*logdir;
static char		*test_basename;
static char		*full_log;

static char		*current_test;
static struct timespec	current_test_start;
static FILE		*current_test_log;

static void alarm_handler(int sig)
{
	char *msg = mprintf("========= FAILED TIMEOUT %s in %lus\n",
			    current_test ?: "(no test)", timeout);

	if (write(childfd, msg, strlen(msg)) != strlen(msg))
		die("write error in alarm handler");
	free(msg);
}

static void set_timeout(unsigned long new_timeout)
{
	timeout = new_timeout;
	alarm(new_timeout);
}

static FILE *test_file_open(const char *fname)
{
	char *path = mprintf("%s/%s.%s/%s", logdir, test_basename,
			     current_test, fname);

	FILE *f = fopen(path, "w");
	if (!f)
		die("error opening %s: %m", path);

	free(path);
	setlinebuf(f);
	return f;
}

static FILE *log_open()
{
	char *path = mprintf("%s/%s", logdir, full_log);
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

static char *test_is_starting(const char *line)
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

static bool test_is_ending(char *line)
{
	return  str_starts_with(line, "========= PASSED ") ||
		str_starts_with(line, "========= FAILED ") ||
		str_starts_with(line, "========= NOTRUN");
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

static void read_watchdog(const char *line)
{
	const char *new_watchdog = str_starts_with(line, "WATCHDOG ");
	if (new_watchdog)
		set_timeout(atol(new_watchdog));
}

static void write_test_file(const char *file, const char *fmt, ...)
{
	va_list args;
	FILE *f = test_file_open(file);

	va_start(args, fmt);
	vfprintf(f, fmt, args);
	va_end(args);

	fclose(f);
}

static void test_start(char *new_test, struct timespec now)
{
	free(current_test);
	current_test		= new_test;
	current_test_start	= now;
	current_test_log	= test_file_open("log");

	write_test_file("status", "TEST FAILED\n");

	set_timeout(default_timeout);
}

static void test_end(struct timespec now)
{
	write_test_file("duration", "%li", now.tv_sec - current_test_start.tv_sec);

	fclose(current_test_log);
	current_test_log = NULL;

	set_timeout(default_timeout);
}

static void usage(void)
{
	puts("supervisor - test supervisor"
	     "Usage: supervisor [OPTIONS] -- <test-command>\n"
	     "\n"
	     "Options\n"
	     "      -S              Exit on success\n"
	     "      -F              Exit on failure\n"
	     "      -T TIMEOUT      Timeout after TIMEOUT seconds\n"
	     "      -b name         base name for log files\n"
	     "      -o dir          output directory for log files\n"
	     "      -h              Display this help and exit");
}

int main(int argc, char *argv[])
{
	bool exit_on_success = false;
	bool exit_on_failure = false;
	int opt, ret = EXIT_FAILURE;
	struct timespec start;

	setlinebuf(stdin);
	setlinebuf(stdout);

	if (clock_gettime(CLOCK_MONOTONIC, &start))
		die("clock_gettime error: %m");

	while ((opt = getopt(argc, argv, "SFT:b:o:f:h")) != -1) {
		switch (opt) {
		case 'S':
			exit_on_success = true;
			break;
		case 'F':
			exit_on_failure = true;
			break;
		case 'T':
			errno = 0;
			default_timeout = strtoul(optarg, NULL, 10);
			if (errno)
				die("error parsing timeout: %m");
			break;
		case 'b':
			test_basename = strdup(optarg);
			break;
		case 'f':
			full_log = strdup(optarg);
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

	if (!test_basename)
		die("Required option -b missing");

	if (!logdir)
		die("Required option -o missing");

	FILE *childf = popen_with_pid(argv + optind, &child);

	FILE *logfile = log_open();

	size_t n = 0;
	ssize_t len;
	char *line = NULL;

	struct sigaction alarm_action = { .sa_handler = alarm_handler };
	if (sigaction(SIGALRM, &alarm_action, NULL))
		die("sigaction error: %m");

	set_timeout(default_timeout);
again:
	while ((len = getline(&line, &n, childf)) >= 0) {
		struct timespec now = xclock_gettime(CLOCK_MONOTONIC);

		strim(line);

		char *output = mprintf("%.5lu %s\n", now.tv_sec - start.tv_sec, line);

		read_watchdog(line);

		char *new_test = test_is_starting(line);

		/* If a test is starting, close logfile for previous test: */
		if (current_test_log && new_test)
			test_end(now);

		if (new_test)
			test_start(new_test, now);

		if (current_test_log)
			fputs(output, current_test_log);
		fputs(output, logfile);
		fputs(output, stdout);

		if (current_test_log && test_is_ending(line)) {
			write_test_file("status", "%s\n", line);
			test_end(now);
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
