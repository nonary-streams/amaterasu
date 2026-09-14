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
