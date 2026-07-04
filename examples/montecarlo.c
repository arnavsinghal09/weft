/* montecarlo: a tight-loop randomness workout (millions of PRNG calls).
 * Estimates pi twice — with the rand() family seeded via the classic
 * srand(time(NULL)) pattern, and with drand48() — then Fisher-Yates
 * shuffles an array with random(), hammers rand_r with a caller-owned
 * state, and prints estimates plus an FNV-1a checksum over the shuffle.
 *
 * This is also the overhead benchmark: ~5 million interposed calls in a
 * hot loop, so per-call shim cost dominates its runtime.
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <time.h>

#define DARTS 2000000
#define SHUFFLE_N 1000

static uint64_t fnv1a(const int *xs, int n) {
    uint64_t h = 1469598103934665603ull;
    for (int i = 0; i < n; i++) {
        h ^= (uint64_t)(uint32_t)xs[i];
        h *= 1099511628211ull;
    }
    return h;
}

int main(void) {
    srand((unsigned)time(NULL)); /* the classic nondeterminism source */

    long in_circle = 0;
    for (long i = 0; i < DARTS; i++) {
        double x = (double)rand() / RAND_MAX;
        double y = (double)rand() / RAND_MAX;
        if (x * x + y * y <= 1.0) in_circle++;
    }
    printf("pi(rand)    = %.6f\n", 4.0 * (double)in_circle / DARTS);

    srand48(time(NULL));
    in_circle = 0;
    for (long i = 0; i < DARTS; i++) {
        double x = drand48(), y = drand48();
        if (x * x + y * y <= 1.0) in_circle++;
    }
    printf("pi(drand48) = %.6f\n", 4.0 * (double)in_circle / DARTS);

    int deck[SHUFFLE_N];
    for (int i = 0; i < SHUFFLE_N; i++) deck[i] = i;
    for (int i = SHUFFLE_N - 1; i > 0; i--) {
        int j = (int)(random() % (i + 1));
        int t = deck[i]; deck[i] = deck[j]; deck[j] = t;
    }
    printf("shuffle checksum = %016llx head=[%d %d %d %d %d]\n",
           (unsigned long long)fnv1a(deck, SHUFFLE_N),
           deck[0], deck[1], deck[2], deck[3], deck[4]);

    unsigned rr_state = 12345;
    uint64_t rr_acc = 0;
    for (long i = 0; i < 1000000; i++) rr_acc += (uint64_t)rand_r(&rr_state);
    printf("rand_r acc = %llu, lrand48 next = %ld, mrand48 next = %ld\n",
           (unsigned long long)rr_acc, lrand48(), mrand48());
    return 0;
}
