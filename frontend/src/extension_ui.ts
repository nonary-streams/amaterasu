namespace Amaterasu {
  export type ExtensionUiText = string | {
    source: string;
    translations?: Readonly<Record<string, string>>;
  };

  export interface ExtensionUiFieldOptions {
    disabled?: boolean;
    readOnly?: boolean;
    required?: boolean;
    help?: ExtensionUiText;
    error?: ExtensionUiText;
  }

  export interface ExtensionUiControl<T> {
    readonly element: HTMLElement;
    value(): T;
    setValue(value: T): void;
    dispose(): void;
  }

  export interface ExtensionUiChoiceControl extends ExtensionUiControl<string> {
    setChoices(choices: readonly ExtensionUiOption[], value?: string): void;
  }

  export interface ExtensionUiOption {
    value: string;
    label: ExtensionUiText;
  }

  export interface ExtensionUiV1 {
    readonly version: 1;
    toggle(options: ExtensionUiFieldOptions & { label: ExtensionUiText; value: boolean; onChange?: (value: boolean) => void }): ExtensionUiControl<boolean>;
    select(options: ExtensionUiFieldOptions & { label: ExtensionUiText; choices: readonly ExtensionUiOption[]; value: string; onChange?: (value: string) => void }): ExtensionUiChoiceControl;
    radioGroup(options: ExtensionUiFieldOptions & { label: ExtensionUiText; choices: readonly ExtensionUiOption[]; value: string; onChange?: (value: string) => void }): ExtensionUiChoiceControl;
    slider(options: ExtensionUiFieldOptions & { label: ExtensionUiText; minimum: number; maximum: number; value: number; step?: number; onChange?: (value: number) => void }): ExtensionUiControl<number>;
  }

  let nextExtensionUiDescriptionId = 0;

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
    if (!Array.isArray(choices) || choices.length < 1 || choices.length > 64 ||
        choices.some((choice) => !choice || typeof choice.value !== "string" || !choice.value || choice.value.length > 128) ||
        new Set(choices.map((choice) => choice.value)).size !== choices.length ||
        !choices.some((choice) => choice.value === selected)) throw new Error("Invalid plugin UI choices or value.");
  }

  function validateExtensionUiFieldOptions(options: ExtensionUiFieldOptions & { onChange?: unknown }): void {
    for (const name of ["disabled", "readOnly", "required"] as const) {
      const value = options[name];
      if (value !== undefined && typeof value !== "boolean") throw new Error(`Invalid plugin UI ${name} state.`);
    }
    if (options.onChange !== undefined && typeof options.onChange !== "function") throw new Error("Invalid plugin UI change callback.");
  }

  function extensionUiControl<T>(
    element: HTMLElement,
    read: () => T,
    write: (value: T) => void,
    controller: AbortController,
    parentSignal: AbortSignal,
    release?: () => void,
  ): ExtensionUiControl<T>;
  function extensionUiControl(
    element: HTMLElement,
    read: () => string,
    write: (value: string) => void,
    controller: AbortController,
    parentSignal: AbortSignal,
    release: () => void,
    updateChoices: (choices: readonly ExtensionUiOption[], value?: string) => void,
  ): ExtensionUiChoiceControl;
  function extensionUiControl<T>(
    element: HTMLElement,
    read: () => T,
    write: (value: T) => void,
    controller: AbortController,
    parentSignal: AbortSignal,
    release: () => void = () => {},
    updateChoices?: (choices: readonly ExtensionUiOption[], value?: string) => void,
  ): ExtensionUiControl<T> | ExtensionUiChoiceControl {
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
    const control: ExtensionUiControl<T> & { setChoices?: (choices: readonly ExtensionUiOption[], value?: string) => void } = {
      element,
      value: read,
      setValue: (value: T) => { if (!disposed) write(value); },
      dispose,
    };
    if (updateChoices) {
      control.setChoices = (choices, value) => { if (!disposed) updateChoices(choices, value); };
    }
    return Object.freeze(control);
  }

  function extensionUiFieldDescriptions(
    element: HTMLElement,
    controls: readonly HTMLElement[],
    options: ExtensionUiFieldOptions,
  ): (updatedControls: readonly HTMLElement[]) => void {
    const descriptionIds: string[] = [];
    if (options.help !== undefined) {
      const help = el("small", "plugin-control-help", extensionUiTextNode(options.help));
      help.id = `amaterasu-plugin-ui-description-${++nextExtensionUiDescriptionId}`;
      descriptionIds.push(help.id);
      element.append(help);
    }
    if (options.error !== undefined) {
      const error = el("small", "plugin-control-error", extensionUiTextNode(options.error));
      error.id = `amaterasu-plugin-ui-description-${++nextExtensionUiDescriptionId}`;
      error.setAttribute("role", "alert");
      descriptionIds.push(error.id);
      element.append(error);
    }
    const apply = (updatedControls: readonly HTMLElement[]): void => {
      for (const control of updatedControls) {
        if (descriptionIds.length) control.setAttribute("aria-describedby", descriptionIds.join(" "));
        if (options.error !== undefined) control.setAttribute("aria-invalid", "true");
        if (options.required) control.setAttribute("aria-required", "true");
      }
      if (element.getAttribute("role") === "radiogroup") {
        if (options.error !== undefined) element.setAttribute("aria-invalid", "true");
        if (options.required) element.setAttribute("aria-required", "true");
      }
    };
    apply(controls);
    return apply;
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
        validateExtensionUiFieldOptions(options);
        const controller = makeController();
        const checkbox = input("", "", "checkbox");
        checkbox.checked = options.value;
        checkbox.disabled = options.disabled === true;
        checkbox.required = options.required === true;
        if (options.readOnly) checkbox.setAttribute("aria-readonly", "true");
        let currentValue = options.value;
        checkbox.addEventListener("click", (event) => {
          if (options.readOnly) { event.preventDefault(); event.stopImmediatePropagation(); }
        }, { capture: true, signal: controller.signal });
        checkbox.addEventListener("change", (event) => {
          if (options.disabled || options.readOnly) {
            checkbox.checked = currentValue;
            event.preventDefault();
            event.stopImmediatePropagation();
            return;
          }
          currentValue = checkbox.checked;
          options.onChange?.(currentValue);
        }, { signal: controller.signal });
        const element = el("label", "field plugin-control plugin-toggle", extensionUiLabel(options.label), checkbox);
        extensionUiFieldDescriptions(element, [checkbox], options);
        return extensionUiControl(element, () => checkbox.checked, (value) => {
          if (typeof value !== "boolean") throw new Error("Toggle value must be boolean.");
          currentValue = value;
          checkbox.checked = value;
        }, controller, signal);
      },
      select(options) {
        if (!options || typeof options.label !== "string" && typeof options.label !== "object") throw new Error("Invalid plugin select.");
        validateExtensionUiFieldOptions(options);
        extensionUiChoices(options.choices, options.value);
        const controller = makeController();
        const select = el("select", "");
        let choices = [...options.choices];
        let currentValue = options.value;
        const renderChoices = (): void => {
          select.replaceChildren(...choices.map((choice) => {
            const option = el("option", "", extensionUiTextNode(choice.label));
            option.value = choice.value;
            return option;
          }));
          select.value = currentValue;
        };
        renderChoices();
        select.disabled = options.disabled === true;
        select.required = options.required === true;
        if (options.readOnly) select.setAttribute("aria-readonly", "true");
        select.addEventListener("click", (event) => {
          if (options.readOnly) { event.preventDefault(); event.stopImmediatePropagation(); }
        }, { capture: true, signal: controller.signal });
        select.addEventListener("keydown", (event) => {
          if (options.readOnly && event.key !== "Tab") { event.preventDefault(); event.stopImmediatePropagation(); }
        }, { capture: true, signal: controller.signal });
        select.addEventListener("change", (event) => {
          if (options.disabled || options.readOnly) {
            select.value = currentValue;
            event.preventDefault();
            event.stopImmediatePropagation();
            return;
          }
          currentValue = select.value;
          options.onChange?.(currentValue);
        }, { signal: controller.signal });
        const element = el("label", "field plugin-control", extensionUiLabel(options.label), select);
        extensionUiFieldDescriptions(element, [select], options);
        const setChoices = (nextChoices: readonly ExtensionUiOption[], value?: string): void => {
          const nextValue = value ?? currentValue;
          extensionUiChoices(nextChoices, nextValue);
          choices = [...nextChoices];
          currentValue = nextValue;
          renderChoices();
        };
        return extensionUiControl(element, () => select.value, (value) => {
          if (!choices.some((choice) => choice.value === value)) throw new Error("Unsupported select value.");
          currentValue = value;
          select.value = value;
        }, controller, signal, () => {}, setChoices);
      },
      radioGroup(options) {
        if (!options || typeof options.label !== "string" && typeof options.label !== "object") throw new Error("Invalid plugin radio group.");
        validateExtensionUiFieldOptions(options);
        extensionUiChoices(options.choices, options.value);
        const controller = makeController();
        const group = Amaterasu.radioGroup(
          extensionUiTextNode(options.label),
          options.choices.map((choice) => ({ value: choice.value, label: extensionUiTextNode(choice.label) })),
          options.value,
          (value) => options.onChange?.(value),
          controller.signal,
          { disabled: options.disabled, readOnly: options.readOnly, required: options.required },
        );
        const applyDescriptions = extensionUiFieldDescriptions(group.node, [...group.controls, group.node], options);
        const setChoices = (nextChoices: readonly ExtensionUiOption[], value?: string): void => {
          const selected = value ?? group.value();
          extensionUiChoices(nextChoices, selected);
          const localized = nextChoices.map((choice) => ({ value: choice.value, label: extensionUiTextNode(choice.label) }));
          group.setChoices(localized, selected);
          applyDescriptions([...group.controls, group.node]);
        };
        return extensionUiControl(group.node, group.value, group.setValue, controller, signal, group.dispose, setChoices);
      },
      slider(options) {
        if (!options || typeof options.label !== "string" && typeof options.label !== "object") throw new Error("Invalid plugin slider.");
        validateExtensionUiFieldOptions(options);
        const controller = makeController();
        const label = extensionUiLabel(options.label);
        const slider = Amaterasu.boundedSlider(
          options.minimum,
          options.maximum,
          options.value,
          typeof options.label === "string" ? options.label : options.label.source,
          false,
          "plugin-slider-inputs",
          {
            step: options.step ?? "any",
            onChange: options.onChange,
            signal: controller.signal,
            accessibleLabel: label,
            disabled: options.disabled,
            readOnly: options.readOnly,
            required: options.required,
          },
        );
        const element = el("div", "field plugin-control plugin-slider", label, slider.node);
        extensionUiFieldDescriptions(element, [slider.range, slider.number], options);
        return extensionUiControl(element, slider.value, slider.setValue, controller, signal, slider.dispose);
      },
    };
    return Object.freeze(ui);
  }
}
