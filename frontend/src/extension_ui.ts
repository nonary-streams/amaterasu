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

  export interface ExtensionUiFieldStatePatch {
    disabled?: boolean;
    readOnly?: boolean;
    required?: boolean;
    help?: ExtensionUiText | null;
    error?: ExtensionUiText | null;
  }

  export interface ExtensionUiControl<T> {
    readonly element: HTMLElement;
    value(): T;
    setValue(value: T): void;
    setState(patch: ExtensionUiFieldStatePatch): void;
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
  let nextExtensionUiFieldId = 0;

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

  function extensionUiControlLabel(text: ExtensionUiText, control: HTMLElement): HTMLLabelElement {
    const label = el("label", "field-label", extensionUiTextNode(text));
    const id = `amaterasu-plugin-ui-control-${++nextExtensionUiFieldId}`;
    control.id = id;
    label.htmlFor = id;
    return label;
  }

  function extensionUiReadOnlyKeyMutation(event: KeyboardEvent): boolean {
    if (event.ctrlKey || event.metaKey || event.altKey) return false;
    return event.key.length === 1 || ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End", "PageUp", "PageDown", "Enter", " ", "Spacebar", "Backspace", "Delete"].includes(event.key);
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
    updateChoices?: undefined,
    updateState?: (patch: ExtensionUiFieldStatePatch) => void,
  ): ExtensionUiControl<T>;
  function extensionUiControl(
    element: HTMLElement,
    read: () => string,
    write: (value: string) => void,
    controller: AbortController,
    parentSignal: AbortSignal,
    release: () => void,
    updateChoices: (choices: readonly ExtensionUiOption[], value?: string) => void,
    updateState?: (patch: ExtensionUiFieldStatePatch) => void,
  ): ExtensionUiChoiceControl;
  function extensionUiControl<T>(
    element: HTMLElement,
    read: () => T,
    write: (value: T) => void,
    controller: AbortController,
    parentSignal: AbortSignal,
    release: () => void = () => {},
    updateChoices?: (choices: readonly ExtensionUiOption[], value?: string) => void,
    updateState?: (patch: ExtensionUiFieldStatePatch) => void,
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
      setState: (patch) => { if (!disposed) updateState?.(patch); },
      dispose,
    };
    if (updateChoices) {
      control.setChoices = (choices, value) => { if (!disposed) updateChoices(choices, value); };
    }
    return Object.freeze(control);
  }

  interface MutableExtensionUiFieldState {
    disabled: boolean;
    readOnly: boolean;
    required: boolean;
    help?: ExtensionUiText;
    error?: ExtensionUiText;
  }

  function extensionUiFieldState(
    element: HTMLElement,
    controls: readonly HTMLElement[],
    state: MutableExtensionUiFieldState,
    applyControlState: (state: MutableExtensionUiFieldState) => void,
  ): ((patch: ExtensionUiFieldStatePatch) => void) & { refreshControls(controls: readonly HTMLElement[]): void } {
    const createDescription = (kind: "help" | "error", text: ExtensionUiText, id?: string): HTMLElement => {
      const description = el("small", `plugin-control-${kind}`, extensionUiTextNode(text));
      description.id = id ?? `amaterasu-plugin-ui-description-${++nextExtensionUiDescriptionId}`;
      if (kind === "error") description.setAttribute("role", "alert");
      return description;
    };
    let activeControls = [...controls];
    let helpNode = state.help === undefined ? null : createDescription("help", state.help);
    let errorNode = state.error === undefined ? null : createDescription("error", state.error);
    const sync = (): void => {
      applyControlState(state);
      const descriptionIds = [helpNode?.id, errorNode?.id].filter((id): id is string => !!id).join(" ");
      for (const control of activeControls) {
        if (descriptionIds) control.setAttribute("aria-describedby", descriptionIds);
        else control.removeAttribute("aria-describedby");
        if (state.error !== undefined) control.setAttribute("aria-invalid", "true");
        else control.removeAttribute("aria-invalid");
        if (state.required) control.setAttribute("aria-required", "true");
        else control.removeAttribute("aria-required");
      }
      if (element.getAttribute("role") === "radiogroup") {
        if (state.error !== undefined) element.setAttribute("aria-invalid", "true");
        else element.removeAttribute("aria-invalid");
        if (state.required) element.setAttribute("aria-required", "true");
        else element.removeAttribute("aria-required");
      }
    };
    if (helpNode) element.append(helpNode);
    if (errorNode) element.append(errorNode);
    sync();

    const updateState = ((patch): void => {
      if (!patch || typeof patch !== "object" || Array.isArray(patch)) throw new Error("Invalid plugin UI state patch.");
      const allowed = ["disabled", "readOnly", "required", "help", "error"];
      if (Object.keys(patch).some((key) => !allowed.includes(key))) throw new Error("Unknown plugin UI state option.");
      const next = { ...state };
      for (const key of ["disabled", "readOnly", "required"] as const) {
        const value = patch[key];
        if (value !== undefined) {
          if (typeof value !== "boolean") throw new Error(`Invalid plugin UI ${key} state.`);
          next[key] = value;
        }
      }
      const hasHelp = Object.prototype.hasOwnProperty.call(patch, "help") && patch.help !== undefined;
      const hasError = Object.prototype.hasOwnProperty.call(patch, "error") && patch.error !== undefined;
      let nextHelpNode = helpNode;
      let nextErrorNode = errorNode;
      if (hasHelp) {
        const help = patch.help;
        if (help === null) { next.help = undefined; nextHelpNode = null; }
        else if (help !== undefined) { next.help = help; nextHelpNode = createDescription("help", help, helpNode?.id); }
      }
      if (hasError) {
        const error = patch.error;
        if (error === null) { next.error = undefined; nextErrorNode = null; }
        else if (error !== undefined) { next.error = error; nextErrorNode = createDescription("error", error, errorNode?.id); }
      }

      const commitDescription = (current: HTMLElement | null, prepared: HTMLElement | null): HTMLElement | null => {
        if (current && prepared) {
          current.replaceChildren(...Array.from(prepared.childNodes));
          return current;
        }
        current?.remove();
        if (prepared) element.append(prepared);
        return prepared;
      };
      if (hasHelp) helpNode = commitDescription(helpNode, nextHelpNode);
      if (hasError) errorNode = commitDescription(errorNode, nextErrorNode);
      Object.assign(state, next);
      sync();
    }) as ((patch: ExtensionUiFieldStatePatch) => void) & { refreshControls(controls: readonly HTMLElement[]): void };
    updateState.refreshControls = (nextControls) => { activeControls = [...nextControls]; sync(); };
    return updateState;
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
        let currentValue = options.value;
        const state: MutableExtensionUiFieldState = {
          disabled: options.disabled === true, readOnly: options.readOnly === true, required: options.required === true,
          help: options.help, error: options.error,
        };
        checkbox.addEventListener("click", (event) => {
          if (state.readOnly) { event.preventDefault(); event.stopImmediatePropagation(); }
        }, { capture: true, signal: controller.signal });
        checkbox.addEventListener("change", (event) => {
          if (state.disabled || state.readOnly) {
            checkbox.checked = currentValue;
            event.preventDefault();
            event.stopImmediatePropagation();
            return;
          }
          currentValue = checkbox.checked;
          options.onChange?.(currentValue);
        }, { signal: controller.signal });
        const element = el("div", "field plugin-control plugin-toggle", extensionUiControlLabel(options.label, checkbox), checkbox);
        const setState = extensionUiFieldState(element, [checkbox], state, (next) => {
          checkbox.disabled = next.disabled;
          checkbox.required = next.required;
          if (next.readOnly) checkbox.setAttribute("aria-readonly", "true"); else checkbox.removeAttribute("aria-readonly");
        });
        return extensionUiControl(element, () => checkbox.checked, (value) => {
          if (typeof value !== "boolean") throw new Error("Toggle value must be boolean.");
          currentValue = value;
          checkbox.checked = value;
        }, controller, signal, undefined, undefined, setState);
      },
      select(options) {
        if (!options || typeof options.label !== "string" && typeof options.label !== "object") throw new Error("Invalid plugin select.");
        validateExtensionUiFieldOptions(options);
        extensionUiChoices(options.choices, options.value);
        const controller = makeController();
        const select = el("select", "");
        let choices = [...options.choices];
        let currentValue = options.value;
        const state: MutableExtensionUiFieldState = {
          disabled: options.disabled === true, readOnly: options.readOnly === true, required: options.required === true,
          help: options.help, error: options.error,
        };
        const makeChoiceOptions = (nextChoices: readonly ExtensionUiOption[]): HTMLOptionElement[] =>
          nextChoices.map((choice) => {
            const option = el("option", "", extensionUiTextNode(choice.label));
            option.value = choice.value;
            return option;
          });
        select.replaceChildren(...makeChoiceOptions(choices));
        select.value = currentValue;
        select.addEventListener("click", (event) => {
          if (state.readOnly) { event.preventDefault(); event.stopImmediatePropagation(); }
        }, { capture: true, signal: controller.signal });
        select.addEventListener("keydown", (event) => {
          if (state.readOnly && extensionUiReadOnlyKeyMutation(event)) { event.preventDefault(); event.stopImmediatePropagation(); }
        }, { capture: true, signal: controller.signal });
        select.addEventListener("change", (event) => {
          if (state.disabled || state.readOnly) {
            select.value = currentValue;
            event.preventDefault();
            event.stopImmediatePropagation();
            return;
          }
          currentValue = select.value;
          options.onChange?.(currentValue);
        }, { signal: controller.signal });
        const element = el("div", "field plugin-control", extensionUiControlLabel(options.label, select), select);
        const setState = extensionUiFieldState(element, [select], state, (next) => {
          select.disabled = next.disabled;
          select.required = next.required;
          if (next.readOnly) select.setAttribute("aria-readonly", "true"); else select.removeAttribute("aria-readonly");
        });
        const setChoices = (nextChoices: readonly ExtensionUiOption[], value?: string): void => {
          const nextValue = value ?? currentValue;
          extensionUiChoices(nextChoices, nextValue);
          const preparedChoices = [...nextChoices];
          const preparedOptions = makeChoiceOptions(preparedChoices);
          select.replaceChildren(...preparedOptions);
          select.value = nextValue;
          choices = preparedChoices;
          currentValue = nextValue;
        };
        return extensionUiControl(element, () => select.value, (value) => {
          if (!choices.some((choice) => choice.value === value)) throw new Error("Unsupported select value.");
          currentValue = value;
          select.value = value;
        }, controller, signal, () => {}, setChoices, setState);
      },
      radioGroup(options) {
        if (!options || typeof options.label !== "string" && typeof options.label !== "object") throw new Error("Invalid plugin radio group.");
        validateExtensionUiFieldOptions(options);
        extensionUiChoices(options.choices, options.value);
        const controller = makeController();
        const state: MutableExtensionUiFieldState = {
          disabled: options.disabled === true, readOnly: options.readOnly === true, required: options.required === true,
          help: options.help, error: options.error,
        };
        const group = Amaterasu.radioGroup(
          extensionUiTextNode(options.label),
          options.choices.map((choice) => ({ value: choice.value, label: extensionUiTextNode(choice.label) })),
          options.value,
          (value) => options.onChange?.(value),
          controller.signal,
          { disabled: state.disabled, readOnly: state.readOnly, required: state.required },
        );
        const setState = extensionUiFieldState(group.node, [...group.controls, group.node], state, (next) => {
          group.setBehavior({ disabled: next.disabled, readOnly: next.readOnly, required: next.required });
        });
        const setChoices = (nextChoices: readonly ExtensionUiOption[], value?: string): void => {
          const selected = value ?? group.value();
          extensionUiChoices(nextChoices, selected);
          const localized = nextChoices.map((choice) => ({ value: choice.value, label: extensionUiTextNode(choice.label) }));
          group.setChoices(localized, selected);
          setState.refreshControls([...group.controls, group.node]);
        };
        return extensionUiControl(group.node, group.value, group.setValue, controller, signal, group.dispose, setChoices, setState);
      },
      slider(options) {
        if (!options || typeof options.label !== "string" && typeof options.label !== "object") throw new Error("Invalid plugin slider.");
        validateExtensionUiFieldOptions(options);
        const controller = makeController();
        const state: MutableExtensionUiFieldState = {
          disabled: options.disabled === true, readOnly: options.readOnly === true, required: options.required === true,
          help: options.help, error: options.error,
        };
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
            disabled: state.disabled,
            readOnly: state.readOnly,
            required: state.required,
          },
        );
        const element = el("div", "field plugin-control plugin-slider", label, slider.node);
        const setState = extensionUiFieldState(element, [slider.range, slider.number], state, (next) => {
          slider.setBehavior({ disabled: next.disabled, readOnly: next.readOnly, required: next.required });
        });
        return extensionUiControl(element, slider.value, slider.setValue, controller, signal, slider.dispose, undefined, setState);
      },
    };
    return Object.freeze(ui);
  }
}
