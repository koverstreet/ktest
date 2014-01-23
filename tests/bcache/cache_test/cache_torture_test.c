#include <errno.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <fcntl.h>
#include <string.h>
#include <linux/fs.h>
#include <sys/ioctl.h>
#include <sys/types.h>
#include <sys/stat.h>

#define KB 1024

void die (const char *function, const char *format, ... ) {

	if (errno != 0)
		perror(function);

	va_list args;
	va_start(args, format);
	vprintf(format, args);
	va_end(args);

	exit(1);
}

int main(int argc, char** argv) {

	errno = 0;

	if (argc != 2)
		die(NULL, "error: Please provide a device to write to.\n");

	int fd = open(argv[1], O_RDWR);

	if (fd <= -1)
		die("open", "error: Can't open device (%s).\n", argv[1]);

	size_t numSect;
	if(ioctl(fd, BLKGETSIZE, &numSect) == -1)
		die("ioctl-BLKGETSIZE", "error: getting blk device size failed\n");
	printf("num sectors: %d\n", (int) numSect);

	size_t blkSize;
	if(ioctl(fd, BLKSSZGET, &blkSize) == -1)
		die("ioclt-BLKSSZGET", "error: getting blk device size failed\n");
	printf("block size: %d\n", (int) blkSize);

	size_t diskSize = numSect * 512;

	char buf[9];
	buf[8] = '\0';
	char word[9] = "DEADBEEF";
	int i, j;

	printf("Writing first pass ... \n");
	for (i = 0; i < diskSize / 8; i++) {
		if (pwrite(fd, word, 8, i*8) < 0)
			die("pwrite", "error: failure on first pass write\n");
	}

	printf("Checking first pass ... \n");
	for (i = 0; i < diskSize / 8; i++) {
		if (pread(fd, buf, 8, i*8) < 0)
			die("pread", "error: failure on first pass read\n");

		if (strcmp(buf, word))
			die("pread", "error: discrepancy found @ byte: %d\n", i*8);
	}
	printf("First pass done.\n");

	printf("Writing 2nd pass ... \n");
	for (i = 0; i < diskSize / (128*KB); i++) {
		if (pwrite(fd, "MEAT", 4, i*128*KB + 4) < 0)
			die("pwrite", "error: failure on 2nd pass write\n");
	}

	strcpy(word + 4, "MEAT");
	printf("Checking 2nd pass ... \n");
	for (i = 0; i < diskSize / (128*KB); i++) {
		if (pread(fd, buf, 8, i*128*KB) < 0)
			die("pread", "error: failure on 2nd pass read\n");

		if (strcmp(buf, word))
			die("pread", "error: discrepancy found @ byte: %d\n", i*8);
	}
	printf("Second pass done.\n");

	printf("Writing 3rd pass ... \n");
	for (i = 0; i < diskSize / (128*KB); i++) {
		if (pwrite(fd, "BEAT", 4, i*128*KB + 4) < 0)
			die("pwrite", "error: failure on 3rd pass write\n");
	}

	strcpy(word + 4, "BEAT");
	printf("Checking 3rd pass ... \n");
	for (i = 0; i < diskSize / (128*KB); i++) {
		if (pread(fd, buf, 8, i*128*KB) < 0)
			die("pread", "error: failure on 3rd pass read\n");

		if (strcmp(buf, word))
			die("pread", "error: discrepancy found @ byte: %d\n", i*8);
	}
	printf("Third pass done.\n");

	printf("Starting torture test ...\n");

	for (j = 'A'; j <= 'Z'; j++) {
		fprintf(stderr,"%c ", j);
		word[0] = j;
		for (i = 0; i < diskSize / (128*KB); i++) {
			if (pwrite(fd, word, 8, i*128*KB + 512*j) < 0)
				die("pwrite", "error: failure on %c write\n", j);
		}
	}

	printf("\nFinished.\n");

	close(fd);
}
