# Web extension UI controls

Browser page extensions execute as trusted same-origin JavaScript. The UI SDK
provides reusable, accessible controls; it does not sandbox a publisher or grant
additional API methods. The signed extension manifest and host authorization
remain the only authority for `sdk.call`.

The additive status, alert, and progress components are documented in the
[feedback UI SDK guide](web-extension-ui-feedback.md); they use the separate
`ui.feedback/1` capability while keeping `sdk.ui.version` at 1.
The semantic card and table components use the separate additive
`ui.layout/1` capability. The native modal dialog uses the additive `ui.dialog/1`
capability documented in the [dialog UI SDK guide](web-extension-ui-dialog.md).
These capabilities do not change `sdk.version` or `sdk.ui.version`.

Hosts that implement the base controls advertise `ui.controls/1` in the
additive `sdk.features` list and supply `sdk.ui` with `sdk.ui.version === 1`.
The form field options and dynamic choices are an additive `ui.forms/1` feature.
Runtime field state updates through `setState()` are an additive `ui.forms/2`
feature. Text fields, number fields, and action buttons are an additive
`ui.controls/2` feature. Feature checks are optional in SDK v1 so pages can
detect older hosts:

```js
if (!sdk.features?.includes("ui.controls/1") ||
    !sdk.features.includes("ui.forms/1") || sdk.ui?.version !== 1) {
  // Render a compatible fallback or a clear unsupported-host message.
}
```

Plugin tabs are a separate additive `ui.tabs/1` capability. Check for it before
calling `sdk.ui.tabs()`; older SDK v1 hosts continue to provide the controls and
feedback features they advertised.

Existing SDK v1 pages continue to run on older hosts. New pages that need only base controls can check `ui.controls/1`; pages that use form metadata or `setChoices()` must also check `ui.forms/1`; pages that call `setState()` must check `ui.forms/2`; pages that use `textField()`, `numberField()`, or `button()` must check `ui.controls/2`. Hosts without the newer features continue to provide the original controls and their creation-time options. The control contract provides `toggle`, `select`, `radioGroup`, `slider`, `textField`, `numberField`, and `button`.

Field controls accept disabled, busy, read-only, required, help, and error; action
buttons accept disabled, busy, help, and error. Busy sets aria-busy; on fields it
reports ongoing work, while disabled blocks editing. Help and error use the same localized text format as labels;
the SDK connects them with `aria-describedby`, sets `aria-invalid` for errors,
and exposes errors as alerts. Native controls enforce disabled and required
states. Read-only controls remain focusable and expose `aria-readonly` while
preventing user changes. Non-mutating keyboard shortcuts remain available. In a
read-only radio group, Tab enters at the checked option; Arrow keys and Home/End
move focus through choices without changing the checked value. Assistive technology
support for `aria-readonly` on radiogroups varies, though option labels and checked
state remain available. The paired read-only slider leaves its native number input
keyboard shortcuts intact and blocks value-changing keys on the range input.
`help` and `error` text sit outside the clickable label and are associated with
the control through `aria-describedby`. Field controls also support programmatic
`setValue()` updates.
Toggle, select, radioGroup, slider, textField, and numberField return field
controls with `element`, `value()`, `setValue(value)`, `setState(patch)`, and
`dispose()`. State patches are partial: omitted properties stay unchanged,
booleans accept `true` or `false`, and pass `null` for help or error to clear
those descriptions. Invalid patches fail before changing the control. State
updates preserve node identity and focus; disabling the focused native control
lets the browser move focus according to its disabled-control behavior.

`button()` returns an action control with `element`, `busy()`, `setState(patch)`,
and `dispose()`. Its state patch accepts disabled, busy, help, and error. A
button's `setState()` is part of `ui.controls/2`; state updates on the original
field controls are part of `ui.forms/2`.

Pages using field `setState()` should require `ui.forms/2`; action button state
updates require `ui.controls/2`:

```js
if (!sdk.features?.includes("ui.forms/2")) {
  throw new Error("This host does not support dynamic form state.");
}

const automatic = sdk.ui.toggle({ label: "Automatic quality", value: true });
automatic.setState({ disabled: true, help: "Saving…", error: null });
// On completion, clear the transient state and show any validation error.
automatic.setState({ disabled: false, help: null, error: validationError ?? null });
```

