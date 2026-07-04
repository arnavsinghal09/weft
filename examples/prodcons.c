/* prodcons: a correctly-synchronized bounded-buffer producer/consumer, used
 * to exercise condition variables under the deterministic scheduler.
 *
 * P producers each push ITEMS integers; C consumers pop until every item is
 * consumed. Synchronization is a mutex plus two condition variables
 * (not_full, not_empty) — the textbook correct pattern, with predicate loops
 * (so it tolerates the scheduler's signal semantics). The consumed checksum
 * is order-independent (a sum), so a correct run is fully deterministic under
 * Weft and identical across seeds in value, while the interleaving itself
 * varies by seed.
 *
 * Usage: prodcons [producers] [consumers] [items_per_producer]
 */
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>

#define CAP 4

static int buf[CAP];
static int count = 0, head = 0, tail = 0;
static long produced_total = 0, consumed_total = 0;
static int outstanding = 0; /* items still to be consumed */

static pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t not_full = PTHREAD_COND_INITIALIZER;
static pthread_cond_t not_empty = PTHREAD_COND_INITIALIZER;

static int items = 100;

static void *producer(void *arg) {
    long base = (long)arg * 1000;
    for (int i = 0; i < items; i++) {
        pthread_mutex_lock(&m);
        while (count == CAP)
            pthread_cond_wait(&not_full, &m);
        int v = (int)(base + i);
        buf[tail] = v;
        tail = (tail + 1) % CAP;
        count++;
        produced_total += v;
        pthread_cond_signal(&not_empty);
        pthread_mutex_unlock(&m);
    }
    return NULL;
}

static void *consumer(void *arg) {
    (void)arg;
    for (;;) {
        pthread_mutex_lock(&m);
        while (count == 0 && outstanding > 0)
            pthread_cond_wait(&not_empty, &m);
        if (count == 0 && outstanding == 0) {
            /* Wake any siblings also waiting to exit, then leave. */
            pthread_cond_broadcast(&not_empty);
            pthread_mutex_unlock(&m);
            return NULL;
        }
        int v = buf[head];
        head = (head + 1) % CAP;
        count--;
        outstanding--;
        consumed_total += v;
        pthread_cond_signal(&not_full);
        pthread_mutex_unlock(&m);
    }
}

int main(int argc, char **argv) {
    int np = (argc > 1) ? atoi(argv[1]) : 2;
    int nc = (argc > 2) ? atoi(argv[2]) : 2;
    if (argc > 3) items = atoi(argv[3]);
    if (np < 1) np = 1;
    if (nc < 1) nc = 1;
    if (np > 16) np = 16;
    if (nc > 16) nc = 16;
    outstanding = np * items;

    pthread_t prod[16], cons[16];
    for (int i = 0; i < np; i++)
        pthread_create(&prod[i], NULL, producer, (void *)(long)i);
    for (int i = 0; i < nc; i++)
        pthread_create(&cons[i], NULL, consumer, NULL);
    for (int i = 0; i < np; i++) pthread_join(prod[i], NULL);
    for (int i = 0; i < nc; i++) pthread_join(cons[i], NULL);

    printf("producers=%d consumers=%d items=%d produced=%ld consumed=%ld match=%d\n",
           np, nc, items, produced_total, consumed_total,
           produced_total == consumed_total);
    return produced_total == consumed_total ? 0 : 1;
}
