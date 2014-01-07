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

uint64_t nr_blocks = 1024 * 1024 * 4;
int fd;
io_context_t ioctx;

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

	while (1) {
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
		if (ret < 0 && ret != -EAGAIN)
			printf("io_submit() error %i\n", ret);

	}

	return NULL;
}

int main(int argc, char **argv)
{
	pthread_t threads[4];
	unsigned i;

	memset(threads, 0, sizeof(pthread_t) * 4);

#if 0
	if (argc != 2) {
		printf("Specify a file/device to test against\n");
		exit(EXIT_FAILURE);
	}

	fd = open(argv[1], O_RDONLY|O_DIRECT);
#else
	fd = open("/dev/zero", O_RDONLY);
#endif
	if (fd < 0) {
		perror("Open error");
		exit(EXIT_FAILURE);
	}

	//nr_blocks = getblocks(fd);

	if (io_setup(128, &ioctx)) {
		perror("Error creating io context");
		exit(EXIT_FAILURE);
	}

	for (i = 0; i < 8; i++)
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
	io_destroy(ioctx);
	printf("io_destroy done\n");

	exit(EXIT_SUCCESS);
}
