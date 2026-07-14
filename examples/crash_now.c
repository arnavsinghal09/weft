/*
 * crash_now — die by an uncaught signal immediately. Under a windowed run a
 * signal death is a real crash mid-window (F1): the orchestrator must discard
 * the run (exit 3) rather than report a clean or merely-failed pass.
 */
#include <signal.h>

int main(void) {
    raise(SIGKILL);
    return 0;
}
