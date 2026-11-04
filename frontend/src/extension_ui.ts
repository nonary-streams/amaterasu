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

  function extensionUiControl<T>(element: HTMLElement, read: () => T, write: (value: T) => void, controller: AbortController, parentSignal: AbortSignal): ExtensionUiControl<T> {
    let disposed = false;
    const dispose = (): void => {
      if (disposed) return;
      disposed = true;
      controller.abort();
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
        const name = `amaterasu-plugin-radio-${++nextPluginRadioGroupId}`;
        const group = el("fieldset", "field plugin-control plugin-radio-group");
        const legend = el("legend", "field-label", extensionUiTextNode(options.label));
        group.append(legend);
        const inputs: HTMLInputElement[] = [];
        for (const choice of options.choices) {
          const radio = input(name, choice.value, "radio");
          radio.checked = choice.value === options.value;
          radio.addEventListener("change", () => {
            if (radio.checked) options.onChange?.(radio.value);
          }, { signal: controller.signal });
          group.append(el("label", "plugin-radio-option", radio, el("span", "", extensionUiTextNode(choice.label))));
          inputs.push(radio);
        }
        return extensionUiControl(group,
          () => inputs.find((radio) => radio.checked)?.value ?? "",
          (value) => {
            if (!options.choices.some((choice) => choice.value === value)) throw new Error("Unsupported radio group value.");
            for (const radio of inputs) radio.checked = radio.value === value;
          }, controller, signal);
      },
      slider(options) {
        if (!options || typeof options.label !== "string" && typeof options.label !== "object") throw new Error("Invalid plugin slider.");
        const step = options.step ?? "any";
        if (!Number.isFinite(options.minimum) || !Number.isFinite(options.maximum) || options.minimum >= options.maximum ||
            !Number.isFinite(options.value) || options.value < options.minimum || options.value > options.maximum ||
            (step !== "any" && (!Number.isFinite(step) || step <= 0))) throw new Error("Invalid plugin slider bounds or value.");
        const controller = makeController();
        const range = input("", String(options.value), "range");
        const number = input("", String(options.value), "number");
        for (const control of [range, number]) {
          control.min = String(options.minimum);
          control.max = String(options.maximum);
          control.step = String(step);
        }
        const label = extensionUiLabel(options.label);
        label.id = `amaterasu-plugin-slider-${++nextPluginControlId}-label`;
        range.setAttribute("aria-labelledby", label.id);
        number.setAttribute("aria-labelledby", label.id);
        const valueOf = (): number => {
          const value = Number(number.value);
          if (!number.value || !number.validity.valid || !Number.isFinite(value) || value < options.minimum || value > options.maximum)
            throw new Error(`Enter a valid number from ${options.minimum} to ${options.maximum}.`);
          return value;
        };
        range.addEventListener("input", () => {
          number.value = range.value;
          options.onChange?.(valueOf());
        }, { signal: controller.signal });
        number.addEventListener("input", () => {
          try {
            const value = valueOf();
            range.value = String(value);
            options.onChange?.(value);
          } catch { /* Keep incomplete edits visible until the user corrects them. */ }
        }, { signal: controller.signal });
        const element = el("div", "field plugin-control plugin-slider", label, el("div", "plugin-slider-inputs", range, number));
        return extensionUiControl(element, valueOf, (value) => {
          if (!Number.isFinite(value) || value < options.minimum || value > options.maximum) throw new Error("Unsupported slider value.");
          number.value = String(value);
          range.value = String(value);
        }, controller, signal);
      },
    };
    return Object.freeze(ui);
  }

  let nextPluginRadioGroupId = 0;
  let nextPluginControlId = 0;
}
