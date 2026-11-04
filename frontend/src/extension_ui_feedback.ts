namespace Amaterasu {
  export interface ExtensionUiFeedbackStatusOptions {
    message: ExtensionUiText;
    busy?: boolean;
  }

  export interface ExtensionUiFeedbackAlertOptions {
    message: ExtensionUiText;
    severity?: "info" | "success" | "warning" | "error";
  }

  export interface ExtensionUiFeedbackProgressOptions {
    label: ExtensionUiText;
    value?: number | null;
    maximum?: number;
    message?: ExtensionUiText;
  }

  export interface ExtensionUiFeedbackHandle {
    readonly element: HTMLElement;
    dispose(): void;
  }

  export interface ExtensionUiFeedbackStatus extends ExtensionUiFeedbackHandle {
    setMessage(message: ExtensionUiText): void;
    setBusy(busy: boolean): void;
  }

  export interface ExtensionUiFeedbackAlert extends ExtensionUiFeedbackHandle {
    setMessage(message: ExtensionUiText): void;
    setSeverity(severity: "info" | "success" | "warning" | "error"): void;
  }

  export interface ExtensionUiFeedbackProgress extends ExtensionUiFeedbackHandle {
    value(): number | null;
    setValue(value: number | null): void;
    setMessage(message: ExtensionUiText | null): void;
  }

  export interface ExtensionUiFeedbackV1 {
    readonly version: 1;
    status(options: ExtensionUiFeedbackStatusOptions): ExtensionUiFeedbackStatus;
    alert(options: ExtensionUiFeedbackAlertOptions): ExtensionUiFeedbackAlert;
    progress(options: ExtensionUiFeedbackProgressOptions): ExtensionUiFeedbackProgress;
  }

  export type ExtensionUiWithFeedbackV1 = ExtensionUiV1 & ExtensionUiFeedbackV1;

  let nextExtensionUiFeedbackId = 0;

  function feedbackTextNode(value: ExtensionUiText): Text | string {
    if (typeof value === "string") return localizedTextNode(value);
    if (!value || typeof value.source !== "string" || !value.source.trim()) throw new Error("Invalid plugin UI text.");
    const translations = value.translations ?? {};
    if (!translations || typeof translations !== "object" || Array.isArray(translations) ||
        Object.values(translations).some((translation) => typeof translation !== "string" || !translation.trim()))
      throw new Error("Invalid plugin UI translations.");
    return localeFormattedTextNode((locale) => translations[locale] ?? translations[locale.split("-")[0] ?? locale] ?? translateText(value.source));
  }

  function validateFeedbackMessage(message: ExtensionUiText): void {
    if (typeof message === "string") return;
    if (!message || typeof message !== "object" || typeof message.source !== "string" || !message.source.trim())
      throw new Error("Invalid plugin UI text.");
    const translations = message.translations ?? {};
    if (!translations || typeof translations !== "object" || Array.isArray(translations) ||
        Object.values(translations).some((translation) => typeof translation !== "string" || !translation.trim()))
      throw new Error("Invalid plugin UI translations.");
  }

  function feedbackHandle<T extends ExtensionUiFeedbackHandle>(
    element: HTMLElement,
    signal: AbortSignal,
    methods: Omit<T, "element" | "dispose">,
  ): T {
    let disposed = false;
    const dispose = (): void => {
      if (disposed) return;
      disposed = true;
      signal.removeEventListener("abort", dispose);
    };
    if (signal.aborted) dispose();
    else signal.addEventListener("abort", dispose, { once: true });
    return Object.freeze({ element, ...methods, dispose }) as T;
  }

  function feedbackLifecycle(signal: AbortSignal): void {
    if (!signal || typeof signal.addEventListener !== "function" || typeof signal.removeEventListener !== "function")
      throw new Error("Plugin UI requires a lifecycle signal.");
    if (signal.aborted) throw new Error("Plugin page is no longer active.");
  }

  function feedbackSetMessage(element: HTMLElement, message: ExtensionUiText): void {
    validateFeedbackMessage(message);
    element.replaceChildren(feedbackTextNode(message));
  }

  export function createExtensionUiFeedbackV1(signal: AbortSignal): ExtensionUiFeedbackV1 {
    feedbackLifecycle(signal);
    const makeElement = (className: string): HTMLElement => el("div", `plugin-feedback ${className}`);
    const feedback: ExtensionUiFeedbackV1 = {
      version: 1 as const,
      status(options) {
        feedbackLifecycle(signal);
        if (!options || typeof options !== "object" || Array.isArray(options)) throw new Error("Invalid plugin status feedback.");
        validateFeedbackMessage(options.message);
        if (options.busy !== undefined && typeof options.busy !== "boolean") throw new Error("Invalid plugin status busy state.");
        const element = makeElement("plugin-feedback-status");
        element.setAttribute("role", "status");
        element.setAttribute("aria-live", "polite");
        element.setAttribute("aria-atomic", "true");
        element.setAttribute("aria-busy", String(options.busy === true));
        feedbackSetMessage(element, options.message);
        let disposed = false;
        const handle = feedbackHandle<ExtensionUiFeedbackStatus>(element, signal, {
          setMessage(message) { if (!disposed && !signal.aborted) feedbackSetMessage(element, message); },
          setBusy(busy) {
            if (typeof busy !== "boolean") throw new Error("Invalid plugin status busy state.");
            if (!disposed && !signal.aborted) element.setAttribute("aria-busy", String(busy));
          },
        });
        const originalDispose = handle.dispose;
        return Object.freeze({ ...handle, dispose() { disposed = true; originalDispose(); } });
      },
      alert(options) {
        feedbackLifecycle(signal);
        if (!options || typeof options !== "object" || Array.isArray(options)) throw new Error("Invalid plugin alert feedback.");
        validateFeedbackMessage(options.message);
        const severity = options.severity === undefined ? "info" : options.severity;
        if (!["info", "success", "warning", "error"].includes(severity)) throw new Error("Invalid plugin alert severity.");
        const element = makeElement("plugin-feedback-alert");
        element.setAttribute("role", "alert");
        element.setAttribute("aria-live", "assertive");
        element.setAttribute("aria-atomic", "true");
        element.setAttribute("data-severity", severity);
        feedbackSetMessage(element, options.message);
        let disposed = false;
        const handle = feedbackHandle<ExtensionUiFeedbackAlert>(element, signal, {
          setMessage(message) { if (!disposed && !signal.aborted) feedbackSetMessage(element, message); },
          setSeverity(nextSeverity) {
            if (!["info", "success", "warning", "error"].includes(nextSeverity)) throw new Error("Invalid plugin alert severity.");
            if (!disposed && !signal.aborted) element.setAttribute("data-severity", nextSeverity);
          },
        });
        const originalDispose = handle.dispose;
        return Object.freeze({ ...handle, dispose() { disposed = true; originalDispose(); } });
      },
      progress(options) {
        feedbackLifecycle(signal);
        if (!options || typeof options !== "object" || Array.isArray(options)) throw new Error("Invalid plugin progress feedback.");
        validateFeedbackMessage(options.label);
        if (options.message !== undefined) validateFeedbackMessage(options.message);
        const maximum = options.maximum === undefined ? 100 : options.maximum;
        if (typeof maximum !== "number" || !Number.isFinite(maximum) || maximum <= 0)
          throw new Error("Invalid plugin progress maximum.");
        const initialValue = options.value === undefined ? null : options.value;
        if (initialValue !== null && (typeof initialValue !== "number" || !Number.isFinite(initialValue)))
          throw new Error("Invalid plugin progress value.");
        const clampedValue = initialValue === null ? null : Math.min(maximum, Math.max(0, initialValue));
        const element = makeElement("plugin-feedback-progress");
        const label = el("label", "plugin-feedback-label", feedbackTextNode(options.label));
        const progress = el("progress", "plugin-feedback-progressbar");
        const progressId = `amaterasu-plugin-feedback-progress-${++nextExtensionUiFeedbackId}`;
        progress.id = progressId;
        label.htmlFor = progressId;
        progress.max = maximum;
        progress.setAttribute("max", String(maximum));
        const syncValue = (value: number | null): void => {
          if (value === null) {
            progress.removeAttribute("value");
          } else {
            progress.value = value;
            progress.setAttribute("value", String(value));
          }
        };
        syncValue(clampedValue);
        const content: HTMLElement[] = [label, progress];
        let messageNode: HTMLElement | null = options.message === undefined ? null : el("small", "plugin-feedback-message", feedbackTextNode(options.message));
        if (messageNode) {
          messageNode.id = `amaterasu-plugin-feedback-description-${nextExtensionUiFeedbackId}`;
          progress.setAttribute("aria-describedby", messageNode.id);
          content.push(messageNode);
        }
        element.replaceChildren(...content);
        let currentValue = clampedValue;
        let disposed = false;
        const handle = feedbackHandle<ExtensionUiFeedbackProgress>(element, signal, {
          value: () => currentValue,
          setValue(value) {
            if (value !== null && (typeof value !== "number" || !Number.isFinite(value))) throw new Error("Invalid plugin progress value.");
            if (disposed || signal.aborted) return;
            currentValue = value === null ? null : Math.min(maximum, Math.max(0, value));
            syncValue(currentValue);
          },
          setMessage(message) {
            if (message !== null) validateFeedbackMessage(message);
            if (disposed || signal.aborted) return;
            if (message === null) {
              messageNode?.remove();
              messageNode = null;
              progress.removeAttribute("aria-describedby");
            } else if (messageNode) {
              messageNode.replaceChildren(feedbackTextNode(message));
            } else {
              messageNode = el("small", "plugin-feedback-message", feedbackTextNode(message));
              messageNode.id = `amaterasu-plugin-feedback-description-${nextExtensionUiFeedbackId}`;
              progress.setAttribute("aria-describedby", messageNode.id);
              element.append(messageNode);
            }
          },
        });
        const originalDispose = handle.dispose;
        return Object.freeze({ ...handle, dispose() { disposed = true; originalDispose(); } });
      },
    };
    return Object.freeze(feedback);
  }

  export function createExtensionUiWithFeedbackV1(signal: AbortSignal): ExtensionUiWithFeedbackV1 {
    const ui = createExtensionUiV1(signal);
    const feedback = createExtensionUiFeedbackV1(signal);
    return Object.freeze({ ...ui, ...feedback, version: 1 as const });
  }
}
