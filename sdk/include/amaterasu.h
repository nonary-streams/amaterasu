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
 * poll is bounded/nonblocking and writes ready=0 or 1. All modules continue to
 * receive ticks until every participant is ready, before reverse-order destroy. */
typedef struct {
    AmaterasuAbiHeader header;
    AmaterasuStatus (*poll)(void *instance, uint8_t *ready);
} AmaterasuModuleQuiesceApi;

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

const AmaterasuModuleApi *amaterasu_module_v1(void);
const AmaterasuRuntimeApi *amaterasu_runtime_v1(void);
const AmaterasuRuntimeWaitApi *amaterasu_runtime_wait_v1(void);

#ifdef __cplusplus
}
#endif
#endif
