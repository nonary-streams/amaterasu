namespace Amaterasu {
  export type ExtensionUiDialogCloseReason = "cancel" | "close" | "dispose";

  export interface ExtensionUiDialogOptions {
    title: ExtensionUiText;
    description?: ExtensionUiText;
    content?: string | Node | readonly (string | Node)[];
    initialFocus?: HTMLElement;
    cancelLabel?: ExtensionUiText;
    onClose?: (reason: ExtensionUiDialogCloseReason) => void;
  }

  export interface ExtensionUiDialog {
    readonly element: HTMLDialogElement;
    open(): void;
    close(): void;
    dispose(): void;
  }

  export interface ExtensionUiDialogsV1 {
    dialog(options: ExtensionUiDialogOptions): ExtensionUiDialog;
  }

  export type ExtensionUiWithDialogsV1 = ExtensionUiWithFeedbackV1 & ExtensionUiDialogsV1;

  let nextExtensionUiDialogId = 0;
  const extensionUiDialogCloseFromParent = new WeakMap<HTMLDialogElement, () => void>();

  function extensionUiDialogTextNode(value: ExtensionUiText): Text | string {
    if (typeof value === "string") {
      if (!value.trim()) throw new Error("Invalid plugin dialog text.");
      return localizedTextNode(value);
    }
    if (!value || typeof value !== "object" || typeof value.source !== "string" || !value.source.trim())
      throw new Error("Invalid plugin dialog text.");
    const translations = value.translations ?? {};
    if (!translations || typeof translations !== "object" || Array.isArray(translations) ||
        Object.values(translations).some((translation) => typeof translation !== "string" || !translation.trim()))
      throw new Error("Invalid plugin dialog translations.");
    return localeFormattedTextNode((locale) =>
      translations[locale] ?? translations[locale.split("-")[0] ?? locale] ?? translateText(value.source));
  }

  function extensionUiDialogContent(content: ExtensionUiDialogOptions["content"]): Node[] {
    if (content === undefined) return [];
    const values = Array.isArray(content) ? [...content] : [content];
    return values.map((value) => {
      if (typeof value === "string") return document.createTextNode(value);
      if (!value || typeof value !== "object" || value.ownerDocument !== document ||
          ![1, 3, 11].includes(value.nodeType))
        throw new Error("Dialog content must be text or a node owned by this document.");
      return value;
    });
  }

  export function createExtensionUiDialogsV1(signal: AbortSignal): ExtensionUiDialogsV1 {
    if (!signal || typeof signal.addEventListener !== "function" || typeof signal.removeEventListener !== "function")
      throw new Error("Plugin UI requires a lifecycle signal.");
    if (signal.aborted) throw new Error("Plugin page is no longer active.");

    const dialogs: ExtensionUiDialogsV1 = {
      dialog(options) {
        if (signal.aborted) throw new Error("Plugin page is no longer active.");
        if (!options || typeof options !== "object" || Array.isArray(options))
          throw new Error("Invalid plugin dialog.");
        if (options.onClose !== undefined && typeof options.onClose !== "function")
          throw new Error("Invalid plugin dialog close callback.");
        const titleNode = extensionUiDialogTextNode(options.title);
        const descriptionNode = options.description === undefined
          ? null : extensionUiDialogTextNode(options.description);
        const cancelLabel = extensionUiDialogTextNode(options.cancelLabel ?? "Cancel");
        const contentNodes = extensionUiDialogContent(options.content);
        const initialFocus = options.initialFocus;
        if (initialFocus !== undefined && (!initialFocus || typeof initialFocus.focus !== "function" ||
            initialFocus.ownerDocument !== document || !contentNodes.some((node) => node === initialFocus ||
              ((node.nodeType === 1 || node.nodeType === 11) && node.contains(initialFocus)))))
          throw new Error("Dialog initial focus must be inside its content.");

        const instanceId = ++nextExtensionUiDialogId;
        const titleId = `amaterasu-plugin-dialog-title-${instanceId}`;
        const descriptionId = `amaterasu-plugin-dialog-description-${instanceId}`;
        const element = document.createElement("dialog");
        element.className = "plugin-dialog";
        element.setAttribute("aria-modal", "true");
        element.setAttribute("aria-labelledby", titleId);
        const title = document.createElement("h2");
        title.id = titleId;
        title.append(titleNode);
        element.append(title);
        if (descriptionNode !== null) {
          const description = document.createElement("p");
          description.className = "plugin-dialog-description";
          description.id = descriptionId;
          description.append(descriptionNode);
          element.setAttribute("aria-describedby", descriptionId);
          element.append(description);
        }
        const body = document.createElement("div");
        body.className = "plugin-dialog-content";
        body.append(...contentNodes);
        element.append(body);
        const actions = document.createElement("div");
        actions.className = "actions plugin-dialog-actions";
        const cancelButton = document.createElement("button");
        cancelButton.type = "button";
        cancelButton.className = "button secondary";
        cancelButton.append(cancelLabel);
        actions.append(cancelButton);
        element.append(actions);

        const controller = new AbortController();
        const nativeClose = element.close.bind(element);
        let disposed = false;
        let opener: HTMLElement | null = null;
        let pendingCloseReason: ExtensionUiDialogCloseReason | null = null;
        let closeEventPending = false;
        let suppressFocusReturn = false;
        const restoreFocus = (): void => {
          const target = opener;
          opener = null;
          if (!signal.aborted && target?.isConnected) target.focus();
        };
        const closeNestedDialogs = (): void => {
          const nestedDialogs = Array.from(element.querySelectorAll<HTMLDialogElement>("dialog")).reverse();
          for (const nested of nestedDialogs) {
            const closeOwned = extensionUiDialogCloseFromParent.get(nested);
            if (closeOwned) closeOwned();
            else if (nested.open) nested.close();
          }
        };
        const closeDialog = (reason: ExtensionUiDialogCloseReason, returnValue?: string): void => {
          if (disposed || signal.aborted || !element.open) return;
          pendingCloseReason = reason;
          closeEventPending = true;
          element.close(returnValue ?? reason);
        };
        const closeFromParent = (): void => {
          if (disposed || signal.aborted) return;
          if (element.open) {
            suppressFocusReturn = true;
            closeDialog("close");
          } else if (closeEventPending) {
            suppressFocusReturn = true;
          }
        };
        const onCancel = (event: Event): void => {
          event.preventDefault();
          closeDialog("cancel");
        };
        const onKeyDown = (event: KeyboardEvent): void => {
          if (event.key !== "Tab" || disposed || signal.aborted) return;
          const focusable = Array.from(element.querySelectorAll<HTMLElement>(
            "a[href],area[href],button:not(:disabled),input:not(:disabled):not([type=hidden]),select:not(:disabled),textarea:not(:disabled),iframe,[tabindex]:not([tabindex='-1']),[contenteditable='true']",
          )).filter((control) => !control.hidden && control.getAttribute("aria-hidden") !== "true" && control.tabIndex >= 0 && control.getClientRects().length > 0);
          const first = focusable[0];
          const last = focusable.at(-1);
          const active = document.activeElement;
          if (!first || !last) {
            event.preventDefault();
            cancelButton.focus();
          } else if (event.shiftKey && (active === first || !element.contains(active))) {
            event.preventDefault();
            last.focus();
          } else if (!event.shiftKey && (active === last || !element.contains(active))) {
            event.preventDefault();
            first.focus();
          }
        };
        const onClose = (): void => {
          const reason = pendingCloseReason ?? (element.returnValue === "cancel" ? "cancel" : "close");
          pendingCloseReason = null;
          closeEventPending = false;
          const shouldRestoreFocus = !suppressFocusReturn;
          suppressFocusReturn = false;
          if (disposed || signal.aborted) return;
          if (shouldRestoreFocus) restoreFocus();
          else opener = null;
          options.onClose?.(reason);
        };
        const onDialogFormSubmit = (event: SubmitEvent): void => {
          const form = event.target as HTMLFormElement | null;
          if (form?.tagName !== "FORM" || form.closest("dialog") !== element) return;
          const submitter = event.submitter as HTMLButtonElement | HTMLInputElement | null;
          const method = submitter?.hasAttribute("formmethod")
            ? submitter.formMethod.toLowerCase() : form.method.toLowerCase();
          if (method !== "dialog") return;
          pendingCloseReason = "close";
          closeEventPending = true;
          queueMicrotask(() => {
            if (disposed || signal.aborted) return;
            if (element.open) {
              pendingCloseReason = null;
              closeEventPending = false;
            } else {
              closeNestedDialogs();
            }
          });
        };
        const onHostAbort = (): void => {
          if (disposed) return;
          disposed = true;
          try {
            if (element.open) element.close("dispose");
          } finally {
            opener = null;
            controller.abort();
            element.remove();
          }
        };
        element.close = ((returnValue?: string): void => {
          if (element.open) closeNestedDialogs();
          if (element.open && !disposed && !signal.aborted) {
            closeEventPending = true;
            if (pendingCloseReason === null)
              pendingCloseReason = returnValue === "cancel" ? "cancel" : "close";
          }
          nativeClose(returnValue);
        }) as typeof element.close;
        extensionUiDialogCloseFromParent.set(element, closeFromParent);
        cancelButton.addEventListener("click", () => closeDialog("cancel"), { signal: controller.signal });
        element.addEventListener("cancel", onCancel, { signal: controller.signal });
        element.addEventListener("keydown", onKeyDown, { signal: controller.signal });
        element.addEventListener("submit", onDialogFormSubmit, { capture: true, signal: controller.signal });
        element.addEventListener("close", onClose, { signal: controller.signal });
        signal.addEventListener("abort", onHostAbort, { once: true });

        const handle: ExtensionUiDialog = {
          element,
          open() {
            if (disposed || signal.aborted || element.open || closeEventPending) return;
            if (!element.isConnected) throw new Error("Append the dialog element before opening it.");
            const active = document.activeElement as HTMLElement | null;
            opener = active && typeof active.focus === "function" ? active : null;
            try {
              element.showModal();
            } catch (error) {
              opener = null;
              throw error;
            }
            (initialFocus ?? cancelButton).focus();
          },
          close() { closeDialog("close"); },
          dispose() {
            if (disposed) return;
            const wasOpen = element.open;
            const pendingReason = closeEventPending ? pendingCloseReason : null;
            disposed = true;
            try {
              if (wasOpen) element.close("dispose");
              if (!signal.aborted && (wasOpen || closeEventPending)) {
                restoreFocus();
                options.onClose?.(wasOpen ? "dispose" : pendingReason ?? "dispose");
              }
            } finally {
              pendingCloseReason = null;
              closeEventPending = false;
              opener = null;
              controller.abort();
              signal.removeEventListener("abort", onHostAbort);
              extensionUiDialogCloseFromParent.delete(element);
              element.remove();
            }
          },
        };
        return Object.freeze(handle);
      },
    };
    return Object.freeze(dialogs);
  }

  export function createExtensionUiWithDialogsV1(signal: AbortSignal): ExtensionUiWithDialogsV1 {
    const ui = createExtensionUiWithFeedbackV1(signal);
    const dialogs = createExtensionUiDialogsV1(signal);
    return Object.freeze({ ...ui, ...dialogs, version: 1 as const });
  }
}
