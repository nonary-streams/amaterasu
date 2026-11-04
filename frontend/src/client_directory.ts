namespace Amaterasu {
  export interface ClientDirectoryEntry {
    key: string;
    name: string;
    searchText: string;
    pairedAt: number;
    card: HTMLElement;
  }

  export interface ClientDirectoryView {
    element: HTMLElement;
    search: HTMLInputElement;
    remove: (key: string) => void;
  }

  export type ClientDirectorySort = "recent" | "name";

  function clientDirectoryTextNode(source: string): Text | string {
    return typeof Amaterasu.localizedTextNode === "function"
      ? Amaterasu.localizedTextNode(source)
      : source;
  }

  function clientDirectorySetText(target: Element, source: string): void {
    if (typeof Amaterasu.setLocalizedTextContent === "function")
      Amaterasu.setLocalizedTextContent(target, source);
    else target.textContent = source;
  }

  function clientDirectorySetPlaceholder(target: Element, source: string): void {
    if (typeof Amaterasu.setLocalizedAttribute === "function")
      Amaterasu.setLocalizedAttribute(target, "placeholder", source);
    else target.setAttribute("placeholder", source);
  }

  function clientDirectoryCountNode(
    visible: number,
    total: number,
  ): Text | string {
    const format = (locale: string): string => {
      const source = total === 1
        ? "Showing {visible} of {total} paired client"
        : "Showing {visible} of {total} paired clients";
      const message = typeof Amaterasu.translateText === "function"
        ? Amaterasu.translateText(source)
        : source;
      return message
        .replaceAll("{visible}", new Intl.NumberFormat(locale).format(visible))
        .replaceAll("{total}", new Intl.NumberFormat(locale).format(total));
    };
    return typeof Amaterasu.localeFormattedTextNode === "function"
      ? Amaterasu.localeFormattedTextNode(format)
      : format("en");
  }

  export function sortClientDirectoryEntries(
    entries: readonly ClientDirectoryEntry[],
    query: string,
    sort: ClientDirectorySort,
    locale?: string,
  ): ClientDirectoryEntry[] {
    const needle = query.trim().toLocaleLowerCase(locale);
    const collator = new Intl.Collator(locale, { sensitivity: "base", numeric: true });
    return entries
      .filter((entry) => !needle || entry.searchText.toLocaleLowerCase(locale).includes(needle))
      .slice()
      .sort((left, right) => {
        if (sort === "recent") {
          const leftDate = Number.isSafeInteger(left.pairedAt) ? left.pairedAt : 0;
          const rightDate = Number.isSafeInteger(right.pairedAt) ? right.pairedAt : 0;
          if (leftDate !== rightDate) return rightDate - leftDate;
        }
        return collator.compare(left.name, right.name) || left.key.localeCompare(right.key);
      });
  }

  export function createClientDirectory(
    entries: readonly ClientDirectoryEntry[],
    initialQuery = "",
    locale?: string,
  ): ClientDirectoryView {
    let directoryEntries = [...entries];
    const search = document.createElement("input");
    search.type = "search";
    search.name = "client_search";
    search.value = initialQuery;
    search.setAttribute("data-draft-ignore", "");
    clientDirectorySetPlaceholder(search, "Search paired clients");

    const sort = document.createElement("select");
    sort.name = "client_sort";
    sort.value = "recent";
    sort.setAttribute("data-draft-ignore", "");

    const recent = document.createElement("option");
    recent.value = "recent";
    recent.append(clientDirectoryTextNode("Recently paired"));
    const name = document.createElement("option");
    name.value = "name";
    name.append(clientDirectoryTextNode("Name"));
    sort.append(recent, name);

    const count = document.createElement("p");
    count.className = "muted client-search-summary";
    count.setAttribute("role", "status");
    count.setAttribute("aria-live", "polite");
    count.setAttribute("aria-atomic", "true");

    const empty = document.createElement("p");
    empty.className = "muted client-search-empty";
    const list = document.createElement("div");
    list.className = "client-settings-list";

    const searchLabel = document.createElement("label");
    searchLabel.className = "field";
    const searchText = document.createElement("span");
    searchText.className = "field-label";
    searchText.append(clientDirectoryTextNode("Search paired clients"));
    searchLabel.append(searchText, search);

    const sortLabel = document.createElement("label");
    sortLabel.className = "field";
    const sortText = document.createElement("span");
    sortText.className = "field-label";
    sortText.append(clientDirectoryTextNode("Sort by"));
    sortLabel.append(sortText, sort);

    const controls = document.createElement("div");
    controls.className = "client-search";
    controls.append(searchLabel, sortLabel, count);

    const update = (): void => {
      const visible = sortClientDirectoryEntries(
        directoryEntries,
        search.value,
        sort.value as ClientDirectorySort,
        locale,
      );
      count.replaceChildren(clientDirectoryCountNode(visible.length, directoryEntries.length));
      empty.hidden = visible.length !== 0;
      clientDirectorySetText(
        empty,
        directoryEntries.length === 0
          ? "No paired clients"
          : "No paired clients match this search.",
      );

      for (const entry of directoryEntries) entry.card.hidden = !visible.includes(entry);
      list.replaceChildren(...visible.map((entry) => entry.card));
      for (const entry of directoryEntries) {
        if (!visible.includes(entry)) list.append(entry.card);
      }
    };

    search.addEventListener("input", update);
    sort.addEventListener("change", update);
    update();

    const element = document.createElement("section");
    element.append(controls, empty, list);
    return {
      element,
      search,
      remove: (key) => {
        directoryEntries = directoryEntries.filter((entry) => entry.key !== key);
        update();
      },
    };
  }
}
