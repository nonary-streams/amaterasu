namespace Amaterasu {
  export type ExtensionUiText = string | {
    source: string;
    translations?: Readonly<Record<string, string>>;
  };

  export interface ExtensionUiControl<T> {
    readonly element: HTMLElement;
    value(): T;
    setValue(value: T): void;
    dispose(): void;
  }

  export interface ExtensionUiOption {
    value: string;
    label: ExtensionUiText;
  }

  export interface ExtensionUiV1 {
    readonly version: 1;
    toggle(options: { label: ExtensionUiText; value: boolean; onChange?: (value: boolean) => void }): ExtensionUiControl<boolean>;
    select(options: { label: ExtensionUiText; choices: readonly ExtensionUiOption[]; value: string; onChange?: (value: string) => void }): ExtensionUiControl<string>;
    radioGroup(options: { label: ExtensionUiText; choices: readonly ExtensionUiOption[]; value: string; onChange?: (value: string) => void }): ExtensionUiControl<string>;
    slider(options: { label: ExtensionUiText; minimum: number; maximum: number; value: number; step?: number; onChange?: (value: number) => void }): ExtensionUiControl<number>;
  }

  function extensionUiTextNode(value: ExtensionUiText): Text | string {
    if (typeof value === "string") return localizedTextNode(value);
    if (!value || typeof value.source !== "string" || !value.source.trim()) throw new Error("Invalid plugin UI text.");
    const translations = value.translations ?? {};
    if (Object.values(translations).some((translation) => typeof translation !== "string" || !translation.trim()))
      throw new Error("Invalid plugin UI translations.");
    return localeFormattedTextNode((locale) => translations[locale] ?? translations[locale.split("-")[0] ?? locale] ?? translateText(value.source));
  }

  function extensionUiLabel(text: ExtensionUiText): HTMLElement {
    return el("span", "field-label", extensionUiTextNode(text));
  }

  function extensionUiChoices(choices: readonly ExtensionUiOption[], selected: string): void {
    if (!Array.isArray(choices) || choices.length < 2 || choices.length > 64 ||
        choices.some((choice) => !choice || typeof choice.value !== "string" || !choice.value || choice.value.length > 128) ||
        new Set(choices.map((choice) => choice.value)).size !== choices.length ||
        !choices.some((choice) => choice.value === selected)) throw new Error("Invalid plugin UI choices or value.");
  }

  function extensionUiControl<T>(element: HTMLElement, read: () => T, write: (value: T) => void, controller: AbortController, parentSignal: AbortSignal, release: () => void = () => {}): ExtensionUiControl<T> {
    let disposed = false;
    const dispose = (): void => {
      if (disposed) return;
      disposed = true;
      controller.abort();
      release();
      parentSignal.removeEventListener("abort", dispose);
    };
    if (parentSignal.aborted) dispose();
    else parentSignal.addEventListener("abort", dispose, { once: true });
    return Object.freeze({
      element,
      value: read,
      setValue: (value: T) => { if (!disposed) write(value); },
      dispose,
    });
  }

  export function createExtensionUiV1(signal: AbortSignal): ExtensionUiV1 {
    if (!signal || typeof signal.addEventListener !== "function") throw new Error("Plugin UI requires a lifecycle signal.");
    const makeController = (): AbortController => {
      if (signal.aborted) throw new Error("Plugin page is no longer active.");
      return new AbortController();
    };

    const ui: ExtensionUiV1 = {
      version: 1 as const,
      toggle(options) {
        if (!options || typeof options.label !== "string" && typeof options.label !== "object" || typeof options.value !== "boolean")
          throw new Error("Invalid plugin toggle.");
        const controller = makeController();
        const checkbox = input("", "", "checkbox");
        checkbox.checked = options.value;
        const element = el("label", "field plugin-control plugin-toggle", extensionUiLabel(options.label), checkbox);
        checkbox.addEventListener("change", () => options.onChange?.(checkbox.checked), { signal: controller.signal });
        return extensionUiControl(element, () => checkbox.checked, (value) => {
          if (typeof value !== "boolean") throw new Error("Toggle value must be boolean.");
          checkbox.checked = value;
        }, controller, signal);
      },
      select(options) {
        if (!options || typeof options.label !== "string" && typeof options.label !== "object") throw new Error("Invalid plugin select.");
        extensionUiChoices(options.choices, options.value);
        const controller = makeController();
        const select = el("select", "");
        for (const choice of options.choices) {
          const option = el("option", "", extensionUiTextNode(choice.label));
          option.value = choice.value;
          select.append(option);
        }
        select.value = options.value;
        const element = el("label", "field plugin-control", extensionUiLabel(options.label), select);
        select.addEventListener("change", () => options.onChange?.(select.value), { signal: controller.signal });
        return extensionUiControl(element, () => select.value, (value) => {
          if (!options.choices.some((choice) => choice.value === value)) throw new Error("Unsupported select value.");
          select.value = value;
        }, controller, signal);
      },
      radioGroup(options) {
        if (!options || typeof options.label !== "string" && typeof options.label !== "object") throw new Error("Invalid plugin radio group.");
        extensionUiChoices(options.choices, options.value);
        const controller = makeController();
        const group = Amaterasu.radioGroup(
          extensionUiTextNode(options.label),
          options.choices.map((choice) => ({ value: choice.value, label: extensionUiTextNode(choice.label) })),
          options.value,
          options.onChange,
          controller.signal,
        );
        return extensionUiControl(group.node, group.value, group.setValue, controller, signal, group.dispose);
      },
      slider(options) {
        if (!options || typeof options.label !== "string" && typeof options.label !== "object") throw new Error("Invalid plugin slider.");
        const controller = makeController();
        const label = extensionUiLabel(options.label);
        const slider = Amaterasu.boundedSlider(
          options.minimum,
          options.maximum,
          options.value,
          typeof options.label === "string" ? options.label : options.label.source,
          false,
          "plugin-slider-inputs",
          { step: options.step ?? "any", onChange: options.onChange, signal: controller.signal, accessibleLabel: label },
        );
        const element = el("div", "field plugin-control plugin-slider", label, slider.node);
        return extensionUiControl(element, slider.value, slider.setValue, controller, signal, slider.dispose);
      },
    };
    return Object.freeze(ui);
  }

}
