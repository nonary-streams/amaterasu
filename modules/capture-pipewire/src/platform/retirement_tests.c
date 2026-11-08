/* A separate archive object exercises the real private retirement machinery
 * without opening a portal, PipeWire connection, or display. */
#define am_pw_probe am_pw_fixture_probe
#define am_pw_memfd_extent_valid am_pw_fixture_memfd_extent_valid
#define am_pw_start am_pw_fixture_start
#define am_pw_valid_named_output am_pw_fixture_valid_named_output
#define am_pw_start_output am_pw_fixture_start_output
#define am_pw_snapshot am_pw_fixture_snapshot
#define am_pw_begin_stop am_pw_fixture_begin_stop
#define am_pw_stopped am_pw_fixture_stopped
#define am_pw_destroy am_pw_fixture_destroy
#include "adapter.c"
#include <time.h>

struct retirement_fixture {
    sem_t cancel_entered;
    sem_t release_cancel;
    sem_t cancel_finished;
    sem_t worker_entered;
    sem_t release_worker;
    atomic_bool cancellation_timed_out;
};

static bool fixture_wait(sem_t *signal, time_t seconds) {
    struct timespec deadline;
    if (clock_gettime(CLOCK_REALTIME, &deadline) != 0) return false;
    deadline.tv_sec += seconds;
    int result;
    do { result = sem_timedwait(signal, &deadline); } while (result != 0 && errno == EINTR);
    return result == 0;
}

static void fixture_cancelled(GCancellable *cancel, gpointer data) {
    (void)cancel;
    struct retirement_fixture *fixture = data;
    sem_post(&fixture->cancel_entered);
    if (!fixture_wait(&fixture->release_cancel, 2)) {
        atomic_store(&fixture->cancellation_timed_out, true);
    }
    sem_post(&fixture->cancel_finished);
}

static gpointer fixture_worker(gpointer data) {
    struct retirement_fixture *fixture = data;
    sem_post(&fixture->worker_entered);
    (void)fixture_wait(&fixture->release_worker, 4);
    return NULL;
}

/* Return zero only when the callback and the worker are both allowed to
 * finish before the reaper is reported joined. Every fixture wait is bounded. */
int am_pw_test_retirement_barrier(void) {
    struct retirement_fixture fixture = {0};
    atomic_init(&fixture.cancellation_timed_out, false);
    sem_t *signals[] = { &fixture.cancel_entered, &fixture.release_cancel,
                         &fixture.cancel_finished, &fixture.worker_entered,
                         &fixture.release_worker };
    size_t initialized = 0;
    while (initialized < sizeof(signals) / sizeof(signals[0])) {
        if (sem_init(signals[initialized], 0, 0) != 0) {
            while (initialized > 0) sem_destroy(signals[--initialized]);
            return 1;
        }
        ++initialized;
    }
    struct am_pw_session *session = g_new0(struct am_pw_session, 1);
    g_mutex_init(&session->geometry_lock);
    atomic_init(&session->stopped, false);
    atomic_init(&session->cleanup_complete, false);
    atomic_init(&session->reap_signaled, false);
    session->cancel = g_cancellable_new();
    g_signal_connect(session->cancel, "cancelled", G_CALLBACK(fixture_cancelled), &fixture);
    if (sem_init(&session->reap_requested, 0, 0)) {
        g_object_unref(session->cancel); g_mutex_clear(&session->geometry_lock); g_free(session);
        while (initialized > 0) sem_destroy(signals[--initialized]);
        return 2;
    }
    if (pthread_create(&session->reaper, NULL, reap_worker, session)) {
        sem_destroy(&session->reap_requested); g_object_unref(session->cancel);
        g_mutex_clear(&session->geometry_lock); g_free(session);
        while (initialized > 0) sem_destroy(signals[--initialized]);
        return 3;
    }
    session->thread = g_thread_try_new("retirement-test", fixture_worker, &fixture, NULL);
    if (!session->thread) {
        sem_post(&session->reap_requested);
        pthread_join(session->reaper, NULL);
        sem_destroy(&session->reap_requested); g_object_unref(session->cancel);
        g_mutex_clear(&session->geometry_lock); g_free(session);
        while (initialized > 0) sem_destroy(signals[--initialized]);
        return 4;
    }
    int failure = 0;
    if (!fixture_wait(&fixture.worker_entered, 2)) failure = 5;
    am_pw_fixture_begin_stop(session);
    if (!fixture_wait(&fixture.cancel_entered, 2)) failure = 6;
    if (atomic_load(&fixture.cancellation_timed_out) || am_pw_fixture_stopped(session)) failure = 7;
    sem_post(&fixture.release_cancel);
    if (!fixture_wait(&fixture.cancel_finished, 2)) failure = 8;
    if (am_pw_fixture_stopped(session)) failure = 9;
    sem_post(&fixture.release_worker);
    bool stopped = false;
    for (unsigned attempt = 0; attempt < 2000 && !stopped; ++attempt) {
        stopped = am_pw_fixture_stopped(session) != 0;
        if (!stopped) {
            const struct timespec pause = { .tv_sec = 0, .tv_nsec = 1000000 };
            nanosleep(&pause, NULL);
        }
    }
    if (!stopped) failure = 10;
    am_pw_fixture_destroy(session);
    while (initialized > 0) sem_destroy(signals[--initialized]);
    return failure;
}
