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
feature. Feature checks are optional in SDK v1 so pages can detect older hosts:

```js
if (!sdk.features?.includes("ui.controls/1") ||
    !sdk.features.includes("ui.forms/1") || sdk.ui?.version !== 1) {
  // Render a compatible fallback or a clear unsupported-host message.
}
```

Existing SDK v1 pages continue to run on older hosts. New pages that need only base controls can check `ui.controls/1`; pages that use form metadata or `setChoices()` must also check `ui.forms/1`; pages that call `setState()` must check `ui.forms/2`. An older host without `ui.forms/2` continues to provide the original controls and their creation-time options. The UI contract provides `toggle`, `select`, `radioGroup`, and `slider`.
Each factory accepts common `disabled`, `readOnly`, `required`, `help`, and
`error` options. `help` and `error` use the same localized text format as labels;
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
the control through `aria-describedby`. `setValue()` remains available for
programmatic updates.
Each factory returns a control with `element`, `value()`, `setValue(value)`,
`setState(patch)`, and `dispose()`. `setState()` applies a partial update: omitted
properties stay unchanged, boolean properties accept `true` or `false`, and pass
`null` for `help` or `error` to clear that description. Invalid patches fail before
changing the control. State updates modify the existing nodes and listeners, so
focus and control identity are preserved. Disabling the currently focused native
control lets the browser move focus according to its disabled-control behavior.
A page using `setState()` should require `ui.forms/2` before mounting:

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
in the new list, the update fails without changing the control. A radio group's
focused choice remains focused if it survives replacement; if it is removed, focus
moves to the selected choice. Programmatic choice updates do not call `onChange`.
Append `element` to the page container. Values are local UI state; the extension
remains responsible for validation, persistence, and any approved service calls.

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
