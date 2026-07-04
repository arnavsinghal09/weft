/* race_bank: a genuine lost-update race on a shared counter.
 *
 * The bug is a *split critical section*: `deposit` reads the balance under
 * the lock, releases it, then re-acquires the lock to write back read+1. The
 * read-modify-write is therefore NOT atomic — two threads can both read the
 * same balance B and both store B+1, losing one increment. This is a real
 * bug pattern: it appears when someone refactors a "get" and a "set" that
 * were once a single locked section, or copies the value out to "minimize
 * time under lock."
 *
 * With N threads each depositing M times, a correct program always ends at
 * N*M. Any lost update makes the final balance smaller.
 *
 * Crucially, the two critical-section boundaries (the unlock after the read
 * and the lock before the write) are scheduler yield points, so Weft's
 * deterministic scheduler can interleave two threads' read/modify/write and
 * either trigger or avoid the race purely as a function of the seed.
 *
 * Usage: race_bank [threads] [iters]   (defaults: 4 threads, 25 iters)
 * Exit code: 0 if balance == expected (no lost update), 1 otherwise.
 */
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>

static long balance = 0;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static int iters = 25;

static void *worker(void *arg) {
    (void)arg;
    for (int i = 0; i < iters; i++) {
        /* --- critical section 1: read --- */
        pthread_mutex_lock(&lock);
        long seen = balance;
        pthread_mutex_unlock(&lock);

        /* The window between the two sections is where another thread can
         * sneak its own read+write in and get clobbered. No artificial work
         * is needed: the unlock/lock are themselves the yield points. */

        /* --- critical section 2: write back --- */
        pthread_mutex_lock(&lock);
        balance = seen + 1;
        pthread_mutex_unlock(&lock);
    }
    return NULL;
}

int main(int argc, char **argv) {
    int nthreads = (argc > 1) ? atoi(argv[1]) : 4;
    if (argc > 2) iters = atoi(argv[2]);
    if (nthreads < 1) nthreads = 1;
    if (nthreads > 64) nthreads = 64;

    pthread_t tids[64];
    for (int i = 0; i < nthreads; i++)
        pthread_create(&tids[i], NULL, worker, NULL);
    for (int i = 0; i < nthreads; i++)
        pthread_join(tids[i], NULL);

    long expected = (long)nthreads * iters;
    long lost = expected - balance;
    printf("threads=%d iters=%d expected=%ld balance=%ld lost=%ld\n",
           nthreads, iters, expected, balance, lost);
    return balance == expected ? 0 : 1;
}
