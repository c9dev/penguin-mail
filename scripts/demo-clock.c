/* A preload that moves the wall clock by DEMO_CLOCK_OFFSET seconds, for
 * screenshots taken at a better hour than the machine's. The demo seeds its
 * mail and events relative to now, so a shifted clock moves every seed, every
 * "Today" and "Yesterday" label and the Next card together. Only
 * scripts/screenshots.sh loads it, and only into the demo it starts.
 *
 *   cc -shared -fPIC -O2 -o demo-clock.so demo-clock.c -ldl
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdlib.h>
#include <sys/time.h>
#include <time.h>

static long offset(void) {
    static long value;
    static int read;
    if (!read) {
        const char *text = getenv("DEMO_CLOCK_OFFSET");
        value = text ? atol(text) : 0;
        read = 1;
    }
    return value;
}

int clock_gettime(clockid_t clock, struct timespec *at) {
    static int (*real)(clockid_t, struct timespec *);
    if (!real) real = dlsym(RTLD_NEXT, "clock_gettime");
    int status = real(clock, at);
    if (status == 0 && (clock == CLOCK_REALTIME || clock == CLOCK_REALTIME_COARSE))
        at->tv_sec += offset();
    return status;
}

int gettimeofday(struct timeval *at, void *zone) {
    static int (*real)(struct timeval *, void *);
    if (!real) real = dlsym(RTLD_NEXT, "gettimeofday");
    int status = real(at, zone);
    if (status == 0 && at) at->tv_sec += offset();
    return status;
}

time_t time(time_t *out) {
    struct timespec at;
    clock_gettime(CLOCK_REALTIME, &at);
    if (out) *out = at.tv_sec;
    return at.tv_sec;
}