Choice controls also provide `setChoices(choices, value?)`. The current
selection is retained unless a new value is supplied; if the current value is not
in the new list, the update fails without changing the control.

A radio group's focused choice remains focused if it survives replacement; if
removed, focus moves to the selected choice. Programmatic choice updates do not
call `onChange`.

Text fields accept an optional localized `placeholder` that refreshes when the
host locale changes, plus a bounded `maxLength`. Number fields accept
optional `minimum`, `maximum`, and positive `step` (or "any");
invalid or out-of-range edits do not fire `onChange`, and `value()` throws until
the input is valid. Programmatic `setValue()` validates before changing the input.
Both fields use native required/disabled/read-only behavior, localized labels and
help/error descriptions, and lifecycle-bound listeners.

`button()` creates a native `type="button"` action with a localized label and an
optional `primary`, `secondary`, or `danger` variant. Its `onClick` callback may
return a promise; while it is pending, the button becomes disabled and exposes
`aria-busy`. Use `setState({ busy: true })` for work started outside the callback.
Buttons support dynamic disabled, busy, help, and error state. Help/errors are
associated with the button, errors are announced as alerts, and state changes
preserve focus and listeners. `dispose()` and the host lifecycle signal remove
button callback listeners. Check `ui.controls/2` before using these factories:

```js
if (!sdk.features?.includes("ui.controls/2")) {
  throw new Error("This host does not support text, number, and action controls.");
}

const search = sdk.ui.textField({ label: "Search", value: "", placeholder: "Name or ID" });
const maxItems = sdk.ui.numberField({ label: "Maximum results", value: 20, minimum: 1, maximum: 100, step: 1 });
const refresh = sdk.ui.button({ label: "Refresh", variant: "primary", onClick: async () => load(search.value(), maxItems.value()) });
container.append(search.element, maxItems.element, refresh.element);
```

Append each control's `element` to the page container. Values are local UI state;
the extension remains responsible for validation, persistence, and approved service calls.

## Plugin tabs

sdk.ui.tabs() is available with ui.tabs/1. Each tab has a stable unique ID, a
localized label, an HTMLElement content node, and optional disabled state. IDs
are unique, 1-128 ASCII letters, digits, dots, underscores, colons, or hyphens,
starting with a letter or digit; at most 64 tabs are allowed. Content is inserted
as a DOM node; the SDK never parses plugin HTML strings. The tablist and panels
receive linked ARIA roles and IDs. Selected panels are shown; other panels are
hidden. Labels refresh when the host locale changes.

```js
if (!sdk.features?.includes("ui.tabs/1")) {
  throw new Error("This host does not support reusable plugin tabs.");
}

const overview = document.createElement("section");
overview.textContent = "Overview content";
const advanced = document.createElement("section");
advanced.textContent = "Advanced content";

const tabs = sdk.ui.tabs({
  label: { source: "Settings sections", translations: { fr: "Settings sections (FR)" } },
  tabs: [
    { id: "overview", label: "Overview", content: overview },
    { id: "advanced", label: "Advanced", content: advanced, disabled: true },
  ],
  onChange(id) { console.log("Selected tab:", id); },
});
container.append(tabs.element);
```

Arrow keys move focus and activate the next or previous enabled tab, wrapping at
the ends; Home and End move to the first and last enabled tabs. Clicks and keyboard
activation call onChange when the selection changes. Programmatic select(id),
setTabs(), and setDisabled() do not call onChange. Selecting an unknown or disabled
tab throws. If the active tab becomes disabled or is removed, the first enabled
tab becomes active; all-disabled sets have no active panel, and re-enabling a tab
selects it when none is active. Updating tabs reuses
surviving tab and panel nodes and preserves tab focus when possible. dispose()
removes event listeners, and the page lifecycle signal disposes the control on
navigation.

The control exposes element, active(), select(id), setTabs(tabs, activeId?),
setDisabled(id, disabled), and dispose(). active() returns the selected tab ID or
null when all tabs are disabled.

