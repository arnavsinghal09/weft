/* chord_node: a faithful implementation of the ORIGINAL (2001 [SIGCOMM])
 * Chord ring-maintenance protocol — the version Pamela Zave proved incorrect
 * — run as a real OS process talking real UDP through Weft's shim + broker.
 *
 * See docs/case-study/chord-spec.md for the primary-source spec. Fidelity
 * points that are load-bearing for the bug being reachable:
 *
 *  - successor-list length r = 2 (succ, succ2);
 *  - stabilize adopts the successor's reported predecessor with NO liveness
 *    check — the original pseudocode has none, and adopting a dead node is
 *    exactly how Zave's Figure-6 gap forms;
 *  - reconcile / update / flush are SEPARATE periodic events with their own
 *    jittered schedules ([PODC]); folding them into stabilize (or running
 *    them every tick) closes the windows in which the anomalies live;
 *  - the failure model is fail-silent with detection latency: a failing node
 *    broadcasts DEAD (standing in for timeout-based perfect failure
 *    detection), and that datagram rides the same faulty network as
 *    everything else.
 *
 * Scenario shape (per the source material's small-ring anomaly conditions):
 * BASE permanent nodes start as the ideal ordered ring (the "stable base" of
 * r+1 = 3); the remaining members are appendages that join at seed-jittered
 * ticks and later fail at seed-jittered ticks. The fault seed thus sweeps the
 * join/fail timing AND (via the broker) message delay/reordering.
 *
 * Every node reports its state (RPT) to a dedicated observer after each
 * tick; those datagrams flow through the broker into the weft-log, which
 * chord-check reads to evaluate the final quiescent configuration.
 *
 * Launch:
 *   CHORD_NNODES=N weft run --seed S --net "latency=uniform:LO-HI" --nodes N \
 *     --record LOG -- chord_node <m> <ticks> <base>
 * where node ids 0..N-2 are Chord members (0..base-1 the stable base) and
 * id N-1 is the observer.
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
#define NONE (-1)

static int M, TICKS, BASE, NNODES, OBS;
static int me, myident;

static int succ, succ2, prdc;
static int joined;
static uint64_t rng;
/* Falsification switch (env CHORD_FIX), a liveness-discipline LEVEL:
 *   0 = original 2001 protocol: NO liveness checks on any adoption (the
 *       version Zave proves incorrect);
 *   1 = liveness check on stabilize's adoption of the successor's predecessor
 *       only (the single [PODC]-referenced correction the user named);
 *   2 = FULL liveness discipline: never adopt a known-dead node anywhere —
 *       stabilize, reconcile (succ2), update (succ promotion), and the
 *       GETSUCC responder all refuse dead nodes (the intent of the "best"
 *       version). Isolates unchecked-adoption flaws from any residual. */
static int fix_level;

static unsigned char dead_id[1 << 8];
static int ident_of_node[MAXN];
static int node_of_ident[1 << 8];
static int sockfd;

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

static void send_to_node(int node, const char *msg) {
    if (node < 0 || node >= NNODES) return;
    struct sockaddr_in a = node_addr(node);
    sendto(sockfd, msg, strlen(msg), 0, (struct sockaddr *)&a, sizeof a);
}
static void send_to_ident(int ident, const char *msg) {
    if (ident < 0) return;
    int node = node_of_ident[ident];
    if (node >= 0) send_to_node(node, msg);
}

/* x strictly within the clockwise arc (a, b) on the identifier circle. */
static int between(int a, int x, int b) {
    int n = 1 << M;
    a %= n; x %= n; b %= n;
    if (a < b) return a < x && x < b;
    return a < x || x < b;
}

static int is_live(int ident) { return ident >= 0 && !dead_id[ident]; }

/* best successor: first successor pointing to a live node (as this node
 * currently believes liveness to be). */
static int best_succ(void) {
    if (is_live(succ)) return succ;
    if (is_live(succ2)) return succ2;
    return NONE;
}

static void report(int date, int alive) {
    char msg[128];
    snprintf(msg, sizeof msg, "RPT %d %d %d %d %d %d", myident, date, alive, succ,
             succ2, prdc);
    send_to_node(OBS, msg);
}

static void init_base_pointers(void) {
    int i;
    for (i = 0; i < BASE; i++)
        if (ident_of_node[i] == myident) break;
    succ = ident_of_node[(i + 1) % BASE];
    succ2 = ident_of_node[(i + 2) % BASE];
    prdc = ident_of_node[(i + BASE - 1) % BASE];
    joined = 1;
}

