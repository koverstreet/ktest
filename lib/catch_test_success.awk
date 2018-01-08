#!/usr/bin/awk -f

BEGIN { starttime = systime() }

{
    printf("%.4d %s\n", systime() - starttime, $0);
    fflush();
    if ($0 ~ /TEST SUCCESS/) {
	exit 7
    } else if ($0 ~ /TEST FAILED/) {
	exit 0
    }
}