Labels and choice labels accept plain source text or `{ source, translations }`.
Translations are keyed by locale (for example `fr` or `fr-CA`); exact locale and
language fallback are tried before the host's translation catalog. Choice lists
contain between one and 64 unique, nonempty wire values. Labels are
inserted as text, never interpreted as markup. Native form controls provide
keyboard and assistive technology behavior. The host lifecycle signal disposes
all SDK control listeners on navigation or signout. Call `dispose()` sooner when
a control is removed before its page.

Control callbacks fire for user input. `setValue()` updates the control without
calling `onChange`, so consumers can restore or normalize local state without
simulating user input. `slider` accepts a positive numeric step or omits `step`
to allow fractional values. Its paired range and number inputs
share the same bounds and accessible name.

```js
const quality = sdk.ui.radioGroup({
  label: { source: "Quality", translations: { fr: "Qualité" } },
  choices: [
    { value: "balanced", label: "Balanced" },
    { value: "sharp", label: "Sharp" },
  ],
  value: "balanced",
  onChange(value) { selectedQuality = value; },
});
container.append(quality.element);
return () => quality.dispose();
```

The controls use host semantic classes and theme variables. Plugins can add
page-specific markup and styles around them; no product-specific provider,
Vibeshine feature, or fixed palette is part of this API.

## Semantic cards and tables (`ui.layout/1`)

`ui.layout/1` is independent of the form, control, and feedback capabilities.
Check the feature before using its factories; a host without it keeps the same
version 1 SDK and existing controls:

```js
if (sdk.version !== 1 || sdk.ui?.version !== 1 ||
    !sdk.features?.includes("ui.layout/1")) {
  // Render a compatible fallback.
  return;
}
```

`card({ label })` returns a labelled native section with an `h2` and a content
slot. Append plugin-created DOM nodes through `card.append(node)` or the
`content` element; strings are rejected, and HTML strings are never parsed.
Build children with `document.createElement()` and set text through
`textContent`. The card label accepts the same localized text values as other
controls.

`table({ caption, columns, rows? })` returns a keyboard-focusable, horizontally
scrollable region containing a native table. The table has a localized caption,
column header cells with `scope="col"`, and text-only data cells. `setRows()`
replaces the rows without rebuilding the table headers. Each row must have one
cell per column. Tables allow up to 32 columns and 1,000 rows; sorting,
selection, pagination, and virtualization remain the page's responsibility.
Caption, header, and cell strings use the same locale resolution as controls.
No table cell value is interpreted as markup.

Both components expose `element` and idempotent `dispose()`. The host lifecycle
signal disposes them on navigation or signout; disposal removes the component
from the DOM and prevents later updates. Call `dispose()` sooner when removing a
component before its page ends.

## Application editor sections

With `ui.application-section/1`, a signed extension can add its own section to
an existing application's editor. This surface is independent of the settings
registry: a section can provide actions, status, help, or a complete plugin UI
without declaring configuration overrides. The host invokes the selected plugin;
it does not hard-code plugin-specific controls.

Declare the locations in the signed manifest:

```json
{"placements": ["application"]}
```

Use `["page", "application"]` for both a navigation page and an app section.
Existing manifests without `placements` retain `["page"]`. Unknown, duplicate,
or empty placements are rejected. Application-only extensions do not add a
navigation item. The publisher's optional JSON configuration accepts either the
existing method array or an object containing `placements` and `methods`.

Register `mountApplication` for the new surface:

```js
window.AmaterasuExtensions.v1.register('my-plugin', {
  title: 'My plugin',
  mountApplication(container, sdk) {
    const app = sdk.application;
    const action = sdk.ui.button({
      label: 'Inspect application',
      onClick: async () => {
        sdk.setBusy(true);
        try {
          await sdk.call('inspect', { application_id: app.id });
        } finally {
          sdk.setBusy(false);
        }
      },
    });
    container.append(action.element);
    return () => action.dispose();
  },
});
```

The frozen `sdk.application` contains the catalog `id` and display `name`.
It is context, not an authorization grant. `sdk.call` still permits only methods
in the signed manifest and the host's independent method approvals. Provider
methods must validate their inputs and permissions; adding a section does not
create a new API route or widen a user's access.

