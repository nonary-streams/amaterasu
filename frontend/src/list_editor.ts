namespace Amaterasu {
  export interface AccessibleListEditorOptions<Value> {
    label: string;
    values: readonly Value[];
    formatValue(value: Value): string;
    parseValue(text: string): Value;
    onChange?(values: readonly Value[]): void;
    maxItems?: number;
  }

  export interface AccessibleListEditor<Value> {
    element: HTMLElement;
    values(): Value[];
    setValues(values: readonly Value[]): void;
    dispose(): void;
  }

  interface ListEditorEntry<Value> {
    text: string;
    value: Value | undefined;
    valid: boolean;
  }

  let nextListEditorId = 0;

  /**
   * Create an ordered editor for string lists or caller-defined structured values.
   * Existing values stay intact until their own input is edited, so callers can
   * display and retain entries they do not recognize through a custom formatter.
   */
  export function accessibleListEditor<Value>(
    options: AccessibleListEditorOptions<Value>,
  ): AccessibleListEditor<Value> {
    const maxItems = options.maxItems ?? 256;
    if (typeof options.label !== "string" || !options.label.trim() ||
        !Array.isArray(options.values) || typeof options.formatValue !== "function" ||
        typeof options.parseValue !== "function" ||
        (options.onChange !== undefined && typeof options.onChange !== "function") ||
        !Number.isInteger(maxItems) || maxItems < 1 || options.values.length > maxItems) {
      throw new Error("Invalid list editor options.");
    }

    const id = ++nextListEditorId;
    const root = document.createElement("fieldset");
    root.className = "list-editor";
    const legend = document.createElement("legend");
    legend.textContent = options.label;
    const list = document.createElement("ol");
    list.className = "list-editor-items";
    list.setAttribute("aria-label", `${options.label} items`);
    const status = document.createElement("p");
    status.id = `amaterasu-list-editor-status-${id}`;
    status.className = "list-editor-status muted";
    status.setAttribute("aria-live", "polite");
    status.setAttribute("aria-atomic", "true");
    const add = document.createElement("button");
    add.type = "button";
    add.className = "button secondary";
    add.textContent = "Add item";
    const rootController = new AbortController();
    let rowController = new AbortController();
    let disposed = false;
    let entries: ListEditorEntry<Value>[] = options.values.map((value) => ({
      text: options.formatValue(value),
      value,
      valid: true,
    }));

    root.append(legend, list, status, add);

    function currentValues(): Value[] {
      if (entries.some((entry) => !entry.valid || entry.value === undefined))
        throw new Error("Resolve invalid list entries before reading their values.");
      return entries.map((entry) => entry.value as Value);
    }

    function publish(): void {
      if (entries.every((entry) => entry.valid && entry.value !== undefined))
        options.onChange?.(currentValues());
    }

    function setStatus(message: string): void {
      status.textContent = message;
    }

    function renderRows(focusIndex?: number): void {
      rowController.abort();
      rowController = new AbortController();
      list.replaceChildren();
      entries.forEach((entry, index) => {
        const item = document.createElement("li");
        item.className = "list-editor-item";
        const input = document.createElement("input");
        input.type = "text";
        input.className = "list-editor-input";
        input.value = entry.text;
        input.setAttribute("aria-label", `${options.label}, item ${index + 1}`);
        input.setAttribute("aria-describedby", status.id);
        input.addEventListener("input", () => {
          entry.text = input.value;
          let value: Value;
          try {
            value = options.parseValue(input.value);
            if (value === undefined) throw new Error("The value is invalid.");
          } catch (error) {
            entry.value = undefined;
            entry.valid = false;
            input.setAttribute("aria-invalid", "true");
            const message = error && typeof error === "object" && "message" in error
              ? (error as { message?: unknown }).message
              : undefined;
            setStatus(typeof message === "string" ? message : "This value is invalid.");
            return;
          }
          entry.value = value;
          entry.valid = true;
          input.removeAttribute("aria-invalid");
          setStatus("");
          publish();
        }, { signal: rowController.signal });

        const remove = document.createElement("button");
        remove.type = "button";
        remove.className = "button secondary list-editor-remove";
        remove.textContent = "Remove";
        remove.setAttribute("aria-label", `Remove ${options.label} item ${index + 1}`);
        remove.addEventListener("click", () => {
          entries.splice(index, 1);
          setStatus("");
          renderRows(Math.min(index, entries.length - 1));
          publish();
        }, { signal: rowController.signal });
        item.append(input, remove);
        list.append(item);
      });
      add.disabled = entries.length >= maxItems;
      if (focusIndex !== undefined && focusIndex >= 0) {
        const row = list.children.item(focusIndex);
        row?.querySelector("input")?.focus();
      }
    }

    add.addEventListener("click", () => {
      if (entries.length >= maxItems) {
        setStatus(`This list can contain at most ${maxItems} items.`);
        return;
      }
      entries.push({ text: "", value: undefined, valid: false });
      setStatus("Enter a value for the new item.");
      renderRows(entries.length - 1);
    }, { signal: rootController.signal });

    renderRows();
    return {
      element: root,
      values: currentValues,
      setValues(values) {
        if (disposed) throw new Error("The list editor has been disposed.");
        if (!Array.isArray(values) || values.length > maxItems)
          throw new Error(`The list can contain at most ${maxItems} items.`);
        entries = values.map((value) => ({ text: options.formatValue(value), value, valid: true }));
        setStatus("");
        renderRows();
      },
      dispose() {
        if (disposed) return;
        disposed = true;
        rootController.abort();
        rowController.abort();
        root.replaceChildren();
      },
    };
  }
}
