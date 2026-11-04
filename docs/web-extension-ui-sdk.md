# Web extension UI controls

Browser page extensions execute as trusted same-origin JavaScript. The UI SDK
provides reusable, accessible controls; it does not sandbox a publisher or grant
additional API methods. The signed extension manifest and host authorization
remain the only authority for `sdk.call`.

The additive status, alert, and progress components are documented in the
[feedback UI SDK guide](web-extension-ui-feedback.md); they use the separate
`ui.feedback/1` capability while keeping `sdk.ui.version` at 1.

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
