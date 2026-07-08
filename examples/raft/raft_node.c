/* raft_node: a minimal Raft LEADER-ELECTION implementation (no log
 * replication) run as a real OS process talking real UDP through Weft's
 * shim + broker — the second, smaller validation target after Chord.
 *
 * Property under test: ElectionSafety — "at most one leader can be elected
 * in a given term" (Ongaro, "Consensus: Bridging Theory and Practice",
 * Figure 3.2, and §3.6.1). The dissertation's Figure 3.2 requires that
 * currentTerm and votedFor be PERSISTENT state, "updated on stable storage
 * before responding to RPCs". The known edge case exercised here: if a
 * server crashes and restarts having LOST votedFor, it can grant a second
 * vote in the same term, letting two candidates each assemble a majority
 * for the SAME term — two leaders, ElectionSafety broken.
 *
 * Falsification switch (env RAFT_FIX):
 *   0 = buggy persistence: a crash-restart clears votedFor (volatile),
 *       violating Figure 3.2's persistence requirement;
 *   1 = correct: votedFor (and currentTerm) survive the restart.
 * Nothing else differs between the two levels.
 *
 * Crash-restart is simulated IN-PROCESS: at seed-jittered ticks a member
 * "crashes" (drops volatile state: role, vote tally, election timer — and,
 * at fix level 0, votedFor) and resumes as a follower next tick. This
 * models a fast reboot; the network keeps delivering (delayed) datagrams
 * sent to it, which is exactly the raciness the persistence rule guards.
 *
 * Every member reports "RPT id date alive term role votedFor" to the
 * observer (node N-1) each tick; raft-check scans ALL reports in the
 * recording (leadership is transient) for two distinct leaders in one term.
 *
 * Launch:
 *   RAFT_NNODES=N weft run --seed S --net "latency=uniform:LO-HI" --nodes N \
 *     --record LOG -- raft_node <ticks>
 * where node ids 0..N-2 are Raft servers and id N-1 is the observer.
 */
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#define MAXN 16
#define FOLLOWER 0
#define CANDIDATE 1
#define LEADER 2

static int TICKS, NNODES, OBS, me, members;
static int fix_level;
static uint64_t rng;
static int sockfd;

/* Figure 3.2 persistent state (persistence is what RAFT_FIX toggles). */
static int current_term = 0;
static int voted_for = -1;
/* Volatile state. */
static int role = FOLLOWER;
static unsigned char got_vote[MAXN];
static int timer = 0;      /* ticks since last heartbeat / vote grant */
static int timeout = 0;    /* current randomized election timeout */

static uint64_t splitmix64(uint64_t *s) {
    uint64_t z = (*s += 0x9E3779B97F4A7C15ULL);
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ULL;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EBULL;
    return z ^ (z >> 31);
}
static uint32_t jitter(uint32_t mod) { return (uint32_t)(splitmix64(&rng) % mod); }

static struct sockaddr_in node_addr(int node) {
    struct sockaddr_in a;
    char ip[32];
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons(9000);
    snprintf(ip, sizeof ip, "127.0.0.%d", node + 1);
    inet_pton(AF_INET, ip, &a.sin_addr);
    return a;
}
static void send_to(int node, const char *msg) {
    if (node < 0 || node >= NNODES) return;
    struct sockaddr_in a = node_addr(node);
    sendto(sockfd, msg, strlen(msg), 0, (struct sockaddr *)&a, sizeof a);
}
static void bcast(const char *msg) {
    for (int k = 0; k < members; k++)
        if (k != me) send_to(k, msg);
}

static void reset_timer(void) {
    /* 6-8 ticks, deliberately TIGHT: a real Raft would randomize over a
     * wide range precisely to avoid simultaneous candidacies, but the edge
     * case under test needs two candidates sharing a term, so the harness
     * keeps timeouts adversarially close (a stress schedule, not a
     * recommended deployment). Latency is on the same scale so replies
     * land inside the candidacy that asked for them. */
    timer = 0;
    timeout = 6 + (int)jitter(3);
}

static int votes(void) {
    int n = 0;
    for (int k = 0; k < members; k++) n += got_vote[k];
    return n;
}

static void step_down(int term) {
    if (term > current_term) {
        current_term = term;
        voted_for = -1;
    }
    role = FOLLOWER;
    memset(got_vote, 0, sizeof got_vote);
}

