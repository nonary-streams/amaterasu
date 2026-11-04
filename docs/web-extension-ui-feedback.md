# Web extension UI feedback

`ui.feedback/1` is an additive feature of the version 1 web extension SDK. It
does not change either `sdk.version === 1` or `sdk.ui.version === 1`. A host
advertises it in the additive `sdk.features` list. The feature is independent
of `ui.controls/1` and `ui.forms/*`.
This UI capability does not add service methods or permissions. `sdk.call`
remains limited to aliases declared by the signed extension manifest and
authorized by the host.

Check both versions and the feature before calling its factories; UI version 1
alone does not promise feedback support:

```js
if (sdk.version !== 1 || sdk.ui?.version !== 1 ||
    !sdk.features?.includes("ui.feedback/1")) {
  // Render a compatible fallback or an unsupported-host message.
  return;
}
```

The capability adds these methods to `sdk.ui`:

```ts
status(options: { message: LocalizedText; busy?: boolean }): {
  element: HTMLElement;
  setMessage(message: LocalizedText): void;
  setBusy(busy: boolean): void;
  dispose(): void;
};

alert(options: { message: LocalizedText; severity?: Severity }): {
  element: HTMLElement;
  setMessage(message: LocalizedText): void;
  setSeverity(severity: Severity): void;
  dispose(): void;
};

progress(options: {
  label: LocalizedText;
  value?: number | null;
  maximum?: number;
  message?: LocalizedText;
}): {
  element: HTMLElement;
  value(): number | null;
  setValue(value: number | null): void;
  setMessage(message: LocalizedText | null): void;
  dispose(): void;
};

type LocalizedText = string | {
  source: string;
  translations?: Record<string, string>;
};
type Severity = "info" | "success" | "warning" | "error";
```

Text and labels accept plain strings or localized text objects. Hosts insert
them as text, never interpret them as HTML. Translations are keyed by locale;
the host tries the exact locale and language fallback before its translation
catalog. Setters update the existing element in place.

## Status and alert semantics

A status is for non-urgent updates and uses a polite, atomic live region. Its
optional `busy` flag defaults to false and is reflected through the accessible
busy state. An alert is for important information that needs prompt attention
and uses assertive alert semantics. Reserve alerts for urgent, time-sensitive
messages that need immediate attention; ordinary success notices and progress
updates belong in a status region. Severity describes the alert's meaning and
maps to the host's theme styling.
The supported values are `info`, `success`, `warning`, and `error`; omitted
severity defaults to `info`.

## Progress semantics

Progress has an accessible name from `label`. Its `maximum` must be finite and
greater than zero, and defaults to 100. An omitted initial `value` or
`value: null` means indeterminate progress. Finite values are bounded to the
range from zero through `maximum`; values outside that range are clamped.
`value()` returns the current bounded number or `null`. Call `setValue(null)`
when the amount of work is unknown. Non-finite values are rejected without
changing progress state.

Optional `message` text explains progress without announcing every percentage
change; `setMessage(null)` clears it. The host exposes determinate values and
maximum to assistive technology, and omits the numeric value for indeterminate
progress.

## Updates and lifecycle

Setters update the returned control in place; they do not create another
feedback element. Programmatic updates are not user input. Append each
`element` to the page container. Controls are bound to the host's extension
lifecycle `AbortSignal`: on navigation or signout, abort automatically disposes
them and further updates are ignored. Call `dispose()` sooner when removing an
element before the page lifecycle ends. Disposal is idempotent.

```js
const status = sdk.ui.status({ message: "Preparing…", busy: true });
container.append(status.element);
return () => status.dispose();
```

See the feature-gated [consumer example](../examples/web-extension/feedback.js).

