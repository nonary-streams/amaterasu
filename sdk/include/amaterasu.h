#ifndef AMATERASU_H
#define AMATERASU_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Draft ABI. Tables may grow at the end; check size and major before use. */
#define AMATERASU_ABI_MAJOR 1
#define AMATERASU_STATUS_OK 0
#define AMATERASU_STATUS_INVALID_ARGUMENT 1
#define AMATERASU_STATUS_INTERNAL_ERROR 2
/* The callee is live but does not serve this operation here. Nothing happened;
 * the caller may use another transport for the same operation. */
#define AMATERASU_STATUS_UNAVAILABLE 3

typedef int32_t AmaterasuStatus;

typedef struct {
    uint32_t size;
    uint32_t major;
    uint32_t minor;
} AmaterasuAbiHeader;

typedef struct {
    const uint8_t *ptr;
    size_t len;
} AmaterasuBorrowedBytes;

/* Reply bytes are borrowed only for the duration of write(). Copy before returning. */
typedef struct {
    void *context;
    void (*write)(void *context, AmaterasuBorrowedBytes bytes);
} AmaterasuReplySink;

/* invoke/context support concurrent consumer-worker calls. The endpoint keeps
 * the same runtime principal across OS threads. Native guards may release on
 * their final owning worker; runtime destruction follows every guard/call. */
typedef struct {
    AmaterasuAbiHeader header;
    void *context;
    AmaterasuStatus (*invoke)(void *context, AmaterasuBorrowedBytes input, AmaterasuReplySink reply);
} AmaterasuHostApi;

/* Lifecycle calls are serialized. create() borrows the host table during the call;
 * copy it if needed later. destroy() must join all module-owned threads. */
typedef struct {
    AmaterasuAbiHeader header;
    AmaterasuStatus (*create)(const AmaterasuHostApi *host, void **instance);
    AmaterasuStatus (*tick)(void *instance);
    AmaterasuStatus (*health)(void *instance);
    void (*destroy)(void *instance);
} AmaterasuModuleApi;

/* Optional amaterasu_module_quiesce_v1() export, using the ModuleApi instance.
 * poll is bounded/nonblocking and writes ready=0 or 1. Declared consumers drain
 * before their providers begin quiescing. All modules continue to receive ticks
 * until every participant is ready, before reverse-order destroy. */
typedef struct {
    AmaterasuAbiHeader header;
    AmaterasuStatus (*poll)(void *instance, uint8_t *ready);
} AmaterasuModuleQuiesceApi;

/* Optional amaterasu_module_shutdown_v1() export, using the ModuleApi instance.
 * The host announces once to every module before any quiescence poll. prepare
 * is bounded/nonblocking and receives the remaining shared budget in milliseconds;
 * retain an absolute deadline rather than renewing a per-module timeout. Close
 * admission only: keep serving owned work and defer provider cleanup until poll.
 * Announcement alone never establishes readiness for destruction. */
typedef struct {
    AmaterasuAbiHeader header;
    AmaterasuStatus (*prepare)(void *instance, uint64_t remaining_ms);
} AmaterasuModuleShutdownApi;

typedef struct {
    AmaterasuAbiHeader header;
    AmaterasuStatus (*create)(AmaterasuBorrowedBytes configuration, void **instance);
    AmaterasuStatus (*endpoint)(void *instance, AmaterasuBorrowedBytes principal, AmaterasuHostApi *output);
    void (*destroy)(void *instance);
} AmaterasuRuntimeApi;

/* Optional amaterasu_runtime_wait_v1() export, using the RuntimeApi instance.
 * Observe before ticking, then wait against that generation to avoid lost
 * wakeups. wait accepts at most 1,000,000 microseconds; it may return early.
 * NotifyWorkReady is a scheduling hint through any existing HostApi endpoint. */
typedef struct {
    AmaterasuAbiHeader header;
    AmaterasuStatus (*observe)(void *instance, uint64_t *generation);
    AmaterasuStatus (*wait)(void *instance, uint64_t generation, uint64_t timeout_us);
} AmaterasuRuntimeWaitApi;

/* Product interface tables begin with this prefix. Their size/version describe
 * the product table. Context and table remain live until release(context). */
typedef struct {
    AmaterasuAbiHeader header;
    void *context;
    void (*release)(void *context);
} AmaterasuNativeInterface;

typedef struct {
    AmaterasuAbiHeader header;
    uint32_t interface_major;
    uint32_t min_minor;
    uint32_t min_size;
} AmaterasuNativeInterfaceQuery;

