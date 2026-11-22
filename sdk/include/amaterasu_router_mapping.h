#ifndef AMATERASU_ROUTER_MAPPING_H
#define AMATERASU_ROUTER_MAPPING_H

#include "amaterasu.h"

#ifdef __cplusplus
extern "C" {
#endif

#define AMATERASU_ROUTER_MAPPING_CONTRACT "amaterasu.router-mapping-interface/1"
#define AMATERASU_ROUTER_MAPPING_SERVICE "amaterasu.router-mapping/1"
#define AMATERASU_ROUTER_MAPPING_INTERFACE_MAJOR 1u
#define AMATERASU_ROUTER_MAPPING_INTERFACE_MINOR 0u
#define AMATERASU_ROUTER_MAPPING_MAX_ENDPOINTS 64u
#define AMATERASU_ROUTER_MAPPING_MAX_REQUEST_BYTES (16u * 1024u)
#define AMATERASU_ROUTER_MAPPING_MAX_REPLY_BYTES 2048u

/* One listener mapping lease. Supply one bounded JSON response to the
 * synchronous reply sink. Table calls may enqueue work or poll status, and
 * must return without network I/O or joining a live worker. Release stops and
 * joins the binding's workers before returning. See docs/router-mapping-sdk.md
 * for request and response schemas. */
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*request)(
        void *context, AmaterasuBorrowedBytes input, AmaterasuReplySink reply);
} AmaterasuRouterMappingApi;

#ifdef __cplusplus
}
#endif

#endif
