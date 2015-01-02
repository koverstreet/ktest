#define _GNU_SOURCE
#define _LARGEFILE_SOURCE
#define _FILE_OFFSET_BITS	64

#include <errno.h>
#include <fcntl.h>
#include <linux/fs.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <sys/types.h>
#include <sys/stat.h>
#include <unistd.h>

#include <libaio.h>
#include <pthread.h>

#define NR_WORKERS	4

uint64_t nr_blocks = 1024 * 1024 * 4;
io_context_t ioctx;
int fd, exiting;

uint64_t getblocks(int fd)
{
	uint64_t ret;
	struct stat statbuf;
	if (fstat(fd, &statbuf)) {
		perror("stat error\n");
		exit(EXIT_FAILURE);
	}
	ret = statbuf.st_size / 512;
	if (S_ISBLK(statbuf.st_mode))
		if (ioctl(fd, BLKGETSIZE, &ret)) {
			perror("ioctl error");
			exit(EXIT_FAILURE);
		}
	return ret / 8;
}

static void *iothread(void *p)
{
	char __attribute__((aligned(4096))) buf[4096];
	unsigned seed = 0;

	while (!exiting) {
		struct iocb iocb[64];
		struct iocb *iocbp[64];
		unsigned i;
		int ret;

		memset(iocb, 0, sizeof(struct iocb) * 64);

		for (i = 0; i < 64; i++) {
			uint64_t offset = rand_r(&seed);

			iocb[i].aio_lio_opcode = IO_CMD_PREAD;
			iocb[i].aio_fildes = fd;

			iocb[i].u.c.buf = buf;
			iocb[i].u.c.nbytes = 4096;
			iocb[i].u.c.offset = (offset % nr_blocks) * 4096;

			iocbp[i] = &iocb[i];
		}

		ret = io_submit(ioctx, 64, iocbp);
		if (ret < 0 && ret != -EAGAIN) {
			printf("io_submit() error %i\n", ret);
			exit(EXIT_FAILURE);
		}
	}

	return NULL;
}

int main(int argc, char **argv)
{
	pthread_t threads[NR_WORKERS];
	unsigned i;
	int flags = 0;

	memset(threads, 0, sizeof(threads));

	if (argc != 2) {
		printf("Specify a file/device to test against\n");
		exit(EXIT_FAILURE);
	}

	if (strcmp(argv[1], "/dev/zero"))
		flags = O_DIRECT;

	fd = open(argv[1], O_RDONLY|flags);
	if (fd < 0) {
		perror("Open error");
		exit(EXIT_FAILURE);
	}

	//nr_blocks = getblocks(fd);

	if (io_setup(128, &ioctx)) {
		perror("Error creating io context");
		exit(EXIT_FAILURE);
	}

	for (i = 0; i < NR_WORKERS; i++)
		if (pthread_create(&threads[i], NULL, iothread, NULL)) {
			printf("pthread_create() error\n");
			exit(EXIT_FAILURE);
		}

	for (i = 0; i < 1000 * 1000;) {
		struct timespec timeout;
		struct io_event events[256];
		int ret;

		timeout.tv_sec = 0;
		timeout.tv_nsec = 10000;

		ret = io_getevents(ioctx, 1, 256, events, &timeout);
		if (ret < 0)
			printf("io_getevents error\n");
		else
			i += ret;

	}

	printf("exiting\n");
	exiting = 1;

	for (i = 0; i < NR_WORKERS; i++)
		pthread_join(threads[i], NULL);

	io_destroy(ioctx);
	printf("io_destroy done\n");

	exit(EXIT_SUCCESS);
}
