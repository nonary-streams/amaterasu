# Web extension modal dialogs

`ui.dialog/1` adds a reusable modal dialog to the version 1 web extension UI.
It does not change `sdk.version` or `sdk.ui.version`, and it adds no service
methods or permissions. `sdk.call` remains limited to declared extension
methods and host authorization.

Check the feature before calling the factory:

```js
if (sdk.version !== 1 || sdk.ui?.version !== 1 ||
    !sdk.features?.includes("ui.dialog/1")) {
  // Render a fallback or an unsupported-host message.
  return;
}
```

## API

```ts
dialog({
  title,
  description?,
  content?: string | Node | readonly (string | Node)[],
  initialFocus?,
  cancelLabel?,
  onClose?,
}): {
  element: HTMLDialogElement;
  open(): void;
  close(): void;
  dispose(): void;
}
```

`title`, `description`, and `cancelLabel` accept plain strings or
`{ source, translations? }` localized text. The title is rendered as a heading
and associated with the native dialog through `aria-labelledby`. Optional
`description` is associated with `aria-describedby`.

Append `element` before calling `open()`. Opening uses native `showModal()`,
which gives the dialog modal semantics and keeps Tab navigation within it. The
cancel button receives focus by default. Pass `initialFocus` to focus a
plugin-owned element inside `content` instead. After a normal close or cancel,
focus returns to the connected element that was active before `open()`.

The cancel button and Escape both close with reason `cancel`; `close()` reports
`close`; manually disposing an open dialog reports `dispose`. Native
`element.close()` and `form method="dialog"` close events are handled too.
`onClose` receives one of those reasons. Forms using `method="dialog"` retain
native submit behavior and submitter return values; plugin submit listeners run
normally and can cancel submission. A submit button's `formmethod` override is
respected, and nested dialogs handle only their own forms. Closing a parent
closes any open nested dialogs without returning focus into the closing parent.
`open()` waits until a queued native close event has been processed before
reopening the same instance. A host
lifecycle abort closes and removes the dialog without running `onClose` or
restoring focus into a page that is being torn down. `dispose()` is idempotent
and removes the dialog and its listeners even if `onClose` throws; the callback
exception may propagate to the caller.

## Safe content and lifecycle

String content is inserted as text, never parsed as HTML. For richer content,
pass DOM nodes created by the plugin in the current document; a
`DocumentFragment` and its descendants are supported. The SDK rejects nodes
owned by a different document. `initialFocus` may point to a focusable element
inside a passed node or fragment. Use `textContent` for dynamic values and avoid
`innerHTML`.

Dialogs are bound to the extension's lifecycle `AbortSignal`. They close and
remove themselves on navigation or signout. Dispose sooner when removing a
dialog while the page remains active.

```js
const field = document.createElement("input");
field.type = "text";
field.setAttribute("aria-label", "Project name");
const dialog = sdk.ui.dialog({
  title: { source: "Rename project", translations: { fr: "Renommer le projet" } },
  description: "Enter a name for this project.",
  content: field,
  initialFocus: field,
  onClose(reason) { console.log("Dialog closed:", reason); },
});
container.append(dialog.element);
dialog.open();
return () => dialog.dispose();
```

See the feature-gated [consumer example](../examples/web-extension/dialog.js).
