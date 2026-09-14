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

typedef struct {
    AmaterasuAbiHeader header;
    AmaterasuStatus (*create)(AmaterasuBorrowedBytes configuration, void **instance);
    AmaterasuStatus (*endpoint)(void *instance, AmaterasuBorrowedBytes principal, AmaterasuHostApi *output);
    void (*destroy)(void *instance);
} AmaterasuRuntimeApi;

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

#ifdef __cplusplus
}
#endif
#endif
