/* netsched: proof that waiting on the network consumes no scheduler entropy.
 *
 * Node 0 (receiver) is multithreaded: thread R blocks in recvfrom for NMSG
 * datagrams from node 1 while workers W1/W2 interleave through a mutex,
 * appending to a shared order log. Node 1 (sender) sends the NMSG datagrams,
 * optionally burning REAL time between sends (NETSCHED_SPIN busy-loop
 * iterations — real delay only: no clock reads, no syscalls).
 *
 * Under Weft a managed blocking recv is deferred to a scheduler idle point,
 * so the receiver consumes datagrams only after the workers can no longer
 * run. The workers' 'a'/'b' interleaving is therefore a pure function of the
 * seed, independent of when node 1's datagrams physically arrive. With
 * entropy-free network waiting the whole `order=` line must be identical for
 * a given seed regardless of NETSCHED_SPIN; if waiting drew RNG per real poll
 * (the old behavior) the sender's real-time jitter would shift it.
 *
 * A filesystem handshake removes the unrelated UDP startup race: node 1 waits
 * for node 0 to create a ready file (i.e. to have bound its port) before
 * sending, so no datagram is dropped as "sent to an unbound address". The
 * ready path is NETSCHED_READY (default /tmp/netsched.ready).
 *
 * Run: weft run --seed S --net "" --nodes 2 -- netsched
 */
#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

#define NMSG 4
#define WORK 6

static struct sockaddr_in mkaddr(const char *ip, int port) {
    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((unsigned short)port);
    inet_pton(AF_INET, ip, &a.sin_addr);
    return a;
}

static const char *ready_path(void) {
    const char *p = getenv("NETSCHED_READY");
    return p ? p : "/tmp/netsched.ready";
}

static pthread_mutex_t log_mu = PTHREAD_MUTEX_INITIALIZER;
static char order_log[256];
static int log_len;

static void log_event(char c) {
    pthread_mutex_lock(&log_mu);
    if (log_len < (int)sizeof order_log - 1) order_log[log_len++] = c;
    pthread_mutex_unlock(&log_mu);
}

static int rx_fd;

static void *receiver(void *arg) {
    (void)arg;
    char buf[64];
    for (int i = 0; i < NMSG; i++) {
        ssize_t n = recvfrom(rx_fd, buf, sizeof buf - 1, 0, NULL, NULL);
        if (n < 0) { perror("recvfrom"); exit(2); }
        log_event('N');
    }
    return NULL;
}

static void *worker(void *arg) {
    char tag = *(const char *)arg;
    for (int i = 0; i < WORK; i++) {
        log_event(tag);
        usleep(500); /* virtual: a yield point, not real time */
    }
    return NULL;
}

int main(void) {
    const char *nid = getenv("WEFT_NODE_ID");
    int node = nid ? atoi(nid) : 0;

    if (node == 0) {
        rx_fd = socket(AF_INET, SOCK_DGRAM, 0);
        struct sockaddr_in me = mkaddr("127.0.0.1", 5000);
        if (bind(rx_fd, (struct sockaddr *)&me, sizeof me) != 0) {
            perror("bind");
            return 2;
        }
        /* Signal readiness *after* binding: creation only, no write(2)
         * (which the shim interposes). Node 1 waits for this. */
        int rf = open(ready_path(), O_CREAT | O_WRONLY | O_TRUNC, 0644);
        if (rf >= 0) close(rf);

        pthread_t r, w1, w2;
        static const char a = 'a', b = 'b';
        pthread_create(&r, NULL, receiver, NULL);
        pthread_create(&w1, NULL, worker, (void *)&a);
        pthread_create(&w2, NULL, worker, (void *)&b);
        pthread_join(w1, NULL);
        pthread_join(w2, NULL);
        pthread_join(r, NULL);
        order_log[log_len] = 0;
        printf("order=%s\n", order_log);
        return 0;
    }

    /* node 1: sender. Wait for node 0 to bind (ready file), then send with
     * optional REAL jitter between sends. */
    while (access(ready_path(), F_OK) != 0) {
        for (volatile long j = 0; j < 10000; j++) { /* real spin, no syscall */ }
    }
    long spin = 0;
    const char *sp = getenv("NETSCHED_SPIN");
    if (sp) spin = atol(sp);
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in me = mkaddr("127.0.0.2", 5001);
    if (bind(fd, (struct sockaddr *)&me, sizeof me) != 0) { perror("bind"); return 2; }
    struct sockaddr_in dst = mkaddr("127.0.0.1", 5000);
    for (int i = 0; i < NMSG; i++) {
        for (volatile long j = 0; j < spin; j++) { /* real time only */ }
        char msg[16];
        int m = snprintf(msg, sizeof msg, "m%d", i);
        sendto(fd, msg, (size_t)m, 0, (struct sockaddr *)&dst, sizeof dst);
    }
    return 0;
}
