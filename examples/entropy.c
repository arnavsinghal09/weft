/* entropy: a multithreaded entropy-source workout. Four worker threads
 * concurrently pull randomness from getrandom(2), open(2)+read(2) on
 * /dev/urandom, and fopen(3)+fread(3) on /dev/urandom, while stamping
 * virtual time — exactly the concurrent pressure the shim's thread-safety
 * must survive (Phase 1 guarantee: the *aggregate* draw is deterministic;
 * per-thread attribution waits for the Phase 2 scheduler, so all output
 * below is commutative across threads: sums, xors, and counts).
 *
 * Also exercises getentropy(3) and getauxval(AT_RANDOM) from the main
 * thread, where full determinism is expected.
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/random.h>
#include <sys/time.h>
#include <unistd.h>

#define THREADS 4
#define ROUNDS 200
#define CHUNK 512

struct acc {
    uint64_t xor_all;
    uint64_t sum_all;
    uint64_t bytes;
    uint64_t clock_sum_us;
};

static void mix(struct acc *a, const unsigned char *buf, size_t n) {
    for (size_t i = 0; i + 8 <= n; i += 8) {
        uint64_t v;
        memcpy(&v, buf + i, 8);
        a->xor_all ^= v;
        a->sum_all += v;
    }
    a->bytes += n;
}

static void *worker(void *arg) {
    struct acc *a = arg;
    unsigned char buf[CHUNK];

    int fd = open("/dev/urandom", O_RDONLY);
    FILE *f = fopen("/dev/urandom", "rb");

    for (int r = 0; r < ROUNDS; r++) {
        struct timeval tv;
        gettimeofday(&tv, NULL);
        a->clock_sum_us += (uint64_t)tv.tv_usec;

        if (getrandom(buf, CHUNK, 0) == CHUNK) mix(a, buf, CHUNK);
        if (fd >= 0 && read(fd, buf, CHUNK) == CHUNK) mix(a, buf, CHUNK);
        if (f && fread(buf, 1, CHUNK, f) == CHUNK) mix(a, buf, CHUNK);
    }

    if (f) fclose(f);
    if (fd >= 0) close(fd);
    return NULL;
}

int main(void) {
    const unsigned char *auxr = (const unsigned char *)getauxval(AT_RANDOM);
    printf("AT_RANDOM = ");
    for (int i = 0; i < 16; i++) printf("%02x", auxr ? auxr[i] : 0);
    printf("\n");

    unsigned char ent[32];
    if (getentropy(ent, sizeof ent) == 0) {
        uint64_t v = 0;
        memcpy(&v, ent, 8);
        printf("getentropy head = %016llx\n", (unsigned long long)v);
    }

    pthread_t tids[THREADS];
    struct acc accs[THREADS];
    memset(accs, 0, sizeof accs);
    for (int i = 0; i < THREADS; i++)
        pthread_create(&tids[i], NULL, worker, &accs[i]);

    struct acc total = {0, 0, 0, 0};
    for (int i = 0; i < THREADS; i++) {
        pthread_join(tids[i], NULL);
        total.xor_all ^= accs[i].xor_all;
        total.sum_all += accs[i].sum_all;
        total.bytes += accs[i].bytes;
        total.clock_sum_us += accs[i].clock_sum_us;
    }

    /* Everything printed is commutative across thread interleavings. */
    printf("combined: bytes=%llu xor=%016llx sum=%016llx\n",
           (unsigned long long)total.bytes,
           (unsigned long long)total.xor_all,
           (unsigned long long)total.sum_all);
    /* The virtual clock hands out ticks 1..N atomically, so the multiset of
     * stamps — and hence this sum — is interleaving-invariant too. */
    printf("clock stamps: count=%d usec_sum=%llu\n", THREADS * ROUNDS,
           (unsigned long long)total.clock_sum_us);
    return 0;
}
