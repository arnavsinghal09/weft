/* deadlock: a classic lock-ordering (ABBA) deadlock, used to demonstrate the
 * scheduler's deadlock detection.
 *
 * Thread 1 takes lock A then lock B; thread 2 takes lock B then lock A. If the
 * scheduler interleaves them so each holds one lock and waits for the other,
 * every thread is blocked and none can proceed — a real deadlock. Weft detects
 * this (all threads blocked, none runnable) and aborts with a diagnostic
 * instead of hanging forever. Other interleavings let one thread finish first,
 * and the program exits cleanly. Which happens is a deterministic function of
 * the seed — so a deadlock, once found, is perfectly reproducible.
 */
#include <pthread.h>
#include <stdio.h>

static pthread_mutex_t a = PTHREAD_MUTEX_INITIALIZER;
static pthread_mutex_t b = PTHREAD_MUTEX_INITIALIZER;

static void *t1(void *_) {
    (void)_;
    pthread_mutex_lock(&a);
    pthread_mutex_lock(&b);
    pthread_mutex_unlock(&b);
    pthread_mutex_unlock(&a);
    return NULL;
}

static void *t2(void *_) {
    (void)_;
    pthread_mutex_lock(&b);
    pthread_mutex_lock(&a);
    pthread_mutex_unlock(&a);
    pthread_mutex_unlock(&b);
    return NULL;
}

int main(void) {
    pthread_t x, y;
    pthread_create(&x, NULL, t1, NULL);
    pthread_create(&y, NULL, t2, NULL);
    pthread_join(x, NULL);
    pthread_join(y, NULL);
    printf("completed without deadlock\n");
    return 0;
}
