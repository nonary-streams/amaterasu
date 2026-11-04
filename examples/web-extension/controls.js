// A minimal independently authored plugin page consuming the public UI SDK.
window.AmaterasuExtensions.v1.register("controls", {
  title: "Controls example",
  mount(container, sdk) {
    if (!sdk.features?.includes("ui.controls/1") || sdk.ui?.version !== 1) {
      const notice = document.createElement("p");
      notice.textContent = "This host does not support the reusable controls in this page.";
      container.append(notice);
      return;
    }
    const enabled = sdk.ui.toggle({ label: "Enable detail", value: true });
    const style = sdk.ui.radioGroup({
      label: { source: "Rendering style", translations: { fr: "Style de rendu" } },
      choices: [
        { value: "balanced", label: "Balanced" },
        { value: "sharp", label: "Sharp" },
      ],
      value: "balanced",
    });
    container.append(enabled.element, style.element);
    return () => {
      enabled.dispose();
      style.dispose();
    };
  },
});
