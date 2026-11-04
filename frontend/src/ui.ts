namespace Amaterasu {
  function uiElementLiteral(value: string): TextValue {
    return typeof Amaterasu.literalText === "function" ? Amaterasu.literalText(value) : value;
  }

  type Child = Node | TextValue | null | undefined;

  function isTextValue(child: Child): child is TextValue {
    return typeof child === "string" || (
      typeof child === "object" && child !== null &&
      "kind" in child && "value" in child
    );
  }
  export function el<K extends keyof HTMLElementTagNameMap>(
    tag: K,
    className = "",
    ...children: Child[]
  ): HTMLElementTagNameMap[K] {
    const node = document.createElement(tag);
    node.className = className;
    for (const child of children) {
      if (child == null) continue;
      if (isTextValue(child)) {
        node.append(
          typeof Amaterasu.textValueNode === "function"
            ? Amaterasu.textValueNode(child)
            : typeof child === "string"
              ? child
              : child.value,
        );
      } else node.append(child);
    }
    return node;
  }

  export function button(
    text: TextValue,
    action: () => void | Promise<void>,
    kind = "secondary",
  ): HTMLButtonElement {
    const node = el("button", `button ${kind}`, text);
    node.type = "button";
    node.addEventListener("click", () => void action());
    return node;
  }

  export function field(
    label: TextValue,
    input: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement,
    hint?: TextValue,
  ): HTMLElement {
    const wrapper = el(
      "label",
      "field",
      el("span", "field-label", label),
      input,
    );
    if (hint) wrapper.append(el("small", "muted", hint));
    return wrapper;
  }

  export function input(
    name: string,
    value = "",
    type = "text",
  ): HTMLInputElement {
    const node = el("input");
    node.name = name;
    node.type = type;
    node.value = value;
    return node;
  }

  export interface RadioGroupOption {
    value: string;
    label: TextValue | Node;
  }

  export interface RadioGroupBehavior {
    disabled?: boolean;
    readOnly?: boolean;
    required?: boolean;
  }

  export interface RadioGroupControl {
    node: HTMLElement;
    controls: readonly HTMLInputElement[];
    value(): string;
    setValue(value: string): void;
    setChoices(options: readonly RadioGroupOption[], value?: string): void;
    setBehavior(behavior: RadioGroupBehavior): void;
    dispose(): void;
  }

  let nextRadioGroupId = 0;

  // Native radio inputs keep arrow-key navigation and selection semantics.
  // The labelled group exposes the same accessible control to every consumer.
  export function radioGroup(
    label: TextValue | Node,
    options: readonly RadioGroupOption[],
    selected: string,
    onChange: (value: string) => void = () => {},
    signal?: AbortSignal,
    behavior: RadioGroupBehavior = {},
  ): RadioGroupControl {
    const validate = (choices: readonly RadioGroupOption[], value: string): void => {
      if (!choices.length || new Set(choices.map((option) => option.value)).size !== choices.length ||
          !choices.some((option) => option.value === value) ||
          choices.some((option) => typeof option.value !== "string" || !option.value || typeof option.label === "undefined"))
        throw new Error("Invalid radio group options or selection.");
    };
    if (typeof onChange !== "function") throw new Error("Invalid radio group callback.");
    validate(options, selected);
    const controller = new AbortController();
    let disposed = false;
    let currentValue = selected;
    let currentOptions = options;
    let controls: HTMLInputElement[] = [];
    let currentBehavior: RadioGroupBehavior = { ...behavior };
    let choiceController = new AbortController();
    const dispose = (): void => {
      if (disposed) return;
      disposed = true;
      controller.abort();
      choiceController.abort();
      signal?.removeEventListener("abort", dispose);
    };
    if (signal?.aborted) dispose();
    else signal?.addEventListener("abort", dispose, { once: true });
    const name = `amaterasu-radio-${++nextRadioGroupId}`;
    const group = el("div", "radio-group");
    group.setAttribute("role", "radiogroup");
    const labelNode = el("span", "radio-group-label", label);
    labelNode.id = `amaterasu-radio-label-${nextRadioGroupId}`;
    const optionsNode = el("div", "radio-options");
    group.setAttribute("aria-labelledby", labelNode.id);
    const applyBehavior = (): void => {
      if (currentBehavior.disabled) group.setAttribute("aria-disabled", "true"); else group.removeAttribute("aria-disabled");
      if (currentBehavior.readOnly) group.setAttribute("aria-readonly", "true"); else group.removeAttribute("aria-readonly");
      if (currentBehavior.required) group.setAttribute("aria-required", "true"); else group.removeAttribute("aria-required");
      for (const control of controls) {
        control.disabled = currentBehavior.disabled === true;
        control.required = currentBehavior.required === true;
        if (currentBehavior.readOnly) control.setAttribute("aria-readonly", "true"); else control.removeAttribute("aria-readonly");
      }
    };
    group.append(labelNode, optionsNode);
    const render = (): void => {
      choiceController.abort();
      choiceController = new AbortController();
      controls = currentOptions.map((option) => {
        const control = input(name, option.value, "radio");
        control.checked = option.value === currentValue;
        control.disabled = currentBehavior.disabled === true;
        control.required = currentBehavior.required === true;
        if (currentBehavior.readOnly) control.setAttribute("aria-readonly", "true");
        control.addEventListener("click", (event) => {
          if (currentBehavior.readOnly) {
            event.preventDefault();
            event.stopImmediatePropagation();
          }
        }, { capture: true, signal: choiceController.signal });
        control.addEventListener("keydown", (event) => {
          if (!currentBehavior.readOnly || event.ctrlKey || event.metaKey || event.altKey) return;
          const index = controls.indexOf(control);
          const focusIndex = event.key === "ArrowDown" || event.key === "ArrowRight" ? (index + 1) % controls.length
            : event.key === "ArrowUp" || event.key === "ArrowLeft" ? (index + controls.length - 1) % controls.length
            : event.key === "Home" ? 0 : event.key === "End" ? controls.length - 1 : -1;
          if (focusIndex >= 0) {
            event.preventDefault();
            event.stopImmediatePropagation();
            controls[focusIndex]?.focus();
            return;
          }
          const mutatingKey = event.key.length === 1 || ["PageUp", "PageDown", "Enter", " ", "Spacebar", "Backspace", "Delete"].includes(event.key);
          if (mutatingKey) {
            event.preventDefault();
            event.stopImmediatePropagation();
          }
        }, { capture: true, signal: choiceController.signal });
        control.addEventListener("change", (event) => {
          if (currentBehavior.disabled || currentBehavior.readOnly) {
            for (const peer of controls) peer.checked = peer.value === currentValue;
            event.preventDefault();
            event.stopImmediatePropagation();
            return;
          }
          if (!control.checked) return;
          currentValue = control.value;
          for (const peer of controls) peer.checked = peer === control;
          onChange(control.value);
        }, { capture: true, signal: choiceController.signal });
        return control;
      });
      optionsNode.replaceChildren(...currentOptions.map((option, index) =>
        el("label", "radio-option", controls[index], el("span", "", option.label))));
    };
    render();
    applyBehavior();
    return {
      node: group,
      get controls() { return controls; },
      value: () => currentValue,
      setValue: (value) => {
        if (!currentOptions.some((option) => option.value === value)) throw new Error("Unsupported radio group value.");
        currentValue = value;
        for (const control of controls) control.checked = control.value === value;
      },
      setBehavior: (nextBehavior) => {
        currentBehavior = { ...nextBehavior };
        applyBehavior();
      },
      setChoices: (nextOptions, nextValue = currentValue) => {
        if (disposed) return;
        validate(nextOptions, nextValue);
        const focusedValue = controls.find((control) => control === document.activeElement)?.value;
        currentOptions = [...nextOptions];
        currentValue = nextValue;
        render();
        if (focusedValue !== undefined) {
          (controls.find((control) => control.value === focusedValue) ??
            controls.find((control) => control.value === currentValue))?.focus();
        }
      },
      dispose,
    };
  }

  export interface BoundedSliderControl {
    node: HTMLElement;
    range: HTMLInputElement;
    number: HTMLInputElement;
    value(): number;
    setValue(value: number): void;
    setBehavior(behavior: Pick<BoundedSliderOptions, "disabled" | "readOnly" | "required">): void;
    dispose(): void;
  }

  export interface BoundedSliderOptions {
    step?: number | "any";
    onChange?: (value: number) => void;
    signal?: AbortSignal;
    accessibleLabel?: Node;
    disabled?: boolean;
    readOnly?: boolean;
    required?: boolean;
  }

  let nextBoundedSliderId = 0;

  export function boundedSlider(
    minimum: number,
    maximum: number,
    value: number,
    label: string,
    integer = false,
    className = "bounded-slider",
    options: BoundedSliderOptions = {},
  ): BoundedSliderControl {
    const step = options.step ?? (integer ? 1 : "any");
    if (!Number.isFinite(minimum) || !Number.isFinite(maximum) || minimum >= maximum ||
        (integer && (!Number.isSafeInteger(minimum) || !Number.isSafeInteger(maximum))) ||
        (step !== "any" && (!Number.isFinite(step) || step <= 0)) ||
        (options.onChange !== undefined && typeof options.onChange !== "function") ||
        !isValidSliderValue(value, minimum, maximum, integer, step))
      throw new Error("Invalid bounded slider limits, step, or value.");

    const number = input("", String(value), "number");
    const range = input("", "", "range");
    for (const control of [number, range]) {
      control.min = String(minimum);
      control.max = String(maximum);
      control.step = String(step);
      control.disabled = options.disabled === true;
      control.required = options.required === true;
      if (options.readOnly) control.setAttribute("aria-readonly", "true");
    }
    if (options.readOnly) number.readOnly = true;
    if (options.accessibleLabel) {
      if (!(options.accessibleLabel as HTMLElement).id)
        (options.accessibleLabel as HTMLElement).id = `amaterasu-slider-label-${++nextBoundedSliderId}`;
      range.setAttribute("aria-labelledby", (options.accessibleLabel as HTMLElement).id);
      number.setAttribute("aria-labelledby", (options.accessibleLabel as HTMLElement).id);
    } else {
      range.setAttribute("aria-label", `${label} slider`);
      number.setAttribute("aria-label", `${label} value`);
    }
    range.value = String(value);
    const controller = new AbortController();
    let disposed = false;
    let currentValue = value;
    let currentBehavior = { disabled: options.disabled === true, readOnly: options.readOnly === true, required: options.required === true };
    const applyBehavior = (): void => {
      for (const control of [range, number]) {
        control.disabled = currentBehavior.disabled;
        control.required = currentBehavior.required;
        if (currentBehavior.readOnly) control.setAttribute("aria-readonly", "true"); else control.removeAttribute("aria-readonly");
      }
      number.readOnly = currentBehavior.readOnly;
    };
    const dispose = (): void => {
      if (disposed) return;
      disposed = true;
      controller.abort();
      options.signal?.removeEventListener("abort", dispose);
    };
    if (options.signal?.aborted) dispose();
    else options.signal?.addEventListener("abort", dispose, { once: true });

    const readValue = (control: HTMLInputElement): number => {
      const parsed = Number(control.value);
      if (!control.value || !control.validity.valid ||
          !isValidSliderValue(parsed, minimum, maximum, integer, step))
        throw new Error(`Enter a valid number from ${minimum} to ${maximum} on step ${step}.`);
      return parsed;
    };
    const restoreCurrentValue = (): void => {
      number.value = String(currentValue);
      range.value = String(currentValue);
    };
    range.addEventListener("click", (event) => {
      if (currentBehavior.readOnly) { event.preventDefault(); event.stopImmediatePropagation(); }
    }, { capture: true, signal: controller.signal });
    range.addEventListener("pointerdown", (event) => {
      if (currentBehavior.readOnly) { event.preventDefault(); event.stopImmediatePropagation(); }
    }, { capture: true, signal: controller.signal });
    range.addEventListener("keydown", (event) => {
      const valueMutationKey = ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End", "PageUp", "PageDown"].includes(event.key);
      if (currentBehavior.readOnly && valueMutationKey) { event.preventDefault(); event.stopImmediatePropagation(); }
    }, { capture: true, signal: controller.signal });
    for (const control of [range, number]) {
      control.addEventListener("input", (event) => {
        if (currentBehavior.disabled || currentBehavior.readOnly) {
          restoreCurrentValue();
          event.preventDefault();
          event.stopImmediatePropagation();
        }
      }, { capture: true, signal: controller.signal });
    }
    range.addEventListener("input", () => {
      let nextValue: number;
      try { nextValue = readValue(range); } catch { return; }
      number.value = String(nextValue);
      currentValue = nextValue;
      options.onChange?.(nextValue);
    }, { signal: controller.signal });
    number.addEventListener("input", () => {
      let nextValue: number;
      try { nextValue = readValue(number); } catch { return; }
      range.value = String(nextValue);
      if (Number(range.value) !== nextValue) return;
      currentValue = nextValue;
      options.onChange?.(nextValue);
    }, { signal: controller.signal });
    const node = el("div", className, range, number);
    return {
      node,
      range,
      number,
      value: () => readValue(number),
      setBehavior: (nextBehavior) => {
        currentBehavior = { disabled: nextBehavior.disabled === true, readOnly: nextBehavior.readOnly === true, required: nextBehavior.required === true };
        applyBehavior();
      },
      setValue: (nextValue) => {
        if (!isValidSliderValue(nextValue, minimum, maximum, integer, step))
          throw new Error(`Slider value must be between ${minimum} and ${maximum} on step ${step}.`);
        const previousNumber = number.value;
        const previousRange = range.value;
        number.value = String(nextValue);
        range.value = String(nextValue);
        if (Number(range.value) !== nextValue) {
          number.value = previousNumber;
          range.value = previousRange;
          throw new Error("Slider value cannot be represented by the range control.");
        }
        currentValue = nextValue;
      },
      dispose,
    };
  }

  export function isValidNumberControlValue(
    value: number,
    minimum?: number,
    maximum?: number,
    step: number | "any" = "any",
    integer = false,
  ): boolean {
    if (!Number.isFinite(value) || minimum !== undefined && value < minimum || maximum !== undefined && value > maximum ||
        (integer && !Number.isSafeInteger(value))) return false;
    if (step === "any") return true;
    if (!Number.isFinite(step) || step <= 0) return false;
    const stepCount = (value - (minimum ?? 0)) / step;
    return Number.isFinite(stepCount) &&
      Math.abs(stepCount - Math.round(stepCount)) <= Number.EPSILON * Math.max(1, Math.abs(stepCount)) * 4;
  }

  function isValidSliderValue(value: number, minimum: number, maximum: number, integer: boolean, step: number | "any"): boolean {
    return isValidNumberControlValue(value, minimum, maximum, step, integer);
  }

  export function sectionHeading(
    title: TextValue,
    description: TextValue,
    ...actions: Child[]
  ): HTMLElement {
    return el(
      "header",
      "page-heading",
      el(
        "div",
        "",
        el("h1", "", title),
        el("p", "muted", description),
      ),
      el("div", "actions", ...actions),
    );
  }

  export function empty(
    title: TextValue,
    description: TextValue,
    action?: HTMLElement,
  ): HTMLElement {
    return el(
      "div",
      "empty-state",
      el("span", "empty-mark", "＋"),
      el("h3", "", title),
      el("p", "muted", description),
      action,
    );
  }

  export function errorPanel(
    error: unknown,
    retry?: () => void | Promise<void>,
  ): HTMLElement {
    const box = el(
      "div",
      "notice error",
      el("strong", "", "Unable to complete request"),
      el("p", "", uiElementLiteral(messageOf(error))),
    );
    box.setAttribute("role", "alert");
    if (retry) box.append(button("Try again", retry));
    return box;
  }

  export function badge(text: TextValue, tone = ""): HTMLElement {
    return el("span", `badge ${tone}`, text);
  }
  export function panel(title: TextValue, ...children: Child[]): HTMLElement {
    return el("section", "panel", el("h2", "", title), ...children);
  }

  type DraftControl = HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;
  export type DraftSnapshot = Map<DraftControl, string>;

  export interface DraftTracker {
    isDirty: (scope?: HTMLElement) => boolean;
    capture: (scope: HTMLElement) => DraftSnapshot;
    markSaved: (snapshot: DraftSnapshot) => void;
    dispose: () => void;
  }

  export class DraftGuardGroup {
    private guards = new Set<{ isDirty: () => boolean; dispose: () => void }>();

    register(isDirty: () => boolean, dispose: () => void): () => void {
      const guard = { isDirty, dispose };
      this.guards.add(guard);
      return () => { if (this.guards.delete(guard)) dispose(); };
    }

    isDirty(): boolean { return [...this.guards].some(guard => guard.isDirty()); }

    dispose(): void {
      const guards = [...this.guards];
      this.guards.clear();
      for (const guard of guards) guard.dispose();
    }
  }

  // Baselines are held only in memory. A captured baseline represents what was
  // submitted, so edits made while a save is in flight remain unsaved.
  export function trackDrafts(root: HTMLElement, onChange: (dirty: boolean) => void = () => {}): DraftTracker {
    const baselines: DraftSnapshot = new Map();
    const signature = (control: DraftControl): string =>
      control instanceof HTMLInputElement && (control.type === "checkbox" || control.type === "radio")
        ? `${control.checked}:${control.value}`
        : control.value;
    const controls = (): DraftControl[] => Array.from(root.querySelectorAll<DraftControl>("input, textarea, select"))
      .filter((control) => !(control instanceof HTMLInputElement && control.type === "hidden") &&
        !control.hasAttribute("data-draft-ignore"));
    const synchronize = (): void => {
      for (const control of baselines.keys()) if (!root.contains(control)) baselines.delete(control);
      for (const control of controls()) if (!baselines.has(control)) baselines.set(control, signature(control));
    };
    const dirty = (scope?: HTMLElement): boolean => {
      synchronize();
      return [...baselines].some(([control, baseline]) =>
        (!scope || scope.contains(control)) && signature(control) !== baseline);
    };
    const changed = (): void => onChange(dirty());
    root.addEventListener("input", changed);
    root.addEventListener("change", changed);
    const observer = new MutationObserver(changed);
    observer.observe(root, { childList: true, subtree: true });
    synchronize();
    return {
      isDirty: dirty,
      capture: (scope) => {
        synchronize();
        return new Map([...baselines.keys()].filter((control) => scope.contains(control))
          .map((control) => [control, signature(control)]));
      },
      markSaved: (snapshot) => {
        for (const [control, value] of snapshot) if (root.contains(control)) baselines.set(control, value);
        changed();
      },
      dispose: () => {
        observer.disconnect();
        root.removeEventListener("input", changed);
        root.removeEventListener("change", changed);
        baselines.clear();
      },
    };
  }

  export async function runAction(
    target: HTMLButtonElement,
    status: HTMLElement,
    action: () => Promise<void>,
  ): Promise<void> {
    if (target.disabled) return;
    target.disabled = true;
    status.replaceChildren();
    try {
      await action();
    } catch (error) {
      if (status.isConnected) status.replaceChildren(errorPanel(error));
    } finally {
      target.disabled = false;
    }
  }

  export function table(headers: string[], rows: Child[][]): HTMLElement {
    const head = el("tr");
    for (const name of headers) {
      const cell = el("th", "", name);
      cell.scope = "col";
      head.append(cell);
    }
    const body = el("tbody");
    for (const row of rows)
      body.append(el("tr", "", ...row.map((value) => el(
        "td",
        "",
        typeof value === "string" ? uiElementLiteral(value) : value,
      ))));
    return el(
      "div",
      "table-scroll",
      el("table", "", el("thead", "", head), body),
    );
  }

  export function pager(
    back: (() => void) | null,
    next: (() => void) | null,
    label: TextValue,
  ): HTMLElement {
    const previous = button("← Previous", () => back?.());
    previous.disabled = !back;
    const following = button("Next →", () => next?.());
    following.disabled = !next;
    return el(
      "div",
      "pager",
      el("span", "muted", label),
      el("div", "actions", previous, following),
    );
  }
}
namespace Amaterasu {
  function uiControlLiteral(value: string): TextValue {
    return typeof Amaterasu.literalText === "function" ? Amaterasu.literalText(value) : value;
  }

