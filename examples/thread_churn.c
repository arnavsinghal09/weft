/* thread_churn: many threads with staggered lifetimes and nested locking,
 * used to stress the scheduler's thread lifecycle and lock modeling.
 *
 * - N worker threads are created; worker i runs (i % 5) + 1 iterations, so
 *   threads finish at very different times (some exit almost immediately).
 * - Each iteration takes two mutexes in a *consistent* nested order
 *   (accounts, then audit), so there is no deadlock — the point is to
 *   exercise nested acquisition and unlock ordering, not to fault.
 * - A trylock probe on a third mutex exercises the trylock path.
 *
 * The final tallies are a deterministic function of the seed.
 *
 * Usage: thread_churn [threads]
 */
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>

static pthread_mutex_t accounts = PTHREAD_MUTEX_INITIALIZER;
static pthread_mutex_t audit = PTHREAD_MUTEX_INITIALIZER;
static pthread_mutex_t probe = PTHREAD_MUTEX_INITIALIZER;

static long balance = 0;
static long audit_log = 0;
static long trylock_wins = 0;

static void *worker(void *arg) {
    int id = (int)(long)arg;
    int rounds = (id % 5) + 1;
    for (int r = 0; r < rounds; r++) {
        pthread_mutex_lock(&accounts);
        balance += id + 1;
        pthread_mutex_lock(&audit); /* nested */
        audit_log += 1;
        pthread_mutex_unlock(&audit);
        pthread_mutex_unlock(&accounts);

        if (pthread_mutex_trylock(&probe) == 0) {
            trylock_wins += 1;
            pthread_mutex_unlock(&probe);
        }
    }
    return NULL;
}

int main(int argc, char **argv) {
    int n = (argc > 1) ? atoi(argv[1]) : 12;
    if (n < 1) n = 1;
    if (n > 64) n = 64;

    pthread_t tids[64];
    for (int i = 0; i < n; i++)
        pthread_create(&tids[i], NULL, worker, (void *)(long)i);
    for (int i = 0; i < n; i++)
        pthread_join(tids[i], NULL);

    printf("threads=%d balance=%ld audit=%ld trylock_wins=%ld\n",
           n, balance, audit_log, trylock_wins);
    return 0;
}