Each mounted section has its own SDK, `AbortSignal`, controls, and cleanup
function. Changing applications, closing the editor, navigating, or signing out
aborts the signal and invokes cleanup. Late script loads never mount into a
closed editor. Check the signal before applying asynchronous results. Multiple
app sections can coexist without sharing draft or lifecycle state.

Call `sdk.setDirty(true)` for unsaved work and clear it only after saving or
explicitly discarding that work. `sdk.setBusy(true)` prevents closing the editor
while a plugin operation is in progress. The host includes both states in its
navigation and application-save guards. These methods are available on the
application surface and do not persist settings themselves.

A selected plugin can own its settings UI by adding a `settings_provider` to
its signed application configuration:

```json
{"placements": ["application"], "settings_provider": "amaterasu.rtx-hdr", "default_expanded": true, "methods": []}
```

This advertises `ui.application-settings/1` and supplies `sdk.settings`, with
`provider`, `canWrite`, `read()`, and `update(expectedRevision, values)`. Requests
are pinned to that provider and the mounted application's scope. They use the
existing authenticated settings endpoints, configuration permissions, native
value validation, and revision checks. The declaration is an association for
presentation, not an authorization grant. `sdk.call` retains its separate signed
method declarations and approvals.

The plugin owns its controls, layout, inheritance behavior, and choice of manual
or automatic saving. `read()` returns the existing settings document containing
`revision`, sparse `values`, and `effective` values. `update()` saves sparse values;
omitting a key restores inheritance. It does not itself prove live application.
The RTX HDR plugin's `ui/application.js` uses this helper and serializes debounced
slider saves while preserving edits made during a pending request.

Its application panel starts expanded and shows one enable switch selected by
the effective host `rtx_hdr_scope`, plus the four picture controls. Debanding,
SDR fallback brightness, and fullscreen/foreground restrictions live under a
closed **Advanced** section. Untouched controls display effective global values
without separate inheritance switches; editing a field creates an application
override while preserving all other sparse values, including an inactive mode's
saved enable setting. Generated application settings use the same edit-to-override
behavior.

Saving during a stream uses the same settings contract as saving while idle.
For options declared `live_action`, the active consumer reads the latest scoped
values through the Module SDK's `read_live_provider_settings` contract and
applies them through the selected provider's runtime API. The consumer retains
the stream's original values for options that require the next session. A UI
section alone does not make a provider's settings live: the provider must declare
the live options and its consumer must implement their runtime application.

For RTX HDR, the plugin debounces edits by 200 ms and the video consumer polls
live settings once per second. Tuning changes therefore apply during an existing
RTX HDR stream after saving and the next poll, without restarting the stream.
The enable toggle also applies live when the stream has prepared the compatible
HDR processing path, including a stream started with conversion disabled. SDR
streams keep their negotiated output range. A successful save confirms
persistence; it is not an acknowledgement that a rendered frame used the value.

The host places plugin-owned cards in the dedicated **Plugin settings** area and
suppresses the generic form for a provider with a selected custom UI. Providers
without custom UI retain the generated settings card as a fallback. Plugins
without settings can attach sections by omitting `settings_provider`.

Application sections are collapsible. Set the optional Boolean
`default_expanded` in the signed UI configuration to show the section's controls
immediately, as RTX HDR does. Omit it or set it to `false` to start closed.
Users can still open or close each section; opening does not create overrides,
save values, or grant API access. Custom controls mount once even while closed,
so collapsing a section preserves its draft and does not cancel a pending save.

Providers without a custom web UI can set the same `default_expanded` field on
their Module SDK `extension_api::ExtensionDescriptor` to control their generated
application settings section. The native descriptor and signed UI configuration
are independent: declare the hint in both if you distribute both surfaces.
An absent native field deserializes as `false` and is omitted when serialized,
preserving existing descriptor wire representations. The hint does not change
the settings schema revision. Older hosts need an update before accepting a
manifest or descriptor containing the new field.

The [application section example](../examples/web-extension/application.js)
uses the existing greeting provider and declares no settings overrides. Package
it with [application-ui.json](../examples/web-extension/application-ui.json)
using the same approved publisher and method-authorization workflow as a page.

