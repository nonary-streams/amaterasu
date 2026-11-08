# Display Settings SDK

The Display Settings SDK is a provider-neutral API for temporary changes to a
specific display. It is separate from effect settings, stream negotiation and
persistent user preferences. A plugin decides what adjustment it needs; the
selected display-settings provider applies, verifies, enforces and restores it.

The public Rust interface is `amaterasu_module_sdk::display_settings`. The
versioned JSON contract is `nonary.display-settings/1`; trusted startup
composition may select a compatible provider under another service alias.
Methods are `enumerate`, `read`, `open_scope`, `acquire`, `status`, `release`,
and `close_scope`. Requests use the SDK's versioned operation envelope. Raw
unversioned requests are not accepted by the default provider.

## Source-scoped plugin use

1. The source owner creates its display and opens a scope with the selected
   provider. Only configured registrars may open scopes, and only configured
   consumers may receive access. The provider resolves the source selector;
   plugins receive an opaque target and scope, not native display identities.
2. The host calls selected `prepare_source` hooks with that scope, the frozen
   plugin settings and revision, lifecycle target, and negotiated output format.
3. A plugin uses an ordinary asynchronous SDK request to acquire an adjustment:

```rust
use amaterasu_module_sdk::display_settings::{Adjustment, HdrMode, Operation};

let command = Operation::Acquire {
    operation_id: adjustment_operation_id,
    scope: context.display_scope.clone(),
    adjustment: Adjustment::new()
        .hdr(HdrMode::Disabled)
        .wide_color_gamut(false),
}.command(request_options)?;
```

The plugin must consume and release the broker result, and accept success only
through `lease_from_reply`, before completing preparation. A provider's admission
is not proof of application. RTX HDR uses this path to keep its capture display
SDR while producing HDR pixels. It disables both HDR and wide color gamut:
disabling HDR alone may leave a wide-gamut source that is not sRGB. Both
properties are independently verified, enforced, and restored. The session
scheduler does not inspect the requested HDR, gamut, resolution, refresh rate,
scale, or position adjustment.

4. Capture starts after every selected hook completes. Joining clients reuse
   the original source and its scope; they do not reacquire or retarget it.
5. After the last consumer retires, the source owner closes the scope before
   removing the display. This restores the properties leased by its plugins.
   Failed restoration retains the source and durable cleanup obligation for retry.

Individual plugins may release their own lease earlier. Other plugins' leases
continue to apply. An incompatible adjustment fails explicitly, while matching
requirements and changes to independent properties can coexist.

## Lifetime and failure semantics

The provider journals the original values and the intended mutation before
calling native APIs. It reports success only after readback verification.
A later failed write or process crash preserves the pending intent for recovery.
Periodic reconciliation enforces active leases against external changes.
Restoration changes only properties that were leased, preserving unrelated
settings. Once an old target generation disappears, its adjustment is never
restored onto a replacement output.

Cancelling a broker request does not prove that its display mutation never ran.
The source owner's durable scope is the recovery authority: closing it releases
all accepted plugin leases, including ones whose replies were lost. Native
restoration is verified before the scope is reported closed.

Operation IDs must identify one logical operation. Retrying the same acquisition
with different settings is rejected. Returned scope identities are fresh, and
retired receipt storage is bounded; stale scope handles cannot regain access
when old receipts are evicted. The provider caps live scopes and leases.

Native plugins are trusted code. SDK scopes and broker grants express cooperative
resource ownership and authorization; they are not a native-code sandbox.

## Default provider

`amaterasu-display-settings` is an independently built implementation, selected
by the Linux RTX HDR package profile. Its current native backend targets KScreen
in a Wayland desktop and supports HDR, wide color gamut, resolution, refresh rate, scale, and
position. Unsupported properties or modes fail instead of being silently ignored.
Display generation includes the Wayland socket identity, so a compositor restart
invalidates old targets. Other platforms can implement the same operation contract
without changing consumers; the current native backend does not claim Windows support.

The existing `processing_intent` source/output ranges remain frame-format
validation contracts. With source-preparation hooks selected, they do not set the
owned display's HDR state; the plugin's Display Settings SDK lease controls it.
