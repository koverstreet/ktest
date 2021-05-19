#include <getopt.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
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

static pid_t child;
static bool print_timeout = true;

static void alarm_handler(int sig)
{
	char msg[] = "TEST TIMEOUT\n";
	kill(child, SIGKILL);
	if (print_timeout)
		write(STDOUT_FILENO, msg, strlen(msg));
	_exit(EXIT_FAILURE);
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
	     "      -h              Display this help and exit\n");
}

int main(int argc, char *argv[])
{
	bool exit_on_success = false;
	bool exit_on_failure = false;
	unsigned long timeout = 0;
	int opt, ret = EXIT_FAILURE;
	struct timespec start, ts;

	setlinebuf(stdin);
	setlinebuf(stdout);

	if (clock_gettime(CLOCK_MONOTONIC, &start))
		die("clock_gettime error: %m");

	while ((opt = getopt(argc, argv, "SFT:h")) != -1) {
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
		case 'h':
			usage();
			exit(EXIT_SUCCESS);
		case '?':
			usage();
			exit(EXIT_FAILURE);
		}
	}

	int pipefd[2];
	if (pipe(pipefd))
		die("error creating pipe: %m");

	child = fork();
	if (child < 0)
		die("fork error: %m");

	if (!child) {
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

		execvp(argv[optind], argv + optind);
		die("error execing %s: %m", argv[optind]);
	}

	struct sigaction alarm_action = { .sa_handler = alarm_handler };
	if (sigaction(SIGALRM, &alarm_action, NULL))
		die("sigaction error: %m");

	if (timeout)
		alarm(timeout);

	FILE *childf = fdopen(pipefd[0], "r");
	if (!childf) {
		fprintf(stderr, "fdopen error: %m\n");
		goto out;
	}

	size_t n = 0, len;
	char *line = NULL;

	while ((len = getline(&line, &n, childf)) >= 0) {
		if (clock_gettime(CLOCK_MONOTONIC, &ts)) {
			fprintf(stderr, "clock_gettime error: %m\n");
			break;
		}

		unsigned long elapsed = ts.tv_sec - start.tv_sec;
		printf("%.5lu ", elapsed);
		fputs(line, stdout);

		if (exit_on_success &&
		    strstr(line, "TEST SUCCESS")) {
			ret = 0;
			break;
		}

		if (exit_on_failure && strstr(line, "TEST FAILED"))
			break;

		if (exit_on_failure && strstr(line, "Kernel panic")) {
			/* Read output for five more seconds, then exit */
			print_timeout = false;
			alarm(5);
		}
	}
out:
	kill(child, SIGKILL);
	exit(ret);
}