/* Factory descriptor is copied during registration; context/code must stay live.
 * The runtime supplies the authenticated caller; factories copy it if retained.
 * Success transfers one provider-owned binding, failure leaves output NULL. */
typedef struct {
    AmaterasuAbiHeader header;
    void *context;
    AmaterasuStatus (*bind)(void *context, AmaterasuBorrowedBytes caller,
        const AmaterasuNativeInterfaceQuery *query,
        const AmaterasuNativeInterface **output);
} AmaterasuNativeServiceFactory;

/* Flow tracing. Send {"op":"trace_bind"} through HostApi; a supporting runtime
 * replies {"kind":"trace_bound","address":N} where N is the address of this
 * endpoint's AmaterasuTraceSinkApi, valid until the runtime is destroyed. Any
 * other reply means tracing is unavailable and the module records nothing. */
#define AMATERASU_TRACE_MARK 1 /* the flow reached a stage; the first record starts it */
#define AMATERASU_TRACE_END 2  /* final stage; value is the slow budget in us, 0 = default */
#define AMATERASU_TRACE_SPAN 3 /* completed work on one thread; value is its duration in us */
#define AMATERASU_MAX_TRACE_NAME_BYTES 96
#define AMATERASU_MAX_TRACE_RECORDS_PER_CALL 64

/* One observation. No pointers; it can be copied between threads. */
typedef struct {
    uint64_t flow;         /* identity of the traced item within its track */
    uint64_t value;        /* kind-specific, see the kinds above */
    uint64_t timestamp_us; /* from now(); 0 asks the sink to stamp on arrival */
    uint16_t track;        /* interned flow family, or the span's name */
    uint16_t stage;        /* interned stage, or the span's label (0 = none) */
    uint8_t kind;
    uint8_t reserved[3];
} AmaterasuTraceRecord;

/* Every function is thread-safe. record() never blocks or allocates; a full
 * buffer drops and counts records. intern() is a cold path that writes a stable
 * nonzero id for a name of letters, digits and . _ - / @ : characters. */
typedef struct {
    AmaterasuAbiHeader header;
    void *context;
    uint64_t (*now)(void *context);
    AmaterasuStatus (*intern)(void *context, AmaterasuBorrowedBytes name, uint16_t *id);
    AmaterasuStatus (*record)(void *context, const AmaterasuTraceRecord *records, size_t count);
} AmaterasuTraceSinkApi;

/* Optional amaterasu_module_direct_v1() export, using the ModuleApi instance.
 * Serves one request of a broker service this module provides, synchronously
 * on the caller's thread. STATUS_OK means exactly one reply was written;
 * STATUS_UNAVAILABLE means nothing was done and the caller will submit an
 * ordinary request. The runtime has already checked the caller's call grant.
 * It is invoked only on the thread that makes this module's lifecycle calls.
 * A module must refuse a call that arrives while another call into the same
 * instance is active. The call must be bounded and must not block. */
typedef struct {
    AmaterasuAbiHeader header;
    AmaterasuStatus (*call)(void *instance, AmaterasuBorrowedBytes service,
        AmaterasuBorrowedBytes caller, AmaterasuBorrowedBytes request, AmaterasuReplySink reply);
} AmaterasuModuleDirectApi;

/* Optional amaterasu_runtime_direct_v1() export, called only by the host with
 * handlers read from verified module libraries. register binds the calling
 * thread as the only thread that may reach the instance; after unregister
 * returns the handler is never invoked. */
typedef struct {
    AmaterasuAbiHeader header;
    AmaterasuStatus (*register_handler)(void *runtime, AmaterasuBorrowedBytes principal, void *instance,
        AmaterasuStatus (*call)(void *instance, AmaterasuBorrowedBytes service,
            AmaterasuBorrowedBytes caller, AmaterasuBorrowedBytes request, AmaterasuReplySink reply));
    AmaterasuStatus (*unregister_handler)(void *runtime, AmaterasuBorrowedBytes principal);
} AmaterasuRuntimeDirectApi;

const AmaterasuModuleApi *amaterasu_module_v1(void);
const AmaterasuModuleQuiesceApi *amaterasu_module_quiesce_v1(void);
const AmaterasuModuleShutdownApi *amaterasu_module_shutdown_v1(void);
const AmaterasuModuleDirectApi *amaterasu_module_direct_v1(void);
const AmaterasuRuntimeApi *amaterasu_runtime_v1(void);
const AmaterasuRuntimeWaitApi *amaterasu_runtime_wait_v1(void);
const AmaterasuRuntimeDirectApi *amaterasu_runtime_direct_v1(void);

#ifdef __cplusplus
}
#endif
#endif
