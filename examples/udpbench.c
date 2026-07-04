/* udpbench: measure datagram round-trip throughput, used to benchmark the
 * simulated network against real kernel loopback UDP.
 *
 * Two threads in one process: an echo thread on 127.0.0.1:9000 and a driver
 * on 127.0.0.1:9001 that performs K round trips. The program prints only the
 * round-trip count; *wall time is measured from outside* (scripts/
 * bench-net.sh), because under weft the in-process clock is virtual and
 * reads as almost zero elapsed.
 *
 * Native run: ./udpbench            (real UDP through the kernel)
 * Weft run:   weft run --seed 1 --net "" -- ./udpbench
 */
#include <arpa/inet.h>
#include <netinet/in.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#define K 5000

static struct sockaddr_in mkaddr(const char *ip, int port) {
    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((unsigned short)port);
    inet_pton(AF_INET, ip, &a.sin_addr);
    return a;
}

static void *echo_thread(void *arg) {
    (void)arg;
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in me = mkaddr("127.0.0.1", 9000);
    if (bind(fd, (struct sockaddr *)&me, sizeof me) != 0) { perror("echo bind"); exit(2); }
    char buf[64];
    for (;;) { /* echo until the driver says END (retries can add extras) */
        struct sockaddr_in from; socklen_t flen = sizeof from;
        ssize_t n = recvfrom(fd, buf, sizeof buf, 0, (struct sockaddr *)&from, &flen);
        if (n < 0) { perror("echo recvfrom"); exit(2); }
        if (n == 3 && memcmp(buf, "END", 3) == 0) break;
        sendto(fd, buf, (size_t)n, 0, (struct sockaddr *)&from, flen);
    }
    close(fd);
    return NULL;
}

int main(void) {
    pthread_t echo;
    pthread_create(&echo, NULL, echo_thread, NULL);

    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in me = mkaddr("127.0.0.1", 9001);
    if (bind(fd, (struct sockaddr *)&me, sizeof me) != 0) { perror("driver bind"); exit(2); }
    struct sockaddr_in srv = mkaddr("127.0.0.1", 9000);

    char msg[64] = "payload-0123456789abcdef";
    char buf[64];
    /* Retry the first exchange until the echo thread has bound (a datagram
     * to an unbound port is dropped, natively and simulated alike). */
    for (;;) {
        sendto(fd, msg, sizeof msg, 0, (struct sockaddr *)&srv, sizeof srv);
        ssize_t n = recvfrom(fd, buf, sizeof buf, MSG_DONTWAIT, NULL, NULL);
        if (n >= 0) break;
    }
    for (int i = 1; i < K; i++) {
        sendto(fd, msg, sizeof msg, 0, (struct sockaddr *)&srv, sizeof srv);
        if (recvfrom(fd, buf, sizeof buf, 0, NULL, NULL) < 0) { perror("recvfrom"); exit(2); }
    }
    sendto(fd, "END", 3, 0, (struct sockaddr *)&srv, sizeof srv);
    pthread_join(echo, NULL);
    close(fd);
    printf("completed %d round trips\n", K);
    return 0;
}
