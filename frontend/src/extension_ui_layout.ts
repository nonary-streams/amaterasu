namespace Amaterasu {
  export interface ExtensionUiCardOptions {
    label: ExtensionUiText;
  }

  export interface ExtensionUiCard {
    readonly element: HTMLElement;
    readonly content: HTMLElement;
    append(child: Node): void;
    dispose(): void;
  }

  export interface ExtensionUiTableColumn {
    header: ExtensionUiText;
  }

  export interface ExtensionUiTableOptions {
    caption: ExtensionUiText;
    columns: readonly ExtensionUiTableColumn[];
    rows?: readonly (readonly ExtensionUiText[])[];
  }

  export interface ExtensionUiTable {
    readonly element: HTMLElement;
    setRows(rows: readonly (readonly ExtensionUiText[])[]): void;
    dispose(): void;
  }

  export interface ExtensionUiLayoutV1 {
    readonly version: 1;
    card(options: ExtensionUiCardOptions): ExtensionUiCard;
    table(options: ExtensionUiTableOptions): ExtensionUiTable;
  }

  let nextExtensionUiLayoutId = 0;
  const EXTENSION_UI_TABLE_MAX_COLUMNS = 32;
  const EXTENSION_UI_TABLE_MAX_ROWS = 1000;

  function extensionUiLayoutLifecycle(signal: AbortSignal): void {
    if (!signal || typeof signal.addEventListener !== "function" || typeof signal.removeEventListener !== "function")
      throw new Error("Plugin UI requires a lifecycle signal.");
    if (signal.aborted) throw new Error("Plugin page is no longer active.");
  }

  function extensionUiLayoutText(value: ExtensionUiText, locale: string): string {
    if (typeof value === "string") return translateText(value);
    if (!value || typeof value !== "object" || Array.isArray(value) || typeof value.source !== "string" || !value.source.trim())
      throw new Error("Invalid plugin UI text.");
    const translations = value.translations ?? {};
    if (!translations || typeof translations !== "object" || Array.isArray(translations) ||
        Object.values(translations).some((translation) => typeof translation !== "string" || !translation.trim()))
      throw new Error("Invalid plugin UI translations.");
    return translations[locale] ?? translations[locale.split("-")[0] ?? locale] ?? translateText(value.source);
  }

  function extensionUiLayoutTextNode(value: ExtensionUiText): Text | string {
    if (typeof value === "string") return localizedTextNode(value);
    extensionUiLayoutText(value, activeIntlLocale());
    return localeFormattedTextNode((locale) => extensionUiLayoutText(value, locale));
  }

  function extensionUiLayoutRequiredText(value: unknown, description: string): asserts value is ExtensionUiText {
    if (typeof value === "string") {
      if (!value.trim()) throw new Error(`Invalid plugin UI ${description}.`);
      return;
    }
    extensionUiLayoutText(value as ExtensionUiText, activeIntlLocale());
  }

  function extensionUiLayoutNode(value: unknown): value is Node {
    if (!value || typeof value !== "object" || typeof (value as Node).nodeType !== "number") return false;
    const supportedNodeTypes: readonly number[] = [Node.ELEMENT_NODE, Node.TEXT_NODE, Node.COMMENT_NODE, Node.DOCUMENT_FRAGMENT_NODE];
    return supportedNodeTypes.includes((value as Node).nodeType);
  }

  function extensionUiLayoutSetLabelAttribute(element: Element, label: ExtensionUiText): void {
    if (typeof label === "string") Amaterasu.setLocalizedAttribute(element, "aria-label", label);
    else Amaterasu.setLocalizedAttributeFormatter(element, "aria-label", (locale) => extensionUiLayoutText(label, locale));
  }

  function createExtensionUiCard(signal: AbortSignal, options: ExtensionUiCardOptions): ExtensionUiCard {
    extensionUiLayoutLifecycle(signal);
    if (!options || typeof options !== "object" || Array.isArray(options)) throw new Error("Invalid plugin UI card.");
    extensionUiLayoutRequiredText(options.label, "card label");

    const id = `amaterasu-plugin-card-${++nextExtensionUiLayoutId}`;
    const heading = el("h2", "", extensionUiLayoutTextNode(options.label));
    heading.id = `${id}-heading`;
    const content = el("div", "plugin-ui-card-content");
    const element = el("section", "panel plugin-ui-card", heading, content);
    element.setAttribute("aria-labelledby", heading.id);
    let disposed = false;
    const dispose = (): void => {
      if (disposed) return;
      disposed = true;
      content.replaceChildren();
      element.remove();
      signal.removeEventListener("abort", dispose);
    };
    const card: ExtensionUiCard = Object.freeze({
      element,
      content,
      append: (child: Node): void => {
        if (disposed || signal.aborted) return;
        if (!extensionUiLayoutNode(child)) throw new Error("Plugin UI card children must be DOM nodes.");
        content.append(child);
      },
      dispose,
    });
    signal.addEventListener("abort", dispose, { once: true });
    return card;
  }

  function validateExtensionUiTableRows(rows: unknown, columnCount: number): asserts rows is readonly (readonly ExtensionUiText[])[] {
    if (!Array.isArray(rows) || rows.length > EXTENSION_UI_TABLE_MAX_ROWS ||
        rows.some((row) => !Array.isArray(row) || row.length !== columnCount))
      throw new Error("Invalid plugin UI table rows.");
    for (const row of rows as readonly (readonly ExtensionUiText[])[]) {
      for (const value of row) {
        if (typeof value !== "string") extensionUiLayoutText(value, activeIntlLocale());
      }
    }
  }

  function createExtensionUiTable(signal: AbortSignal, options: ExtensionUiTableOptions): ExtensionUiTable {
    extensionUiLayoutLifecycle(signal);
    if (!options || typeof options !== "object" || Array.isArray(options)) throw new Error("Invalid plugin UI table.");
    extensionUiLayoutRequiredText(options.caption, "table caption");
    if (!Array.isArray(options.columns) || options.columns.length < 1 || options.columns.length > EXTENSION_UI_TABLE_MAX_COLUMNS)
      throw new Error("Invalid plugin UI table columns.");
    for (const column of options.columns) {
      if (!column || typeof column !== "object" || Array.isArray(column)) throw new Error("Invalid plugin UI table column.");
      extensionUiLayoutRequiredText(column.header, "table header");
    }
    const columnCount = options.columns.length;
    const initialRows = options.rows === undefined ? [] : options.rows;
    validateExtensionUiTableRows(initialRows, columnCount);

    const region = el("div", "table-scroll plugin-ui-table-scroll");
    region.setAttribute("role", "region");
    region.tabIndex = 0;
    extensionUiLayoutSetLabelAttribute(region, options.caption);
    const table = el("table", "plugin-ui-table");
    table.append(el("caption", "", extensionUiLayoutTextNode(options.caption)));
    const head = el("thead", "");
    const headerRow = el("tr", "");
    for (const column of options.columns) {
      const header = el("th", "", extensionUiLayoutTextNode(column.header));
      header.setAttribute("scope", "col");
      headerRow.append(header);
    }
    head.append(headerRow);
    const body = el("tbody", "");
    table.append(head, body);
    region.append(table);

    let disposed = false;
    const renderRows = (rows: readonly (readonly ExtensionUiText[])[]): void => {
      const preparedRows = rows.map((row) => {
        const tr = el("tr", "");
        for (const value of row) tr.append(el("td", "", extensionUiLayoutTextNode(value)));
        return tr;
      });
      body.replaceChildren(...preparedRows);
    };
    renderRows(initialRows);
    const dispose = (): void => {
      if (disposed) return;
      disposed = true;
      table.replaceChildren();
      region.remove();
      signal.removeEventListener("abort", dispose);
    };
    const component: ExtensionUiTable = Object.freeze({
      element: region,
      setRows: (rows: readonly (readonly ExtensionUiText[])[]): void => {
        if (disposed || signal.aborted) return;
        validateExtensionUiTableRows(rows, columnCount);
        renderRows(rows);
      },
      dispose,
    });
    signal.addEventListener("abort", dispose, { once: true });
    return component;
  }

  export function createExtensionUiLayoutV1(signal: AbortSignal): ExtensionUiLayoutV1 {
    extensionUiLayoutLifecycle(signal);
    return Object.freeze({
      version: 1 as const,
      card: (options: ExtensionUiCardOptions) => createExtensionUiCard(signal, options),
      table: (options: ExtensionUiTableOptions) => createExtensionUiTable(signal, options),
    });
  }
}