  export function keybindingsControl(current?: number[] | null, global = false): {
    field: HTMLElement;
    read: () => number[] | null;
  } {
    const control = el("textarea");
    control.name = global ? "input_keybindings" : "keybindings";
    control.value = keybindingsFormValue(current);
    control.rows = 3;
    control.placeholder = global ? "[16,160,17,162,18,164]" : "Inherit";
    control.required = global;
    const scope = global
      ? "Use [] to clear custom mappings; built-in modifiers and the optional Right Alt mapping still apply."
      : "Blank inherits the next scope. Use [] to clear lower custom mappings; built-in modifiers and the optional Right Alt mapping still apply.";
    return {
      field: field("Keybindings", control,
        uiControlLiteral(`${scope} Enter up to 256 ordered source/destination byte pairs, such as [0x41, 0x42]. Generic Shift, Control, and Alt always map to their left variants. Other keys use the first mapping for their source, without chaining. A custom Right Alt mapping takes precedence over the Windows-key option.`)),
      read: () => {
        const value = parseKeybindingsOverride(control.value);
        if (global && value === null)
          throw new Error("Global keybindings require a list; use [] to clear custom mappings.");
        return value;
      },
    };
  }

  export function advancedInputOverrideControls(current?: AdvancedInputOverrides): {
    fields: HTMLElement[];
    read: () => AdvancedInputOverrides;
  } {
    const values = advancedInputOverrideFormValues(current);
    const controls = new Map<keyof AdvancedInputOverrides, HTMLInputElement | HTMLSelectElement>();
    const keybindings = keybindingsControl(current?.keybindings);
    const fields: HTMLElement[] = [keybindings.field];
    for (const [name, label] of [
      ["high_resolution_scrolling", "High-resolution scrolling"],
      ["always_send_scancodes", "Always send scancodes"],
      ["key_rightalt_to_key_win", "Right Alt as Windows key"],
    ] as const) {
      const select = el("select");
      select.name = name;
      for (const [value, text] of [["", "Inherit"], ["true", "Enabled"], ["false", "Disabled"]]) {
        const option = el("option", "", text);
        option.value = value ?? "";
        select.append(option);
      }
      select.value = values[name];
      controls.set(name, select);
      fields.push(field(label, select, "Inherit the next scope, or set an explicit input preference."));
    }
    for (const [name, label, help] of [
      ["key_repeat_delay", "Key repeat delay (milliseconds)", "Blank inherits. Zero disables key repeat; maximum 2147483647."],
      ["key_repeat_frequency", "Key repeat frequency (Hz)", "Blank inherits. Use a finite nonnegative value; zero selects the 24.9 Hz default."],
    ] as const) {
      const control = input(name, values[name], "number");
      control.min = "0";
      control.step = name === "key_repeat_delay" ? "1" : "any";
      if (name === "key_repeat_delay") control.max = String(0x7fff_ffff);
      control.placeholder = "Inherit";
      controls.set(name, control);
      fields.push(field(label, control, help));
    }
    return {
      fields,
      read: () => ({
        ...parseAdvancedInputOverrides({
          get: (name: string) => controls.get(name as keyof AdvancedInputOverrides)?.value ?? null,
        }),
        keybindings: keybindings.read(),
      }),
    };
  }