## Overview, settings, and dashboard widgets

Plugins can attach `mountOverview(container, sdk)` and
`mountSettings(container, sdk)` callbacks by declaring `overview` and `settings`
placements in the signed configuration. A plugin can select any nonempty subset
of `page`, `application`, `overview`, and `settings`. The shell provides generic
surfaces and owns no product-specific renderer. Each mount gets an independent
lifecycle; closing one surface does not retire another. Page and slot mounts
support `setDirty(boolean)` and `setBusy(boolean)` for navigation guards.

`sdk.canCall(alias)` reports whether the signed declaration and local approvals
make that alias available to the current account. It does not grant authority.
Calls are bound to the current signed digest and cancelled with the mount signal.
`sdk.assets.stylesheet(relativePath)` loads CSS only from that same selected,
verified bundle, and removes it when the surface closes. Styles should be scoped
to the plugin's own root element.

The reusable `ui.charts/1` capability exposes:

```js
const gauge = sdk.ui.metricGauge({label: 'Load', value: 25, detail: 'Measured'});
const chart = sdk.ui.metricChart({
  label: 'Measured rate', unit: 'units/s', minimum: 0,
  series: [{id: 'primary', label: 'Primary', points: [
    {time: 1700000000000, value: 1},
    {time: 1700000001000, value: null},
    {time: 1700000002000, value: 3},
  ]}],
});
container.append(gauge.element, chart.element);
gauge.setValue(30, 'New measurement');
chart.setData(nextSeries); // preserves the mounted control and zoom
return () => { gauge.dispose(); chart.dispose(); };
```

Gauges accept an optional positive `maximum` (default 100) and `unit` (default
`%`). Null is unavailable, distinct from zero. Charts accept at most eight unique
series, each with at most 2,000 ordered finite timestamps and finite or null
values. Null values break lines. Optional finite minimum and maximum set the
scale; the minimum must be less than the maximum. Charts provide zoom, series
visibility, and keyboard/pointer inspection. Data updates validate completely
before replacing the existing data. Owners dispose controls they remove before
the mount closes; the lifecycle signal also retires them. The SDK does not
collect data, infer missing values, or equate a stored sample with remote delivery.

Public TypeScript declarations are published in
[`sdk/web/amaterasu-ui.d.ts`](../sdk/web/amaterasu-ui.d.ts). They are generated
from the SDK interfaces by `npm run export-sdk` in `frontend`; importing them
does not import the shell. The independently built
[Stats plugin](../modules/stats/README.md) is a full consumer example.

## Module-owned settings surfaces

`settings_provider` supports both `settings` and `application` placements. A
mounted surface receives `ui.settings/1`, `sdk.settings.scope` and
`sdk.settings.describe()`. The descriptor supplies the selected provider's option
schemas, platform choices, labels, bounds, allowed scopes and application timing.
Discovery is bounded and checks pagination revisions. Host mounts bind
`{kind:"host"}`; app mounts bind that app's ID. Scripts cannot change this binding.
The older `ui.application-settings/1` feature remains available on app mounts.

A module that stores settings through its own approved operation API can declare
`settings_owner` instead of `settings_provider`:

```json
{"placements":["settings","application"],"settings_owner":"amaterasu.display","methods":[]}
```

This associates legacy controls with the selected custom UI; it grants no API
or registry access. Declare and approve actual methods separately. Without a
selected custom surface, legacy controls and registry-generated forms remain
available. Display uses revisioned `nonary.host-settings/1` and
`nonary.catalog/1` methods to preserve existing storage. Frame limiting uses the
selected native provider's registry descriptor through `ui.settings/1`.

After a plugin independently updates an application document and confirms its
readback, call `sdk.application.notifyChanged?.()`. This tells the open app editor
to re-read before saving its own draft. The editor merges non-overlapping changes
and rejects conflicting edits. The notification grants no authority and conveys
no trusted document values or revision. Closing the surface retires notifications,
queued UI work and the lifecycle signal.

Saved Display and Frame limiting settings apply to a new application session.
Their UI does not restart a stream or modify an existing display/limiter lease.
The RTX live-action contract described above remains separate.