static void handle(const char *buf) {
    char kind[16];
    int a1 = 0, a2 = 0, a3 = 0, a4 = 0;
    if (sscanf(buf, "%15s %d %d %d %d", kind, &a1, &a2, &a3, &a4) < 1) return;

    if (!strcmp(kind, "DEAD")) {
        if (a1 >= 0 && a1 < (1 << M)) dead_id[a1] = 1;
    } else if (!strcmp(kind, "GETPRED")) {
        /* Reply with my predecessor AS IS — even if it is dead and I have not
         * flushed yet. This staleness is part of the original protocol. */
        char msg[96];
        snprintf(msg, sizeof msg, "PRED %d %d %d %d", myident, prdc, succ, succ2);
        send_to_ident(a1, msg);
    } else if (!strcmp(kind, "PRED")) {
        /* Stabilize adoption: if my successor's predecessor lies between me and
         * my successor, adopt it. The ORIGINAL 2001 pseudocode has NO liveness
         * check here — adopting a dead node is exactly Zave's Figure-6 flaw.
         * The falsification test (CHORD_FIX=1) adds the [PODC]-style liveness
         * check on the adopted predecessor and changes NOTHING else, isolating
         * this one line as the cause. */
        int their_pred = a2;
        int bs = best_succ();
        int live_ok = (fix_level < 1) || is_live(their_pred);
        if (their_pred != NONE && live_ok && bs != NONE &&
            between(myident, their_pred, bs)) {
            succ = their_pred;
        }
        char msg[48];
        snprintf(msg, sizeof msg, "NOTIFY %d", myident);
        /* notify whoever I now consider my successor (succ may be dead; the
         * datagram to it is simply lost, as in reality). */
        send_to_ident(succ != NONE ? succ : bs, msg);
    } else if (!strcmp(kind, "GETSUCC")) {
        /* Report my successor. Level 2 reports my best (live) successor so a
         * reconciling antecedent never learns a dead second successor. */
        char msg[64];
        snprintf(msg, sizeof msg, "SUCC %d %d", myident,
                 fix_level >= 2 ? best_succ() : succ);
        send_to_ident(a1, msg);
    } else if (!strcmp(kind, "SUCC")) {
        /* reconcile response: adopt successor's successor as my second.
         * Original has no liveness check; level 2 refuses a dead second. */
        if (a2 != NONE && (fix_level < 2 || is_live(a2))) succ2 = a2;
    } else if (!strcmp(kind, "NOTIFY")) {
        int cand = a1;
        if (prdc == NONE || !is_live(prdc) || between(prdc, cand, myident)) prdc = cand;
    } else if (!strcmp(kind, "FINDSUCC")) {
        int joiner = a1;
        int bs = best_succ();
        if (bs != NONE && between(myident, joiner, bs)) {
            char msg[64];
            snprintf(msg, sizeof msg, "FOUNDSUCC %d %d", joiner, bs);
            send_to_ident(joiner, msg);
        } else if (bs != NONE) {
            char msg[48];
            snprintf(msg, sizeof msg, "FINDSUCC %d", joiner);
            send_to_ident(bs, msg);
        }
    } else if (!strcmp(kind, "FOUNDSUCC")) {
        if (a1 == myident && !joined) {
            succ = a2;
            prdc = NONE;
            joined = 1;
        }
    }
}

