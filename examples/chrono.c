/* chrono: a time-API workout. Simulates a small "event scheduler" that
 * mixes every wall/monotonic clock API, formats real dates, sleeps between
 * events, and reports measured latencies — the kind of output that is
 * different on every run without Weft and must be byte-identical under it.
 *
 * APIs exercised: time, gettimeofday, clock_gettime (REALTIME, MONOTONIC,
 * BOOTTIME, PROCESS_CPUTIME_ID), clock_getres, nanosleep, usleep, sleep,
 * timespec_get, gmtime_r + strftime formatting.
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <time.h>
#include <sys/time.h>
#include <unistd.h>

static uint64_t ts_ns(const struct timespec *ts) {
    return (uint64_t)ts->tv_sec * 1000000000ull + (uint64_t)ts->tv_nsec;
}

int main(void) {
    char datebuf[64];
    struct timespec start_mono, res;

    time_t boot = time(NULL);
    struct tm tm;
    gmtime_r(&boot, &tm);
    strftime(datebuf, sizeof datebuf, "%Y-%m-%d %H:%M:%S", &tm);
    printf("scheduler boot at %s (unix %lld)\n", datebuf, (long long)boot);

    clock_gettime(CLOCK_MONOTONIC, &start_mono);
    clock_getres(CLOCK_REALTIME, &res);
    printf("realtime resolution: %ld ns\n", (long)res.tv_nsec);

    for (int event = 1; event <= 6; event++) {
        struct timeval tv;
        struct timespec mono, wall, cpu;

        gettimeofday(&tv, NULL);
        clock_gettime(CLOCK_MONOTONIC, &mono);
        clock_gettime(CLOCK_REALTIME, &wall);
        clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &cpu);

        uint64_t since_boot_us =
            (ts_ns(&mono) - ts_ns(&start_mono)) / 1000;

        time_t wall_sec = wall.tv_sec;
        gmtime_r(&wall_sec, &tm);
        strftime(datebuf, sizeof datebuf, "%H:%M:%S", &tm);

        printf("event %d: wall=%s.%06ld tod=%lld.%06ld +%lluus cpu=%llu\n",
               event, datebuf, wall.tv_nsec / 1000,
               (long long)tv.tv_sec, (long)tv.tv_usec,
               (unsigned long long)since_boot_us,
               (unsigned long long)ts_ns(&cpu));

        /* Sleep a different way each time around. */
        switch (event % 3) {
        case 0: {
            struct timespec d = { 0, 250 * 1000 * 1000 };
            nanosleep(&d, NULL);
            break;
        }
        case 1:
            usleep(150 * 1000);
            break;
        default:
            sleep(1);
            break;
        }
    }

    struct timespec c11;
    timespec_get(&c11, TIME_UTC);
    struct timespec end_mono;
    clock_gettime(CLOCK_MONOTONIC, &end_mono);
    printf("total virtual elapsed: %llu us, c11 time %lld\n",
           (unsigned long long)((ts_ns(&end_mono) - ts_ns(&start_mono)) / 1000),
           (long long)c11.tv_sec);
    return 0;
}
