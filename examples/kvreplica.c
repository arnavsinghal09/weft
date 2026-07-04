/* kvreplica: a replicated register with a real reordering bug.
 *
 * One process, two logical nodes as threads (so Phase 2's deterministic
 * scheduler controls their interleaving; see docs/network-model.md for why
 * cross-process interleaving isn't unified yet):
 *
 *   - The REPLICA thread owns a register. It applies every UPDATE it receives
 *     **in arrival order, with no version check** — that is the bug. Real
 *     systems hit exactly this when someone assumes "UDP from one sender
 *     mostly arrives in order" and skips the sequence check.
 *   - The WRITER thread sends versioned updates v=1..K, then a READ probe,
 *     and prints what the replica ended up with.
 *
 * On an in-order network the register finishes at K. Under seeded latency
 * variance the broker can deliver a *later* write before an *earlier* one;
 * the replica then blindly overwrites newer state with older state, and the
 * final read is stale. Whether that happens is a pure function of the seed:
 * a "trigger" seed reorders the tail of the write burst, an "avoid" seed
 * doesn't.
 *
 * Output: "final=<v> expected=<K> stale=<0|1>"; exit code 1 when stale.
 *
 * Run with: weft run --seed S --net "latency=uniform:1000-50000" -- kvreplica
 */
#include <arpa/inet.h>
#include <netinet/in.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#define K 8 /* number of versioned writes */

static struct sockaddr_in mkaddr(const char *ip, int port) {
    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((unsigned short)port);
    inet_pton(AF_INET, ip, &a.sin_addr);
    return a;
}

static void *replica(void *arg) {
    (void)arg;
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in me = mkaddr("127.0.0.1", 7000);
    if (bind(fd, (struct sockaddr *)&me, sizeof me) != 0) { perror("replica bind"); exit(2); }

    int value = 0;
    for (;;) {
        char buf[64];
        struct sockaddr_in from; socklen_t flen = sizeof from;
        ssize_t n = recvfrom(fd, buf, sizeof buf - 1, 0, (struct sockaddr *)&from, &flen);
        if (n < 0) { perror("replica recvfrom"); exit(2); }
        buf[n] = 0;
        if (strncmp(buf, "UPDATE:", 7) == 0) {
            /* BUG: blindly apply in arrival order — no version check. The
             * correct code would be: int v = atoi(buf+7); if (v > value)
             * value = v; */
            value = atoi(buf + 7);
        } else if (strncmp(buf, "READ", 4) == 0) {
            char reply[32];
            int m = snprintf(reply, sizeof reply, "VALUE:%d", value);
            sendto(fd, reply, (size_t)m, 0, (struct sockaddr *)&from, flen);
            break; /* one READ ends the run */
        }
    }
    close(fd);
    return NULL;
}

static void *writer(void *arg) {
    (void)arg;
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in me = mkaddr("127.0.0.1", 7001);
    if (bind(fd, (struct sockaddr *)&me, sizeof me) != 0) { perror("writer bind"); exit(2); }
    struct sockaddr_in rep = mkaddr("127.0.0.1", 7000);

    for (int v = 1; v <= K; v++) {
        char msg[32];
        int m = snprintf(msg, sizeof msg, "UPDATE:%d", v);
        sendto(fd, msg, (size_t)m, 0, (struct sockaddr *)&rep, sizeof rep);
    }
    sendto(fd, "READ", 4, 0, (struct sockaddr *)&rep, sizeof rep);

    char buf[64];
    ssize_t n = recvfrom(fd, buf, sizeof buf - 1, 0, NULL, NULL);
    if (n < 0) { perror("writer recvfrom"); exit(2); }
    buf[n] = 0;
    int final = (strncmp(buf, "VALUE:", 6) == 0) ? atoi(buf + 6) : -1;
    printf("final=%d expected=%d stale=%d\n", final, K, final != K);
    close(fd);
    return final == K ? NULL : (void *)1;
}

int main(void) {
    pthread_t r, w;
    void *stale = NULL;
    pthread_create(&r, NULL, replica, NULL);
    pthread_create(&w, NULL, writer, NULL);
    pthread_join(w, &stale);
    pthread_join(r, NULL);
    return stale != NULL;
}