static void handle(const char *buf) {
    char kind[16];
    int a1 = 0, a2 = 0;
    if (sscanf(buf, "%15s %d %d", kind, &a1, &a2) < 1) return;

    if (!strcmp(kind, "RV")) { /* RequestVote(term=a1, candidate=a2) */
        if (a1 > current_term) step_down(a1);
        int grant = (a1 == current_term) && (voted_for == -1 || voted_for == a2);
        if (grant) {
            /* Figure 3.2: votedFor must hit stable storage BEFORE this
             * response. RAFT_FIX=0 breaks exactly that promise. */
            voted_for = a2;
            reset_timer();
        }
        char msg[64];
        snprintf(msg, sizeof msg, "RVR %d %d %d", current_term, me, grant);
        send_to(a2, msg);
    } else if (!strcmp(kind, "RVR")) { /* VoteReply(term=a1, voter=a2, granted) */
        int granted = 0;
        sscanf(buf, "%*s %*d %*d %d", &granted);
        if (a1 > current_term) { step_down(a1); return; }
        if (role == CANDIDATE && a1 == current_term && granted) {
            got_vote[a2] = 1;
            if (votes() * 2 > members) {
                role = LEADER;
                char msg[48];
                snprintf(msg, sizeof msg, "HB %d %d", current_term, me);
                bcast(msg);
            }
        }
    } else if (!strcmp(kind, "HB")) { /* heartbeat(term=a1, leader=a2) */
        if (a1 > current_term) step_down(a1);
        if (a1 == current_term) {
            if (role != LEADER) { role = FOLLOWER; reset_timer(); }
            /* two leaders in one term would each ignore the other's HB;
             * the checker catches that from the reports. */
        }
    }
}

static void drain_inbound(void) {
    char buf[256];
    for (;;) {
        ssize_t n = recvfrom(sockfd, buf, sizeof buf - 1, MSG_DONTWAIT, NULL, NULL);
        if (n <= 0) break;
        buf[n] = 0;
        handle(buf);
    }
}

static void report(int date, int alive) {
    char msg[128];
    snprintf(msg, sizeof msg, "RPT %d %d %d %d %d %d", me, date, alive,
             current_term, role, voted_for);
    send_to(OBS, msg);
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: raft_node ticks\n");
        return 2;
    }
    TICKS = atoi(argv[1]);

    const char *nid = getenv("WEFT_NODE_ID");
    me = nid ? atoi(nid) : 0;
    const char *e = getenv("RAFT_NNODES");
    NNODES = e ? atoi(e) : 6;
    OBS = NNODES - 1;
    members = NNODES - 1;

    const char *sd = getenv("WEFT_SEED");
    uint64_t seed = sd ? strtoull(sd, NULL, 0) : 0;
    rng = seed ^ (0xABCDEF01ULL * (uint64_t)(me + 1));

    const char *fx = getenv("RAFT_FIX");
    fix_level = fx ? atoi(fx) : 0;

    sockfd = socket(AF_INET, SOCK_DGRAM, 0);
    if (sockfd < 0) { perror("socket"); return 2; }
    struct sockaddr_in myaddr = node_addr(me);
    if (bind(sockfd, (struct sockaddr *)&myaddr, sizeof myaddr) != 0) {
        perror("bind");
        return 2;
    }

    if (me == OBS) {
        /* Observer: wait for every member's final-tick report, then drain. */
        int terminals = 0;
        char buf[256];
        while (terminals < members) {
            ssize_t n = recvfrom(sockfd, buf, sizeof buf - 1, 0, NULL, NULL);
            if (n <= 0) continue;
            buf[n] = 0;
            int id, date, alive, t, r, v;
            if (sscanf(buf, "RPT %d %d %d %d %d %d", &id, &date, &alive, &t, &r,
                       &v) == 6 &&
                date >= TICKS - 1)
                terminals++;
        }
        for (int i = 0; i < 2000; i++) drain_inbound();
        return 0;
    }

    /* Three seed-jittered crash-restart ticks per server, placed inside
     * the window where elections are still churning. */
    int restart1 = 3 + (int)jitter((uint32_t)(TICKS / 2));
    int restart2 = 3 + (int)jitter((uint32_t)(TICKS / 2));
    int restart3 = 3 + (int)jitter((uint32_t)(2 * TICKS / 3));
    reset_timer();

    for (int t = 0; t < TICKS; t++) {
        if (t == restart1 || t == restart2 || t == restart3) {
            /* Crash-restart: volatile state is gone. Figure 3.2 requires
             * currentTerm and votedFor to SURVIVE this; fix level 0 loses
             * votedFor — the edge case under test. */
            if (fix_level == 0) voted_for = -1;
            role = FOLLOWER;
            memset(got_vote, 0, sizeof got_vote);
            reset_timer();
            report(t, 0); /* observability: mark the restart tick */
            usleep(1000);
            continue;      /* the crashed tick does no protocol work */
        }

        drain_inbound();

        if (role == LEADER) {
            char msg[48];
            snprintf(msg, sizeof msg, "HB %d %d", current_term, me);
            bcast(msg);
        } else {
            timer++;
            if (timer >= timeout) {
                /* Become candidate (Figure 3.2 rules): increment term, vote
                 * for self, request votes from all. */
                current_term++;
                voted_for = me;
                role = CANDIDATE;
                memset(got_vote, 0, sizeof got_vote);
                got_vote[me] = 1;
                reset_timer();
            }
            if (role == CANDIDATE) {
                /* (Re)send RequestVote every tick while candidating — RPC
                 * retries per §3.3; retransmits also widen the window the
                 * lost-votedFor bug needs. */
                char msg[64];
                snprintf(msg, sizeof msg, "RV %d %d", current_term, me);
                bcast(msg);
            }
        }

        drain_inbound();
        report(t, 1);
        usleep(1000);
    }
    return 0;
}