static int drain_inbound(void) {
    char buf[256];
    int got = 0;
    for (;;) {
        ssize_t n = recvfrom(sockfd, buf, sizeof buf - 1, MSG_DONTWAIT, NULL, NULL);
        if (n <= 0) break;
        buf[n] = 0;
        handle(buf);
        got++;
    }
    return got;
}

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: chord_node m ticks base\n");
        return 2;
    }
    M = atoi(argv[1]);
    TICKS = atoi(argv[2]);
    BASE = atoi(argv[3]);

    const char *nid = getenv("WEFT_NODE_ID");
    me = nid ? atoi(nid) : 0;
    const char *e = getenv("CHORD_NNODES");
    NNODES = e ? atoi(e) : 4;
    OBS = NNODES - 1;

    /* Deterministic identifier layout shared by every process: base nodes
     * evenly spaced; appendage k drops just after base (k-BASE)%BASE. */
    memset(node_of_ident, 0xff, sizeof node_of_ident);
    int members = NNODES - 1;
    int space = 1 << M;
    for (int k = 0; k < members; k++) {
        int ident;
        if (k < BASE) {
            ident = (k * space) / BASE + 1;
        } else {
            int b = (k - BASE) % BASE;
            ident = ((b * space) / BASE + 1 + 3 + 2 * ((k - BASE) / BASE)) % space;
        }
        ident_of_node[k] = ident;
        node_of_ident[ident] = k;
    }
    myident = (me < members) ? ident_of_node[me] : -1;

    const char *sd = getenv("WEFT_SEED");
    uint64_t seed = sd ? strtoull(sd, NULL, 0) : 0;
    rng = seed ^ (0x1234567ULL * (uint64_t)(me + 1));

    const char *fx = getenv("CHORD_FIX");
    fix_level = fx ? atoi(fx) : 0;

    sockfd = socket(AF_INET, SOCK_DGRAM, 0);
    if (sockfd < 0) { perror("socket"); return 2; }
    struct sockaddr_in myaddr = node_addr(me);
    if (bind(sockfd, (struct sockaddr *)&myaddr, sizeof myaddr) != 0) {
        perror("bind");
        return 2;
    }

    succ = succ2 = prdc = NONE;

    if (me == OBS) {
        /* Observer: blocking receives (parked in the broker, cheap) until
         * every member has sent its terminal report — a dead report
         * (alive=0) or the final quiescent report (date == TICKS+9). The
         * latency-only campaign net drops nothing, so exactly one terminal
         * arrives per member. Then drain stragglers and exit. */
        int members_total = NNODES - 1;
        int terminals = 0;
        char buf[256];
        while (terminals < members_total) {
            ssize_t n = recvfrom(sockfd, buf, sizeof buf - 1, 0, NULL, NULL);
            if (n <= 0) continue;
            buf[n] = 0;
            int ident, date, alive, s1, s2, p;
            if (sscanf(buf, "RPT %d %d %d %d %d %d", &ident, &date, &alive, &s1,
                       &s2, &p) == 6) {
                if (!alive || date >= TICKS + 9) terminals++;
            }
        }
        for (int i = 0; i < 2000; i++) drain_inbound();
        return 0;
    }

    int is_base = (me < BASE);
    if (is_base) init_base_pointers();

    /* Appendage schedule, jittered by the seed: join early, fail later —
     * with in-flight stabilize/notify/reconcile traffic in between. Only
     * appendages fail (the stable base is permanent), which also keeps the
     * papers' failure assumption satisfiable. */
    int join_tick = is_base ? 0 : 2 + (int)jitter(6);
    int fail_tick = is_base ? -1 : (TICKS / 3) + (int)jitter(TICKS / 3);
    int bootstrap = (int)jitter((uint32_t)BASE); /* contact a random base node */

    for (int t = 0; t < TICKS; t++) {
        drain_inbound();

        if (!is_base && t == fail_tick) {
            char msg[32];
            snprintf(msg, sizeof msg, "DEAD %d", myident);
            for (int k = 0; k < NNODES; k++)
                if (k != me) send_to_node(k, msg);
            report(t, 0);
            return 0; /* fail-silent from here on */
        }

        if (!is_base && !joined && t >= join_tick) {
            char msg[48];
            snprintf(msg, sizeof msg, "FINDSUCC %d", myident);
            send_to_node(bootstrap, msg);
        }

        if (joined) {
            /* update (periodic, jittered): promote past a dead successor.
             * Original promotes succ2 blindly; level 2 only promotes a live
             * second successor (never installs a known-dead first successor). */
            if (jitter(3) == 0 && !is_live(succ) && succ2 != NONE &&
                (fix_level < 2 || is_live(succ2))) {
                succ = succ2;
            }
            /* flush (periodic, jittered): discard a dead predecessor. */
            if (jitter(3) == 0 && prdc != NONE && !is_live(prdc)) {
                prdc = NONE;
            }
            /* stabilize (periodic, jittered). */
            int bs = best_succ();
            if (bs != NONE && jitter(3) != 0) {
                char msg[32];
                snprintf(msg, sizeof msg, "GETPRED %d", myident);
                send_to_ident(bs, msg);
            }
            /* reconcile (periodic, rarer — this lag is the bug's window). */
            if (jitter(4) == 0) {
                int target = is_live(succ) ? succ : bs;
                if (target != NONE) {
                    char msg[32];
                    snprintf(msg, sizeof msg, "GETSUCC %d", myident);
                    send_to_ident(target, msg);
                }
            }
        }

        report(t, 1);
        drain_inbound();
        /* Virtual pacing: usleep is virtualized (advances this process's
         * virtual clock, returns immediately). Cross-process interleaving
         * comes from the broker's fault model and OS scheduling — which is
         * exactly the nondeterministic input the recording captures. */
        usleep(1000);
    }

    /* Quiescent tail: fault injection is over (all failures happened before
     * TICKS); keep running the maintenance protocol to give it every chance
     * to repair, then report the final state. */
    for (int q = 0; q < 8; q++) {
        drain_inbound();
        if (jitter(2) == 0 && !is_live(succ) && succ2 != NONE &&
            (fix_level < 2 || is_live(succ2)))
            succ = succ2;
        if (prdc != NONE && !is_live(prdc)) prdc = NONE;
        int bs = best_succ();
        if (bs != NONE) {
            char msg[32];
            snprintf(msg, sizeof msg, "GETPRED %d", myident);
            send_to_ident(bs, msg);
            snprintf(msg, sizeof msg, "GETSUCC %d", myident);
            send_to_ident(is_live(succ) ? succ : bs, msg);
        }
        report(TICKS + q, 1);
        usleep(1000);
    }
    report(TICKS + 9, 1);
    return 0;
}
