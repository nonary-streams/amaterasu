namespace Amaterasu {
  type Child = Node | string | null | undefined;
  export function el<K extends keyof HTMLElementTagNameMap>(
    tag: K,
    className = "",
    ...children: Child[]
  ): HTMLElementTagNameMap[K] {
    const node = document.createElement(tag);
    node.className = className;
    for (const child of children) if (child != null) node.append(child);
    return node;
  }

  export function button(
    text: string,
    action: () => void | Promise<void>,
    kind = "secondary",
  ): HTMLButtonElement {
    const node = el("button", `button ${kind}`, text);
    node.type = "button";
    node.addEventListener("click", () => void action());
    return node;
  }

  export function field(
    label: string,
    input: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement,
    hint?: string,
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

  export function sectionHeading(
    title: string,
    description: string,
    ...actions: Child[]
  ): HTMLElement {
    return el(
      "header",
      "page-heading",
      el(
        "div",
        "",
        el("p", "eyebrow", "HOST CONSOLE"),
        el("h1", "", title),
        el("p", "muted", description),
      ),
      el("div", "actions", ...actions),
    );
  }

  export function empty(
    title: string,
    description: string,
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
      el("p", "", messageOf(error)),
    );
    box.setAttribute("role", "alert");
    if (retry) box.append(button("Try again", retry));
    return box;
  }

  export function badge(text: string, tone = ""): HTMLElement {
    return el("span", `badge ${tone}`, text);
  }
  export function panel(title: string, ...children: Child[]): HTMLElement {
    return el("section", "panel", el("h2", "", title), ...children);
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
      body.append(el("tr", "", ...row.map((value) => el("td", "", value))));
    return el(
      "div",
      "table-scroll",
      el("table", "", el("thead", "", head), body),
    );
  }

  export function pager(
    back: (() => void) | null,
    next: (() => void) | null,
    label: string,
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
        `${scope} Enter up to 256 ordered source/destination byte pairs, such as [0x41, 0x42]. Generic Shift, Control, and Alt always map to their left variants. Other keys use the first mapping for their source, without chaining. A custom Right Alt mapping takes precedence over the Windows-key option.`),
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
        field(`${label} exact value`, value, "Applied by the x264 software backend. Exact strings are validated by the installed codec at launch; hardware encoders preserve these fields without applying them."),
      );
    }
    const minThreads = input("min_threads", current?.min_threads?.toString() ?? "", "number");
    minThreads.min = "1";
    minThreads.max = "256";
    minThreads.step = "1";
    minThreads.placeholder = global ? "Default request (2)" : "Inherit";
    controls.set(minThreads.name, minThreads);
    fields.push(field(
      "Requested software thread minimum",
      minThreads,
      "Requests a minimum thread count for software H.264 encoding. The codec may cap the effective thread count based on frame size and runtime constraints. Hardware selections preserve this request without applying it.",
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
    title: string,
    description: string,
    label: string,
  ): Promise<boolean> {
    return new Promise((resolve) => {
      const dialog = el("dialog", "confirmation");
      dialog.setAttribute("aria-label", title);
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
