// Signed, provider-agnostic consumer qualification for additive UI primitives.
(() => {
  const required = ["ui.dialog/1", "ui.tabs/1", "ui.layout/1", "ui.feedback/1"];
  const mount = (container, sdk) => {
    if (sdk?.version !== 1 || sdk?.ui?.version !== 1 ||
        required.some((feature) => !sdk.features?.includes(feature))) {
      const fallback = document.createElement("p");
      fallback.dataset.uiPrimitiveFallback = "true";
      fallback.textContent = "This host does not support all reusable UI primitives.";
      container.append(fallback);
      return () => fallback.remove();
    }
    const section = document.createElement("section");
    section.dataset.signedUiPrimitives = "true";
    section.dataset.detectedFeatures = required.join(",");
    const openButton = document.createElement("button");
    openButton.type = "button";
    openButton.textContent = "Open qualification dialog";
    const dialogInput = document.createElement("input");
    dialogInput.type = "text";
    dialogInput.setAttribute("aria-label", "Qualification value");
    const dialog = sdk.ui.dialog({
      title: { source: "Qualification dialog", translations: { fr: "Dialogue de qualification" } },
      description: "Dialog description for accessibility checks.",
      content: dialogInput,
      initialFocus: dialogInput,
      onClose(reason) { window.__signedUiDialogCloseReason = reason; },
    });
    const summary = document.createElement("p");
    summary.textContent = "Summary panel content";
    const details = document.createElement("p");
    details.textContent = "Details panel content";
    const tabs = sdk.ui.tabs({
      label: { source: "Qualification sections", translations: { fr: "Sections de qualification" } },
      tabs: [
        { id: "summary", label: { source: "Summary", translations: { fr: "Résumé" } }, content: summary },
        { id: "details", label: { source: "Details", translations: { fr: "Détails" } }, content: details },
      ],
    });
    const card = sdk.ui.card({ label: { source: "Metrics card", translations: { fr: "Carte des mesures" } } });
    const cardText = document.createElement("p");
    cardText.textContent = "Semantic card content";
    card.append(cardText);
    const table = sdk.ui.table({
      caption: { source: "Qualification results", translations: { fr: "Résultats de qualification" } },
      columns: [{ header: { source: "Name", translations: { fr: "Nom" } } }, { header: "Value" }],
      rows: [["alpha", "<img src=x onerror=alert(1)>"]],
    });
    const status = sdk.ui.status({ message: { source: "Ready", translations: { fr: "Prêt" } }, busy: false });
    const alert = sdk.ui.alert({ message: "Qualification warning", severity: "warning" });
    const progress = sdk.ui.progress({ label: "Qualification progress", value: 1, message: "Complete" });
    openButton.addEventListener("click", () => dialog.open(), { signal: sdk.signal });
    section.append(openButton, tabs.element, card.element, table.element,
      status.element, alert.element, progress.element, dialog.element);
    container.append(section);
    window.__signedUiPrimitiveHandles = { dialog, tabs, card, table, status, alert, progress };
    window.__signedUiPrimitiveLifecycle = { aborted: false };
    sdk.signal.addEventListener("abort", () => { window.__signedUiPrimitiveLifecycle.aborted = true; }, { once: true });
    return () => {
      dialog.dispose(); tabs.dispose(); card.dispose(); table.dispose();
      status.dispose(); alert.dispose(); progress.dispose(); section.remove();
    };
  };
  window.__signedUiPrimitiveQualification = Object.freeze({ required, mount });
  window.AmaterasuExtensions.v1.register("signed-ui-primitives", {
    title: "Signed UI Primitives Qualification",
    mount,
  });
})();
