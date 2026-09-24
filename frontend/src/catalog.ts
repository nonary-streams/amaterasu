namespace Amaterasu {
  export function renderCatalog(context: PageContext): void {
    const { content, api, notify } = context;
    const search = input("search");
    search.type = "search";
    search.placeholder = "Search names, descriptions, tags";
    search.setAttribute("aria-label", "Search catalog");
    const searchForm = el("form", "search-form", search);
    const searchButton = el("button", "button secondary", "Search");
    searchButton.type = "submit";
    searchForm.append(searchButton);
    const results = el("div");
    const editor = el("div");
    const status = el("div");
    let query = "";
    let revision = 0;
    let cursors: unknown[] = [null];
    let page = 0;
    let generation = 0;
    const add = button("+ Add application", () => showEditor(), "primary");
    add.disabled = !api.allows("catalog.write");
    content.append(
      sectionHeading(
        "Your library",
        "A durable catalog of the applications on your host.",
        add,
      ),
      searchForm,
      status,
      editor,
      results,
    );

    async function load(): Promise<void> {
      const current = ++generation;
      results.setAttribute("aria-busy", "true");
      try {
        const reply = await api.post<CatalogReply>(
          query ? "catalog/search" : "catalog/list",
          { ...(query ? { query } : {}), cursor: cursors[page], limit: 12 },
        );
        if (!content.isConnected || current !== generation) return;
        revision = reply.revision;
        const grid = el("div", "catalog-grid");
        for (const row of reply.entries) grid.append(card(row));
        results.replaceChildren(
          el(
            "div",
            "section-meta",
            el("span", "", `${reply.entries.length} applications on this page`),
            badge(`Revision ${revision}`),
          ),
          reply.entries.length
            ? grid
            : empty(
                query
                  ? "No matches on this page"
                  : "Make room for your favorites",
                reply.next_cursor
                  ? "Search continues on the next page."
                  : query
                    ? "Try a different name or tag."
                    : "Add your first application to start building this host’s library.",
              ),
          pager(
            page
              ? () => {
                  page--;
                  void load();
                }
              : null,
            reply.next_cursor != null
              ? () => {
                  cursors = cursors.slice(0, page + 1);
                  cursors.push(reply.next_cursor);
                  page++;
                  void load();
                }
              : null,
            `Page ${page + 1}`,
          ),
        );
      } catch (error) {
        if (content.isConnected && current === generation)
          results.replaceChildren(errorPanel(error, reset));
      } finally {
        results.removeAttribute("aria-busy");
      }
    }

    function reset(): void {
      cursors = [null];
      page = 0;
      void load();
    }
    searchForm.addEventListener("submit", (event) => {
      event.preventDefault();
      query = search.value.trim();
      reset();
    });

    function card(row: CatalogRow): HTMLElement {
      const providerLaunch = row.entry.launch.kind === "playnite";
      const edit = button("Edit", () => showEditor(row));
      edit.disabled = !api.allows("catalog.write") || providerLaunch;
      if (providerLaunch) edit.title = "Playnite launch targets are managed by the provider.";
      const remove = button(
        "Delete",
        async () => {
          if (
            !(await confirmAction(
              `Delete ${row.entry.name}?`,
              "This removes its catalog entry. It does not remove files or stop a running application.",
              "Delete entry",
            ))
          )
            return;
          await runAction(remove, status, async () => {
            await api.post("catalog/delete", {
              id: row.id,
              expected_revision: revision,
            });
            notify("Application removed from the catalog.");
            reset();
          });
        },
        "quiet danger-text",
      );
      remove.disabled = !api.allows("catalog.write");
      return el(
        "article",
        "app-card",
        el(
          "div",
          "app-art",
          el("span", "", row.entry.name.slice(0, 2).toUpperCase()),
          badge(
            row.entry.launch.kind === "executable"
              ? "Executable"
              : providerLaunch
                ? "Playnite"
                : "Application",
          ),
        ),
        el(
          "div",
          "app-details",
          el("h2", "", row.entry.name),
          el(
            "p",
            "muted description",
            row.entry.description || "No description added.",
          ),
          el("div", "tags", ...(row.entry.tags ?? []).map((tag) => badge(tag))),
          el("p", "path", launchPath(row.entry)),
          el("div", "card-footer", edit, remove),
        ),
      );
    }

    function showEditor(row?: CatalogRow): void {
      const editRevision = revision;
      const form = entryForm(row?.entry);
      const feedback = el("div");
      const save = el(
        "button",
        "button primary",
        row ? "Save changes" : "Add application",
      );
      save.type = "submit";
      const cancel = button("Cancel", () => editor.replaceChildren());
      form.append(feedback, el("div", "actions", save, cancel));
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        if (!form.reportValidity()) return;
        void runAction(save, feedback, async () => {
          const entry = entryFromForm(new FormData(form));
          await api.post(row ? "catalog/update" : "catalog/create", {
            ...(row ? { id: row.id } : {}),
            entry,
            expected_revision: editRevision,
          });
          editor.replaceChildren();
          notify(row ? "Application updated." : "Application added.");
          reset();
        });
      });
      editor.replaceChildren(
        panel(
          row ? "Edit application" : "Add application",
          el(
            "p",
            "muted",
            "Save launch metadata for this host. Adding an entry does not launch it.",
          ),
          form,
        ),
      );
      editor.scrollIntoView({ behavior: "smooth", block: "start" });
      form.querySelector("input")?.focus();
    }
    void load();
  }

  function entryForm(entry?: Entry): HTMLFormElement {
    const form = el("form", "editor-form");
    const name = input("name", entry?.name ?? "");
    name.required = true;
    name.maxLength = 160;
    const description = el("textarea");
    description.name = "description";
    description.value = entry?.description ?? "";
    description.maxLength = 1024;
    description.rows = 2;
    const tags = input("tags", entry?.tags?.join(", ") ?? "");
    const kind = el("select");
    kind.name = "kind";
    for (const [value, text] of [
      ["application", "Application / bundle"],
      ["executable", "Executable with arguments"],
    ]) {
      const option = el("option", "", text);
      option.value = value ?? "";
      kind.append(option);
    }
    kind.value = entry?.launch.kind ?? "application";
    const path = input("path", entry ? launchPath(entry) : "");
    path.required = true;
    path.placeholder = "/Applications/Example.app";
    const executableLaunch =
      entry?.launch.kind === "executable" ? entry.launch : undefined;
    const argumentsField = el("textarea");
    argumentsField.name = "arguments";
    argumentsField.rows = 2;
    argumentsField.value = JSON.stringify(executableLaunch?.arguments ?? []);
    const directory = input(
      "working_directory",
      entry?.launch.kind === "executable"
        ? (entry.launch.working_directory ?? "")
        : "",
    );
    const autoDetach = el("input");
    autoDetach.type = "checkbox";
    autoDetach.name = "auto_detach";
    autoDetach.checked = executableLaunch?.auto_detach ?? true;
    const waitAll = el("input");
    waitAll.type = "checkbox";
    waitAll.name = "wait_all";
    waitAll.checked = executableLaunch?.wait_all ?? true;
    const exitTimeout = input(
      "exit_timeout_seconds",
      String(executableLaunch?.exit_timeout_seconds ?? 5),
      "number",
    );
    exitTimeout.min = "0";
    exitTimeout.max = "300";
    exitTimeout.step = "1";
    const detached = el("textarea");
    detached.name = "detached";
    detached.rows = 3;
    detached.value = executableLaunch?.detached?.join("\n") ?? "";
    const preparation = input(
      "preparation",
      JSON.stringify(executableLaunch?.preparation ?? []),
      "hidden",
    );
    const fec = input(
      "fec_percentage",
      entry?.config_overrides?.fec_percentage?.toString() ?? "",
      "number",
    );
    fec.min = "1";
    fec.max = "255";
    fec.step = "1";
    const processPolicy = el(
      "div",
      "form-grid",
      field("Auto detach", autoDetach, "Retain a quick successful launcher."),
      field("Wait for all processes", waitAll, "Keep the app active while its process group is running."),
      field("Exit timeout (seconds)", exitTimeout, "0 skips the graceful shutdown window; maximum 300."),
      field("Detached commands", detached, "One ordered command per line; up to 16."),
    );
    const executable = el(
      "div",
      "form-grid",
      field(
        "Arguments",
        argumentsField,
        'A JSON array of literal arguments. Example: ["--fullscreen"]',
      ),
      field("Working directory", directory, "Optional absolute path."),
      processPolicy,
      preparation,
    );
    const showKind = () => {
      executable.hidden = kind.value !== "executable";
      path.placeholder =
        kind.value === "executable"
          ? "/opt/games/example"
          : "/Applications/Example.app";
    };
    kind.addEventListener("change", showKind);
    showKind();
    form.append(
      el("div", "form-grid", field("Name", name), field("Launch type", kind)),
      field("Description", description),
      field("Tags", tags, "Separate tags with commas."),
      field("Absolute application path", path),
      field("FEC percentage", fec, "Leave blank to inherit the global streaming setting; supported values are 1–255."),
      executable,
    );
    return form;
  }
}
