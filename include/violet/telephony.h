#ifndef VIOLET_TELEPHONY_H
#define VIOLET_TELEPHONY_H

/* Proposed UI-facing service contract. Single SIM, one voice call at a time.
 * No backend is linked yet. Operations return 0 or a negative errno value.
 * Commands acknowledge local acceptance; read state for the modem outcome.
 * Invoke all operations on the same UI event loop. Reads must not block on I/O.
 */
enum violet_call_state {
    VIOLET_CALL_UNAVAILABLE,
    VIOLET_CALL_IDLE,
    VIOLET_CALL_DIALING,
    VIOLET_CALL_RINGING,
    VIOLET_CALL_ACTIVE,
    VIOLET_CALL_ENDING
};

struct violet_call_status {
    enum violet_call_state state;
    int last_error; /* 0 or a negative errno for the most recent call failure. */
};

int violet_call_read(struct violet_call_status *out);
/* The backend copies the NUL-terminated dial string before returning. */
int violet_call_dial(const char *number);
int violet_call_answer(void);
int violet_call_end(void);

#endif
