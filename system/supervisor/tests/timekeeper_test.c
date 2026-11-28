// Host test of the timekeeper's decisions (platform/sel4/include/timekeeper.h):
// make test-unit. No kernel: pages are plain memory and time is a number.
#include "timekeeper.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures;
#define EXPECT(what) \
  do { \
    if (!(what)) { \
      printf("FAILED %s:%d: %s\n", __FILE__, __LINE__, #what); \
      failures++; \
    } \
  } while (0)

enum { POWER = 8, TOUCH = 4, AUDIO = 22, APP = 11 };
_Static_assert(TK_CLIENTS <= 27 && (TK_IRQ_BADGE >> 28) == 0 && (TK_BADGE >> 28) == 0, "badges are 28 bits");
static struct tk_page pages[TK_CLIENTS];
static struct timekeeper k;
static unsigned kicks;

static void fresh(void) {
  memset(pages, 0, sizeof pages);
  memset(&k, 0, sizeof k);
  kicks = 0;
  for (unsigned i = 0; i < TK_CLIENTS; i++) k.c[i].page = &pages[i];
  k.power = POWER;
  k.period = 500;
}
static unsigned long long next;
// What a process does: write its request, then kick.
static void arm(unsigned i, unsigned ticks) {
  tk_page_arm(&pages[i], ticks);
  if (ticks) kicks |= 1u << i;
}
static unsigned scan(unsigned long long now) {
  unsigned kicked = kicks;
  kicks = 0;
  return tk_scan(&k, now, kicked, &next);
}
static unsigned bit(unsigned i) { return 1u << i; }

// A request is counted from the look that takes it up (the kick's).
static void a_deadline_fires_once_and_never_early(void) {
  fresh();
  arm(AUDIO, 90);
  EXPECT(scan(10) == 0 && next == 100);
  EXPECT(scan(99) == 0 && next == 100);
  EXPECT(scan(100) == bit(AUDIO));
  // Signalled once, and nothing is left to wait for.
  EXPECT(scan(101) == 0 && next == 0);
  EXPECT(scan(5000) == 0 && next == 0);
  // Asked again (a sleeper woken early asks for what is left): signalled again.
  arm(AUDIO, 1);
  EXPECT(scan(5001) == 0 && next == 5002);
  EXPECT(scan(5002) == bit(AUDIO));
  // The page cannot move a deadline once it is taken up, except by kicking again.
  arm(AUDIO, 50);
  scan(6000);
  pages[AUDIO].ticks = 1;
  EXPECT(scan(6010) == 0 && next == 6050);
  // Writing zero cancels it without a kick (the Open Runtime's timer stop).
  pages[AUDIO].ticks = 0;
  EXPECT(scan(6060) == 0 && next == 0);
  // A kick with nothing asked for is nothing.
  kicks = bit(AUDIO);
  EXPECT(scan(6070) == 0 && next == 0);
  EXPECT(tk_ticks_until(10, 100) == 90 && tk_ticks_until(100, 100) == 1 && tk_ticks_until(0, 1ull << 40) == 0xffffffffu);
}

static void the_earliest_deadline_is_next(void) {
  fresh();
  arm(AUDIO, 300);
  arm(TOUCH, 200);
  arm(APP, 250);
  EXPECT(scan(0) == 0 && next == 200);
  EXPECT(scan(260) == (bit(TOUCH) | bit(APP)) && next == 300);
  // A cancelled request is neither signalled nor waited for.
  arm(AUDIO, 0);
  EXPECT(scan(400) == 0 && next == 0);
}

static void a_client_without_a_page_is_ignored(void) {
  fresh();
  k.c[APP].page = 0;
  arm(APP, 1);
  EXPECT(scan(50) == 0 && next == 0);
  // Nor is a kick bit beyond the table.
  kicks = ~0u;
  EXPECT(scan(55) == 0);
  EXPECT(scan(60) == 0);
}

static void only_power_can_freeze(void) {
  fresh();
  tk_page_command(&pages[AUDIO], TK_OP_SUSPEND, ~0u);
  scan(10);
  EXPECT(!k.frozen && pages[AUDIO].state == 0);
  tk_page_command(&pages[POWER], TK_OP_SUSPEND, bit(TOUCH));
  scan(20);
  EXPECT(k.frozen && k.freezes == 1 && pages[AUDIO].state == TK_FROZEN && pages[POWER].freezes == 1);
  // The same command is carried out once.
  k.frozen = 0;
  scan(30);
  EXPECT(!k.frozen);
}

static void frozen_holds_deadlines_and_releases_them_together(void) {
  fresh();
  tk_page_command(&pages[POWER], TK_OP_SUSPEND, 0);
  scan(1000);
  EXPECT(k.frozen && k.thaw == 1500);
  arm(AUDIO, 1);
  arm(APP, 20);
  arm(POWER, 50);
  // Held: nothing fires for the others, and the next look is Power's own deadline.
  EXPECT(scan(1000) == 0 && next == 1050);
  EXPECT(scan(1030) == 0 && next == 1050);
  EXPECT(k.held == 2);
  // Power is never held.
  EXPECT(scan(1050) == bit(POWER) && next == 1500);
  EXPECT(scan(1499) == 0 && next == 1500);
  // The release: everything due goes at once, and the next one is a period on.
  EXPECT(scan(1500) == (bit(AUDIO) | bit(APP)) && k.thaw == 2000);
  // Each sleeper loops once and sleeps again: held until the next release.
  arm(AUDIO, 1);
  EXPECT(scan(1501) == 0 && next == 2000);
  EXPECT(scan(1999) == 0);
  EXPECT(scan(2000) == bit(AUDIO));
  // Counted once per held deadline, however often it is looked at.
  EXPECT(k.held == 3);
  // A deadline later than the release is not brought forward.
  arm(APP, 7000);
  EXPECT(scan(2000) == 0 && next == 9000);
}

static void power_resumes_and_everything_due_fires(void) {
  fresh();
  tk_page_command(&pages[POWER], TK_OP_SUSPEND, 0);
  scan(0);
  arm(AUDIO, 5);
  arm(APP, 400);
  EXPECT(scan(0) == 0);
  EXPECT(scan(10) == 0);
  tk_page_command(&pages[POWER], TK_OP_RESUME, 4);
  EXPECT(scan(20) == bit(AUDIO) && next == 400);
  EXPECT(!k.frozen && k.reason == 4 && pages[APP].state == 0 && pages[POWER].reason == 4);
  // A resume while awake changes nothing.
  tk_page_command(&pages[POWER], TK_OP_RESUME, 9);
  scan(30);
  EXPECT(k.reason == 4);
}

static void a_wake_source_ends_the_freeze_and_others_cannot(void) {
  fresh();
  tk_page_command(&pages[POWER], TK_OP_SUSPEND, bit(TOUCH));
  scan(0);
  arm(AUDIO, 5);
  // Not in the mask: ignored, and used up.
  pages[APP].wake = 7;
  EXPECT(scan(10) == 0 && k.frozen && pages[APP].wake == 0);
  pages[TOUCH].wake = 2;
  EXPECT(scan(20) == bit(AUDIO));
  EXPECT(!k.frozen && k.reason == 2 && pages[TOUCH].wake == 0 && pages[POWER].state == 0);
  // A wake written while awake does not end the next freeze.
  pages[TOUCH].wake = 2;
  scan(30);
  tk_page_command(&pages[POWER], TK_OP_SUSPEND, bit(TOUCH));
  scan(40);
  EXPECT(k.frozen);
  // A wake in the same look as the suspend counts: the user is already there.
  fresh();
  tk_page_command(&pages[POWER], TK_OP_SUSPEND, bit(TOUCH));
  pages[TOUCH].wake = 2;
  scan(0);
  EXPECT(!k.frozen && k.reason == 2 && k.freezes == 1);
}

static void a_hostile_page_cannot_hold_the_others(void) {
  fresh();
  // Asking over and over for the shortest sleep wakes its owner only.
  arm(AUDIO, 1000);
  scan(50);
  for (unsigned i = 1; i < 50; i++) {
    arm(APP, 1);
    EXPECT(scan(50 + 2 * i) == 0 && next == 50 + 2 * i + 1);
    EXPECT(scan(50 + 2 * i + 1) == bit(APP) && next == 1050);
  }
  EXPECT(scan(1050) == bit(AUDIO));
  // The longest sleep does not hide a nearer one.
  fresh();
  arm(APP, ~0u);
  arm(AUDIO, 40);
  EXPECT(scan(60) == 0 && next == 100);
  EXPECT(scan(100) == bit(AUDIO) && next == 60 + 0xffffffffull);
  // Nor can it freeze, or wake without being a wake source.
  pages[APP].cmd = 0x100 | TK_OP_SUSPEND;
  pages[APP].arg = ~0u;
  scan(110);
  EXPECT(!k.frozen);
  // A period of zero (never set) still makes progress while frozen.
  k.period = 0;
  tk_page_command(&pages[POWER], TK_OP_SUSPEND, 0);
  scan(200);
  arm(AUDIO, 1);
  scan(200);
  EXPECT(scan(201) == bit(AUDIO));
}

static void short_waits_are_not_held(void) {
  fresh();
  k.hold_min = 3;
  tk_page_command(&pages[POWER], TK_OP_SUSPEND, 0);
  scan(1000);
  // A device wait in the middle of work (a tick or two) is served on time;
  // an idle loop's longer sleep waits for the release.
  arm(AUDIO, 2);
  arm(APP, 3);
  EXPECT(scan(1000) == 0 && next == 1002);
  EXPECT(scan(1002) == bit(AUDIO) && next == 1500);
  EXPECT(k.held == 1);
  // The same process asking for longer next time is held like the rest.
  arm(AUDIO, 20);
  EXPECT(scan(1010) == 0 && next == 1500);
  EXPECT(scan(1500) == (bit(AUDIO) | bit(APP)));
  // And one that goes back to short waits is urgent again.
  arm(APP, 1);
  EXPECT(scan(1500) == 0 && next == 1501);
  EXPECT(scan(1501) == bit(APP));
}

static void timer_values(void) {
  EXPECT(tk_timer_value(10, 0) == 0);
  EXPECT(tk_timer_value(10, 10) == 1);
  EXPECT(tk_timer_value(10, 5) == 1);
  EXPECT(tk_timer_value(10, 510) == 500);
  EXPECT(tk_timer_value(0, ~0ull) == 0x7fffffffu);
}

// Many sleepers with random periods, some freezes: every request is
// signalled, never before its time, and while awake by the first look at or
// after it.
static void random_sleepers(void) {
  fresh();
  unsigned long long due[TK_CLIENTS] = {0}, now = 1;
  unsigned seed = 12345, woken = 0;
  for (unsigned step = 0; step < 20000; step++) {
    seed = seed * 1103515245u + 12345u;
    unsigned i = (seed >> 16) % TK_CLIENTS;
    if (!due[i] && i != POWER) {
      unsigned ticks = 1 + (seed >> 8) % 40;
      due[i] = now + ticks;
      arm(i, ticks);
    }
    if (step % 5000 == 2500) tk_page_command(&pages[POWER], TK_OP_SUSPEND, 0);
    if (step % 5000 == 4000) tk_page_command(&pages[POWER], TK_OP_RESUME, 1);
    unsigned fire = scan(now);
    for (unsigned c = 0; c < TK_CLIENTS; c++) {
      if (fire >> c & 1) {
        EXPECT(due[c] && due[c] <= now);
        due[c] = 0;
        woken++;
      } else if (due[c] && due[c] <= now) {
        // Late only while frozen, and then no later than the next release.
        EXPECT(k.frozen && k.thaw > now && k.thaw <= now + k.period);
      }
    }
    if (next) EXPECT(next > now);
    now += next && next - now < 7 ? next - now : 1 + (seed >> 4) % 7;
  }
  EXPECT(woken > 2000);
}

// The timekeeper's loop as timekeeper_thread.h runs it: scan, program the
// timer, publish when it should fire. `irq` says whether the timer works.
static struct tk_watch w;
static unsigned long long programmed; // when the hardware will interrupt; 0: never
static unsigned tk_pass(unsigned long long now) {
  unsigned fire = scan(now);
  unsigned value = tk_timer_value(now, next);
  programmed = value ? now + value : 0;
  tk_watch_set(&w, now, value);
  return fire;
}

// An open app clears CNTP_CTL after every reprogram (it may: the kernel
// exports the timer to user mode). Without the guard no interrupt comes and
// nothing ever scans again; with it, every sleeper is still woken, at most
// the slack plus a guard turn late, and the root's health deadline too.
static void an_app_stopping_the_timer_delays_but_never_hangs(void) {
  fresh();
  memset(&w, 0, sizeof w);
  enum { SLACK = 20, TURN = 10 }; // ticks: the guard's slack, a time slice
  arm(0, 100);        // the supervisor's health check
  arm(TOUCH, 30);
  arm(AUDIO, 250);
  EXPECT(tk_pass(1) == 0);
  EXPECT(programmed == 31);
  unsigned long long due[TK_CLIENTS] = {0};
  due[0] = 101, due[TOUCH] = 31, due[AUDIO] = 251;
  unsigned woken = 0, kicks_sent = 0;
  for (unsigned long long now = 2; now < 2000 && woken != 3; now += TURN) {
    // The app runs after every pass and stops the timer: no interrupt, ever.
    programmed = 0;
    if (!tk_guard_turn(&w, now, SLACK)) continue;
    kicks_sent++;
    unsigned fire = tk_pass(now); // a kick with no client bits
    for (unsigned c = 0; c < TK_CLIENTS; c++)
      if (fire >> c & 1) {
        EXPECT(due[c] && now >= due[c] && now <= due[c] + SLACK + TURN);
        due[c] = 0;
        woken++;
      }
  }
  EXPECT(woken == 3);
  EXPECT(kicks_sent == w.late && kicks_sent >= 3);
  // A kick re-reads no request: the root's deadline was not pushed back.
  EXPECT(k.c[0].deadline == 0);
  // A working timer is never "late": the guard leaves it alone.
  fresh();
  memset(&w, 0, sizeof w);
  arm(TOUCH, 30);
  tk_pass(1);
  EXPECT(!tk_guard_turn(&w, 31, SLACK) && !tk_guard_turn(&w, 31 + SLACK, SLACK));
  EXPECT(tk_pass(31) == 1u << TOUCH);
  EXPECT(!tk_guard_turn(&w, 5000, SLACK) && w.late == 0); // nothing waiting: timer stopped
}

// The guard never acts on a half-written deadline, and waits for an
// interrupt only when its yield ran nobody else and WFI works.
static void the_guard_reads_whole_deadlines_and_idles_only_when_alone(void) {
  struct tk_watch v;
  memset(&v, 0, sizeof v);
  unsigned long long due = 7;
  tk_watch_set(&v, 0x1fffffff0ull, 0x20);
  EXPECT(tk_watch_get(&v, &due) && due == 0x200000010ull);
  v.seq++; // the timekeeper is mid-write
  EXPECT(!tk_watch_get(&v, &due) && !tk_guard_turn(&v, ~0ull, 0));
  v.seq++;
  EXPECT(tk_guard_turn(&v, 0x200000010ull + 6, 5) && v.late == 1);
  EXPECT(tk_guard_may_wait(&v, 100, 102, 5));
  EXPECT(!tk_guard_may_wait(&v, 100, 105, 5)); // someone else ran
  v.wfi = 3;
  EXPECT(!tk_guard_may_wait(&v, 100, 101, 5)); // WFI traps here: yield only
  // Stuck in the first WFI for three looks: restart it yielding.
  unsigned looks = 0;
  v.wfi = 1;
  EXPECT(!tk_guard_stuck(&v, &looks, 3) && !tk_guard_stuck(&v, &looks, 3) && tk_guard_stuck(&v, &looks, 3));
  looks = 0;
  EXPECT(!tk_guard_stuck(&v, &looks, 3) && !tk_guard_stuck(&v, &looks, 3));
  v.wfi = 2; // it came back: the count starts again
  EXPECT(!tk_guard_stuck(&v, &looks, 3) && looks == 0);
  v.wfi = 1;
  EXPECT(!tk_guard_stuck(&v, &looks, 3));
}

// timekeeper_attach_all: the Open Runtime is attached on its own and the
// Sentinel (26) never sleeps, so neither gets a page here.
static void the_sentinel_and_the_open_runtime_are_not_attached(void) {
  enum { OPEN_RT = 24, SENTINEL = 26, TOTAL = 27 };
  unsigned skip = 1u << OPEN_RT | 1u << SENTINEL, served = 0;
  for (unsigned id = 0; id < 40; id++) served |= (unsigned)tk_serves(id, TOTAL, skip) << id;
  EXPECT(served == (((1u << TOTAL) - 2) & ~skip));
  EXPECT(!(served & 1)); // 0 is the supervisor itself
  EXPECT(!tk_serves(27, 40, 0)); // past the clients' pages
}

int main(void) {
  an_app_stopping_the_timer_delays_but_never_hangs();
  the_guard_reads_whole_deadlines_and_idles_only_when_alone();
  the_sentinel_and_the_open_runtime_are_not_attached();
  a_deadline_fires_once_and_never_early();
  the_earliest_deadline_is_next();
  a_client_without_a_page_is_ignored();
  only_power_can_freeze();
  frozen_holds_deadlines_and_releases_them_together();
  power_resumes_and_everything_due_fires();
  a_wake_source_ends_the_freeze_and_others_cannot();
  a_hostile_page_cannot_hold_the_others();
  short_waits_are_not_held();
  timer_values();
  random_sleepers();
  if (failures) return 1;
  puts("timekeeper: 14 tests passed");
  return 0;
}