  export function softwareEncoderOverrideControls(
    current?: SoftwareEncoderOverrides,
    global = false,
  ): {
    fields: HTMLElement[];
    read: () => SoftwareEncoderOverrides;
  } {
    const controls = new Map<string, HTMLInputElement | HTMLSelectElement>();
    const fields: HTMLElement[] = [];
    for (const [key, label, fallback] of [
      ["sw_preset", "Software H.264 preset", "superfast"],
      ["sw_tune", "Software H.264 tune", "zerolatency"],
    ] as const) {
      const mode = el("select");
      mode.name = `${key}_mode`;
      const inherited = el("option", "", global ? `Default (${fallback})` : "Inherit");
      inherited.value = "inherit";
      const specified = el("option", "", "Exact string (including empty)");
      specified.value = "value";
      mode.append(inherited, specified);
      const stored = current?.[key];
      mode.value = stored == null ? "inherit" : "value";
      const value = input(`${key}_value`, stored ?? "");
      value.disabled = mode.value === "inherit";
      mode.addEventListener("change", () => {
        value.disabled = mode.value === "inherit";
      });
      controls.set(mode.name, mode);
      controls.set(value.name, value);
      fields.push(
        field(label, mode, "Inherit the lower scope, or retain an exact software tuning string."),
        field(uiControlLiteral(`${label} exact value`), value, "Applied by the x264 software backend. Exact strings are validated by the installed codec at launch; hardware encoders preserve these fields without applying them."),
      );
    }
    const minThreads = input("min_threads", current?.min_threads?.toString() ?? "", "number");
    minThreads.min = "-2147483648";
    minThreads.max = "2147483647";
    minThreads.step = "1";
    minThreads.placeholder = global ? "Default request (2)" : "Inherit";
    controls.set(minThreads.name, minThreads);
    fields.push(field(
      "Minimum software slices",
      minThreads,
      "For software x264, the requested lower bound is max(client-negotiated slices per frame, this value); nonpositive values leave the client count as the floor. x264 may cap effective threads or slices for frame dimensions and backend limits. Hardware encoders preserve this field without applying it.",
    ));
    const qp = input("qp", current?.qp?.toString() ?? "", "number");
    qp.min = "-2147483648";
    qp.max = "2147483647";
    qp.step = "1";
    qp.placeholder = global ? "Default (28)" : "Inherit";
    controls.set(qp.name, qp);
    fields.push(field(
      "QP",
      qp,
      "The reference uses default 28 and consumes QP for VAAPI constant-quality encoding. Amaterasu currently has no VAAPI consumer, and x264 ABR ignores this value; it is retained as configured metadata.",
    ));
    const backend = el("select");
    backend.name = "encoder_backend";
    for (const [value, label] of [
      ["", global ? "Trusted startup selection" : "Inherit"],
      ["auto", "Automatic (registered hardware, then available x264)"],
      ["software", "Software x264 (requires available backend)"],
    ] as const) {
      const option = el("option", "", label);
      option.value = value;
      backend.append(option);
    }
    backend.value = current?.encoder ?? "";
    controls.set(backend.name, backend);
    fields.push(field("Encoder backend", backend, "Explicit software selection fails when x264 is unavailable. Automatic selection does not probe hardware or retry a failed hardware open."));
    return {
      fields,
      read: () => parseSoftwareEncoderOverrides({
        get: (name: string) => controls.get(name)?.value ?? null,
      }),
    };
  }

  export function confirmAction(
    title: TextValue,
    description: TextValue,
    label: TextValue,
  ): Promise<boolean> {
    return new Promise((resolve) => {
      const dialog = el("dialog", "confirmation");
      if (typeof title === "object" && title.kind === "literal") {
        dialog.setAttribute("aria-label", title.value);
      } else {
        if (typeof Amaterasu.setLocalizedAttribute === "function") {
          Amaterasu.setLocalizedAttribute(
            dialog,
            "aria-label",
            typeof title === "string" ? title : title.value,
          );
        } else {
          dialog.setAttribute("aria-label", typeof title === "string" ? title : title.value);
        }
      }
      const finish = (accepted: boolean) => {
        dialog.close();
        dialog.remove();
        resolve(accepted);
      };
      const cancel = button("Cancel", () => finish(false));
      dialog.append(
        el("h2", "", title),
        el("p", "muted", description),
        el(
          "div",
          "actions",
          cancel,
          button(label, () => finish(true), "danger"),
        ),
      );
      dialog.addEventListener("cancel", (event) => {
        event.preventDefault();
        finish(false);
      });
      document.body.append(dialog);
      dialog.showModal();
      cancel.focus();
    });
  }
}
