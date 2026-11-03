namespace Amaterasu {
  // These controls render the shared configuration schema. The provider remains
  // authoritative for bounds, cross-field constraints, and persistence.
  export type ExtensionFieldDefinition = {
    type: string;
    default: unknown;
    inner?: ExtensionFieldDefinition;
    minimum?: number | null;
    maximum?: number | null;
    min_length?: number | null;
    max_length?: number | null;
    min_items?: number | null;
    max_items?: number | null;
    options?: string[];
    item?: ExtensionFieldDefinition;
    fields?: Record<string, ExtensionFieldDefinition>;
    tag?: string;
    variants?: Record<string, Record<string, ExtensionFieldDefinition>>;
  };
  export interface ExtensionFieldEditor {
    node: HTMLElement;
    value(): unknown;
  }

  export function extensionFieldEditor(definition: ExtensionFieldDefinition, value: unknown, choiceLabels: Record<string, string> = {}): ExtensionFieldEditor {
    switch (definition.type) {
      case "nullable": {
        if (!definition.inner) throw new Error("Invalid nullable setting.");
        const inner = definition.inner;
        const group = el("div", "extension-nullable");
        const area = el("div");
        const status = el("p", "muted");
        let editor: ExtensionFieldEditor | null = value === null ? null : extensionFieldEditor(inner, value);
        const add = button("Set value", () => { editor = extensionFieldEditor(inner, structuredClone(inner.default)); render(); });
        const remove = button("Clear value", () => { editor = null; render(); });
        function render(): void {
          status.textContent = editor ? "Value set." : "No value set.";
          add.hidden = editor !== null;
          remove.hidden = editor === null;
          area.replaceChildren(...(editor ? [editor.node] : []));
        }
        group.append(status, area, add, remove);
        render();
        return { node: group, value: () => editor ? editor.value() : null };
      }
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
          const label = choiceLabels[option] ?? option;
          const element = el("option", "", typeof Amaterasu.literalText === "function" ? Amaterasu.literalText(label) : label);
          element.value = option;
          control.append(element);
        }
        if (typeof value === "string") control.value = value;
        return { node: control, value: () => control.value };
      }
      case "tagged_union": {
        if (!definition.tag || !definition.variants || !value || typeof value !== "object" || Array.isArray(value))
          throw new Error("Invalid tagged setting.");
        const tag = definition.tag;
        const variants = definition.variants;
        const initial = value as Record<string, unknown>;
        if (typeof initial[tag] !== "string" || !Object.hasOwn(variants, initial[tag])) throw new Error("Unsupported setting variant.");
        const select = el("select");
        for (const name of Object.keys(variants)) { const option = el("option", "", name); option.value = name; select.append(option); }
        select.value = initial[tag] as string;
        const container = el("div");
        let fields: Array<readonly [string, ExtensionFieldEditor]> = [];
        const render = (existing?: Record<string, unknown>) => {
          const selected = variants[select.value];
          if (!selected) throw new Error("Unsupported setting variant.");
          fields = Object.entries(selected).map(([name, child]) => [name, extensionFieldEditor(child, existing && Object.hasOwn(existing, name) ? existing[name] : structuredClone(child.default))] as const);
          container.replaceChildren(...fields.map(([name, editor]) => el("label", "field", el("span", "field-label", name), editor.node)));
        };
        select.addEventListener("change", () => render());
        render(initial);
        return { node: el("div", "extension-tagged-union", select, container), value: () => ({ [tag]: select.value, ...Object.fromEntries(fields.map(([name, editor]) => [name, editor.value()])) }) };
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

namespace Amaterasu {
  interface BindingKey { platform: string; code: number | null; extended?: boolean; normalized?: boolean }
  interface BindingValue {
    id: string; enabled: boolean; source: string; context: string; trigger: BindingKey;
    modifiers: BindingKey[]; propagation: string; action_id: string;
    arguments: Record<string, unknown>; target: string;
  }

  function bindingChoice(label: string, value: string, choices: Array<[string, string]>): HTMLSelectElement {
    const select = el("select");
    select.setAttribute("aria-label", label);
    if (!value) {
      const placeholder = el("option", "", `Choose ${label.toLowerCase()}`);
      placeholder.value = "";
      select.append(placeholder);
    }
    for (const [id, name] of choices) {
      const option = el("option", "", name);
      option.value = id;
      select.append(option);
    }
    select.value = value;
    return select;
  }

  function bindingKeyEditor(key: BindingKey, label: string): ExtensionFieldEditor {
    if (!key || !["", "linux_evdev", "windows_scan", "moonlight_vk"].includes(key.platform))
      throw new Error("Unsupported key identity.");
    const group = el("fieldset", "extension-key", el("legend", "", label));
    const platform = bindingChoice("Key encoding", key.platform, [
      ["linux_evdev", "Linux evdev (physical)"], ["windows_scan", "Windows scan code (physical)"],
      ["moonlight_vk", "Moonlight virtual key"],
    ]);
    const code = input("", key.code === null ? "" : String(key.code), "number");
    code.min = "1";
    code.max = "65535";
    code.step = "1";
    const flag = input("", "", "checkbox");
    flag.checked = key.platform === "windows_scan" ? key.extended === true : key.normalized === true;
    const flagLabel = el("label", "", flag, el("span"));
    const updateFlag = () => {
      flagLabel.hidden = !platform.value || platform.value === "linux_evdev";
      flagLabel.lastElementChild!.textContent = platform.value === "windows_scan" ? " Extended scan code" : " Normalized Moonlight key";
      code.max = platform.value === "moonlight_vk" ? "255" : "65535";
    };
    platform.addEventListener("change", updateFlag);
    updateFlag();
    group.append(el("label", "field", el("span", "field-label", "Encoding"), platform),
      el("label", "field", el("span", "field-label", "Key code"), code), flagLabel,
      el("small", "muted", "Use the key code for this input provider. Left and right modifiers have separate codes."));
    return { node: group, value: () => {
      const number = Number(code.value);
      if (!code.value || !Number.isInteger(number) || number < 1 || number > Number(code.max)) throw new Error("Enter a supported key code.");
      if (platform.value === "linux_evdev") return { platform: platform.value, code: number };
      if (platform.value === "windows_scan") return { platform: platform.value, code: number, extended: flag.checked };
      if (platform.value === "moonlight_vk") return { platform: platform.value, code: number, normalized: flag.checked };
      throw new Error("Unsupported key encoding.");
    } };
  }

  export function extensionKeybindingEditor(value: unknown): ExtensionFieldEditor {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid keybinding.");
    const binding = value as BindingValue;
    if (!Array.isArray(binding.modifiers) || binding.modifiers.length > 8 || !binding.arguments || typeof binding.arguments !== "object" || Array.isArray(binding.arguments))
      throw new Error("Invalid keybinding.");
    const group = el("fieldset", "extension-keybinding");
    const id = input("", binding.id);
    const context = input("", binding.context);
    const enabled = input("", "", "checkbox"); enabled.checked = binding.enabled;
    const source = bindingChoice("Source", binding.source, [["session_input", "Session input"], ["host_local", "Host local"]]);
    const target = bindingChoice("Target", binding.target, [["current_stream", "Current stream"], ["current_session", "Current session"], ["host", "Host"]]);
    const propagation = bindingChoice("Propagation", binding.propagation, [["forward", "Forward trigger"], ["consume_trigger", "Consume trigger"]]);
    const action = input("", binding.action_id);
    const args = el("textarea"); args.value = JSON.stringify(binding.arguments, null, 2);
    const trigger = bindingKeyEditor(binding.trigger, "Trigger key");
    const modifierContainer = el("div");
    const modifiers: ExtensionFieldEditor[] = [];
    const addModifier = (key: BindingKey) => {
      if (modifiers.length >= 8) return;
      const editor = bindingKeyEditor(key, "Modifier");
      const row = el("div", "extension-modifier", editor.node);
      row.append(button("Remove modifier", () => { const index = modifiers.indexOf(editor); if (index >= 0) modifiers.splice(index, 1); row.remove(); }));
      modifiers.push(editor); modifierContainer.append(row);
    };
    for (const modifier of binding.modifiers) addModifier(modifier);
    group.append(field("Binding ID", id), el("label", "field", enabled, " Enabled"), field("Context", context),
      field("Source", source), field("Target", target), field("Propagation", propagation), field("Action ID", action),
      field("Action arguments (JSON object)", args), trigger.node, modifierContainer,
      button("Add modifier", () => {
        const selected = trigger.value() as BindingKey;
        addModifier(selected.platform === "windows_scan"
          ? { platform: "windows_scan", code: 29, extended: false }
          : selected.platform === "moonlight_vk"
            ? { platform: "moonlight_vk", code: 17, normalized: true }
            : { platform: "linux_evdev", code: 29 });
      }));
    return { node: group, value: () => {
      if (!id.value.trim() || !context.value.trim() || !action.value.trim() || !source.value || !target.value || !propagation.value)
        throw new Error("Complete the binding ID, context, source, target, propagation and action.");
      let parsed: unknown;
      try { parsed = JSON.parse(args.value); } catch { throw new Error("Enter valid action arguments as a JSON object."); }
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error("Action arguments must be a JSON object.");
      const selectedTrigger = trigger.value() as BindingKey;
      const selectedModifiers = modifiers.map((item) => item.value() as BindingKey);
      if (selectedModifiers.some((modifier) => modifier.platform !== selectedTrigger.platform))
        throw new Error("Trigger and modifiers must use the same key encoding.");
      const keys = [selectedTrigger, ...selectedModifiers].map((key) => JSON.stringify(key));
      if (new Set(keys).size !== keys.length) throw new Error("A chord cannot repeat a key.");
      return { id: id.value, enabled: enabled.checked, source: source.value, context: context.value,
        trigger: selectedTrigger, modifiers: selectedModifiers, propagation: propagation.value,
        action_id: action.value, arguments: parsed, target: target.value };
    } };
  }
  /// An unset shortcut has no trigger or action. Creating one exposes blank
  /// fields, and the user must choose a key and action before saving it.
  export function optionalExtensionKeybindingEditor(value: unknown): ExtensionFieldEditor {
    if (value !== null && (!value || typeof value !== "object" || Array.isArray(value)))
      throw new Error("Invalid optional keybinding.");
    const group = el("div", "extension-optional-keybinding");
    const area = el("div");
    const status = el("p", "muted");
    let editor: ExtensionFieldEditor | null = null;
    const add = button("Add shortcut", () => {
      editor = extensionKeybindingEditor({ id: "", enabled: false, source: "", context: "", target: "", propagation: "", action_id: "", arguments: {}, trigger: { platform: "", code: null }, modifiers: [] });
      render();
    });
    const remove = button("Remove shortcut", () => { editor = null; render(); });
    function render(): void {
      status.textContent = editor ? "Configure the shortcut before saving." : "No shortcut configured.";
      add.hidden = editor !== null;
      remove.hidden = editor === null;
      area.replaceChildren(...(editor ? [editor.node] : []));
    }
    if (value !== null) editor = extensionKeybindingEditor(value);
    group.append(status, area, add, remove);
    render();
    return { node: group, value: () => editor ? editor.value() : null };
  }

  /** Choose the editor from the declared schema; never instantiate a nullable
   * shortcut's inner default as a configured binding. */
  export function extensionKeybindingSettingEditor(definition: ExtensionFieldDefinition, value: unknown): ExtensionFieldEditor {
    if (definition.type === "nullable") {
      if (!definition.inner) throw new Error("Invalid nullable setting.");
      return optionalExtensionKeybindingEditor(value);
    }
    return extensionKeybindingEditor(value);
  }

}
