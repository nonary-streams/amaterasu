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
    search.placeholder = "Search paired clients";
    search.value = initialQuery;
    search.setAttribute("data-draft-ignore", "");

    const sort = document.createElement("select");
    sort.name = "client_sort";
    sort.value = "recent";
    sort.setAttribute("data-draft-ignore", "");

    const recent = document.createElement("option");
    recent.value = "recent";
    recent.textContent = "Recently paired";
    const name = document.createElement("option");
    name.value = "name";
    name.textContent = "Name";
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
    searchText.textContent = "Search paired clients";
    searchLabel.append(searchText, search);

    const sortLabel = document.createElement("label");
    sortLabel.className = "field";
    const sortText = document.createElement("span");
    sortText.className = "field-label";
    sortText.textContent = "Sort by";
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
      count.textContent = visible.length + " of " + directoryEntries.length + " paired clients";
      empty.hidden = visible.length !== 0;
      empty.textContent = directoryEntries.length === 0
        ? "No paired clients remain."
        : "No paired clients match this search.";

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
