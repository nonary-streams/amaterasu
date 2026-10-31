namespace Amaterasu {
  // These controls render the shared configuration schema. The provider remains
  // authoritative for bounds, cross-field constraints, and persistence.
  export type ExtensionFieldDefinition = {
    type: string;
    default: unknown;
    minimum?: number | null;
    maximum?: number | null;
    min_length?: number | null;
    max_length?: number | null;
    min_items?: number | null;
    max_items?: number | null;
    options?: string[];
    item?: ExtensionFieldDefinition;
    fields?: Record<string, ExtensionFieldDefinition>;
  };
  export interface ExtensionFieldEditor {
    node: HTMLElement;
    value(): unknown;
  }

  export function extensionFieldEditor(definition: ExtensionFieldDefinition, value: unknown): ExtensionFieldEditor {
    switch (definition.type) {
      case "boolean": {
        const control = input("", "", "checkbox");
        control.checked = value === true;
        return { node: control, value: () => control.checked };
      }
      case "integer":
      case "decimal": {
        const control = input("", String(value), "number");
        control.step = definition.type === "integer" ? "1" : "any";
        if (definition.minimum != null) control.min = String(definition.minimum);
        if (definition.maximum != null) control.max = String(definition.maximum);
        return { node: control, value: () => {
          if (!control.value || !control.validity.valid) throw new Error("Enter a valid number.");
          const number = Number(control.value);
          if (!Number.isFinite(number) || (definition.type === "integer" && !Number.isSafeInteger(number)))
            throw new Error("Enter a valid number.");
          return number;
        } };
      }
      case "string": {
        const control = input("", typeof value === "string" ? value : "");
        if (definition.max_length != null) control.maxLength = definition.max_length;
        return { node: control, value: () => control.value };
      }
      case "enum": {
        const control = el("select");
        for (const option of definition.options ?? []) {
          const element = el("option", "", option);
          element.value = option;
          control.append(element);
        }
        if (typeof value === "string") control.value = value;
        return { node: control, value: () => control.value };
      }
      case "object": {
        if (!definition.fields || !value || typeof value !== "object" || Array.isArray(value))
          throw new Error("Invalid object setting.");
        const group = el("fieldset", "extension-object");
        const editors = Object.entries(definition.fields).map(([name, child]) => {
          const editor = extensionFieldEditor(child, (value as Record<string, unknown>)[name]);
          const label = el("label", "field", el("span", "field-label", name), editor.node);
          group.append(label);
          return [name, editor] as const;
        });
        return { node: group, value: () => Object.fromEntries(editors.map(([name, editor]) => [name, editor.value()])) };
      }
      case "list": {
        if (!definition.item || !Array.isArray(value)) throw new Error("Invalid list setting.");
        const itemDefinition = definition.item;
        const group = el("div", "extension-list");
        const rows = el("div");
        const editors: ExtensionFieldEditor[] = [];
        const append = (itemValue: unknown) => {
          const editor = extensionFieldEditor(itemDefinition, itemValue);
          const row = el("div", "extension-list-item", editor.node);
          const remove = button("Remove", () => {
            const index = editors.indexOf(editor);
            if (index >= 0) editors.splice(index, 1);
            row.remove();
          });
          row.append(remove);
          editors.push(editor);
          rows.append(row);
        };
        for (const item of value) append(item);
        group.append(rows, button("Add", () => append(structuredClone(itemDefinition.default))));
        return { node: group, value: () => editors.map((editor) => editor.value()) };
      }
      default:
        throw new Error(`Unsupported setting type: ${definition.type}`);
    }
  }
}
