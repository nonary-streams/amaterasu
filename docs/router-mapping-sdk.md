# Router mapping provider SDK

NVHTTP calculates desired mappings from live listener ports, address-family
policy, and management's explicit WAN exposure. It sends that plan to the
startup-selected provider through `amaterasu_module_sdk::router_mapping::RouterMapping`.
The provider owns discovery, mapping and pinhole leases, refresh, and cleanup.
The default provider uses the Network SDK for router I/O.

The packaged default component is `amaterasu.router-mapping` and serves
`amaterasu.router-mapping/1`. Set NVHTTP's trusted `router_mapping_provider`
setting to a versioned alias to select another provider, or to `null` to disable
the route. The signed composition must grant NVHTTP native use of the alias and
depend on its provider component. A missing or denied provider leaves the
listeners available and reports a mapping error if UPnP is requested.

Providers register the typed family `amaterasu.router-mapping-interface/1`.
The C declaration is in `sdk/include/amaterasu_router_mapping.h`. The table's
synchronous `request` callback accepts bounded UTF-8 JSON and returns one
bounded JSON object through the reply sink. Its work on the host thread is
limited to validation, scheduling, and polling. The lifetime of a mapping
lease is the lifetime of the native binding.

| Request | Response |
| --- | --- |
| `{"action":"update","desired":{"enabled":true,"ipv6_enabled":false,"endpoints":[{"address":"0.0.0.0","port":47989,"protocol":"tcp","active":true}]}}` | `{"result":"status","status":{"status":"active","error":null}}` |
| `{"action":"status"}` | The last observed status in the same response format |
| `{"action":"prepare_shutdown","remaining_ms":500}` | `{"result":"accepted"}` |
| `{"action":"poll_shutdown"}` | `{"result":"shutdown","ready":false,"error":null}` while pending |

An update records desired state for the owned lease. It does not prove the
router accepted a mapping. Providers should report `active` only after a
mapping or pinhole is confirmed. Unsupported routes and router errors remain
visible as bounded advisory status. Addresses may be unspecified to describe
wildcard listeners. The provider resolves such a listener to an eligible router
interface before making a mapping.

Preparation may tighten the existing shutdown deadline. Polling initiates
retirement and reports `ready: true` only after the binding's workers settle and
owned effects are retired. A failed or unconfirmed cleanup includes an error
and retains the exact outstanding effects for the lifetime of the binding.
Release joins the provider's workers under the configured deadline.
