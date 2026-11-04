// Example consumer of the additive ui.dialog/1 capability.
window.AmaterasuExtensions.v1.register("dialog", {
  title: "Dialog example",
  mount(container, sdk) {
    if (sdk.version !== 1 || sdk.ui?.version !== 1 ||
        !sdk.features?.includes("ui.dialog/1")) {
      const notice = document.createElement("p");
      notice.textContent = "This host does not support reusable modal dialogs.";
      container.append(notice);
      return;
    }

    const name = document.createElement("input");
    name.type = "text";
    name.setAttribute("aria-label", "Project name");
    const result = document.createElement("p");
    result.setAttribute("role", "status");
    const dialog = sdk.ui.dialog({
      title: { source: "Rename project", translations: { fr: "Renommer le projet" } },
      description: "Enter a project name. Escape or Cancel closes this dialog.",
      content: name,
      initialFocus: name,
      onClose(reason) {
        if (reason === "cancel") result.textContent = "Rename cancelled.";
      },
    });
    const open = document.createElement("button");
    open.type = "button";
    open.textContent = "Rename project";
    open.addEventListener("click", () => dialog.open(), { signal: sdk.signal });
    container.append(open, result, dialog.element);
    return () => dialog.dispose();
  },
});
