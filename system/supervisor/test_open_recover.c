// Host test for open_recover.h (`make test-unit`): how often a fault in the
// Open Runtime is answered with a warm restart before the supervisor falls
// back to suspending it.
#include "open_recover.h"
#include <stdio.h>

static int failed;
#define CHECK(what) \
  do { \
    if (!(what)) { \
      printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #what); \
      failed = 1; \
    } \
  } while (0)

enum { MS = 1000 }; // ticks in a millisecond, for this test
static const unsigned long long QUIET = (unsigned long long)OPEN_WARM_QUIET_MS * MS;

int main(void) {
  // A fault loop: three restarts, then no more however long it goes on.
  struct open_warm w = {0};
  unsigned long long now = 5000 * MS;
  for (int i = 0; i < OPEN_WARM_LIMIT; i++)
    CHECK(open_warm_allowed(&w, now += 1 * MS, QUIET));
  for (int i = 0; i < 1000; i++)
    CHECK(!open_warm_allowed(&w, now += 1 * MS, QUIET));
  CHECK(w.total == OPEN_WARM_LIMIT);

  // Pacing the faults just inside the quiet period does not earn more:
  // every fault, allowed or not, restarts the wait.
  w = (struct open_warm){0};
  now = 5000 * MS;
  int allowed = 0;
  for (int i = 0; i < 50; i++)
    allowed += open_warm_allowed(&w, now += QUIET, QUIET);
  CHECK(allowed == OPEN_WARM_LIMIT);

  // After a quiet period the count starts over, but only up to the cap per
  // boot: faults spaced out past the quiet period are not restarted forever.
  w = (struct open_warm){0};
  now = 5000 * MS;
  for (int i = 0; i < OPEN_WARM_TOTAL; i++)
    CHECK(open_warm_allowed(&w, now += QUIET + 1, QUIET));
  CHECK(w.total == OPEN_WARM_TOTAL && w.count == 1);
  for (int i = 0; i < 1000; i++)
    CHECK(!open_warm_allowed(&w, now += 100 * QUIET, QUIET));
  CHECK(w.total == OPEN_WARM_TOTAL);

  // Three every quiet period (the most a paced attacker can get) still ends at the cap.
  w = (struct open_warm){0};
  now = 5000 * MS;
  allowed = 0;
  for (int i = 0; i < 300; i++)
    allowed += open_warm_allowed(&w, now += (i % 3 == 0) ? QUIET + 1 : 1, QUIET);
  CHECK(allowed == OPEN_WARM_TOTAL);

  // A burst, a quiet period, another burst.
  w = (struct open_warm){0};
  now = 5000 * MS;
  CHECK(open_warm_allowed(&w, now, QUIET));
  CHECK(open_warm_allowed(&w, now + 1, QUIET));
  CHECK(open_warm_allowed(&w, now + 2, QUIET));
  CHECK(!open_warm_allowed(&w, now + 3, QUIET));
  now += 3 + QUIET + 1;
  CHECK(open_warm_allowed(&w, now, QUIET));
  CHECK(open_warm_allowed(&w, now + 1, QUIET));
  CHECK(open_warm_allowed(&w, now + 2, QUIET));
  CHECK(!open_warm_allowed(&w, now + 3, QUIET));

  // A fault in the first moments after power-on (the clock near zero) is allowed.
  w = (struct open_warm){0};
  CHECK(open_warm_allowed(&w, 0, QUIET));
  CHECK(open_warm_allowed(&w, 1, QUIET));
  CHECK(open_warm_allowed(&w, 2, QUIET));
  CHECK(!open_warm_allowed(&w, 3, QUIET));

  if (!failed)
    printf("open_recover: ok\n");
  return failed;
}
