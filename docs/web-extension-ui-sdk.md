# Web extension UI controls

Browser page extensions execute as trusted same-origin JavaScript. The UI SDK
provides reusable, accessible controls; it does not sandbox a publisher or grant
additional API methods. The signed extension manifest and host authorization
remain the only authority for `sdk.call`.

Hosts that implement the control contract advertise the `ui.controls/1` feature
in the additive `sdk.features` list and supply `sdk.ui` with
`sdk.ui.version === 1`. These fields are optional in the SDK v1 type so a page
can detect older hosts before using the controls:

```js
if (!sdk.features?.includes("ui.controls/1") || sdk.ui?.version !== 1) {
  // Render a compatible fallback or a clear unsupported-host message.
}
```

Existing SDK v1 pages continue to run on older hosts. The UI contract currently provides `toggle`, `select`, `radioGroup`, and `slider`.
Each factory returns a control with `element`, `value()`, `setValue(value)`, and
`dispose()`. Append `element` to the page container. Values are local UI state;
the extension remains responsible for validation, persistence, and any approved
service calls.

Labels and choice labels accept plain source text or `{ source, translations }`.
Translations are keyed by locale (for example `fr` or `fr-CA`); exact locale and
language fallback are tried before the host's translation catalog. Labels are
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
