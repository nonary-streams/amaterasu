// Test-only signed web extension fixture for the independent browser qualification.
window.AmaterasuExtensions.v1.register("plugin-ui-qualification", {
  title: "Plugin UI Qualification",
  mount(container, api) {
    window.__pluginUiQualificationMounts = (window.__pluginUiQualificationMounts ?? 0) + 1;
    const panel = document.createElement("section");
    panel.dataset.pluginUiQualification = "true";
    const heading = document.createElement("h2");
    heading.textContent = "Plugin controls";
    const label = document.createElement("label");
    label.textContent = "Strength";
    const slider = document.createElement("input");
    slider.type = "range";
    slider.min = "0";
    slider.max = "100";
    slider.value = "10";
    slider.setAttribute("aria-label", "Strength");
    label.append(slider);
    const greet = document.createElement("button");
    greet.type = "button";
    greet.textContent = "Say hello";
    greet.setAttribute("aria-label", "Say hello");
    const status = document.createElement("p");
    status.setAttribute("role", "status");
    greet.addEventListener("click", async () => {
      try {
        const result = await api.call("greet", { name: "keyboard" });
        if (!api.signal.aborted) status.textContent = result.message;
      } catch {
        if (!api.signal.aborted) status.textContent = "Greeting unavailable.";
      }
    }, { signal: api.signal });
    panel.append(heading, label, greet, status);
    container.replaceChildren(panel);
    return () => {
      window.__pluginUiQualificationCleanups = (window.__pluginUiQualificationCleanups ?? 0) + 1;
      container.replaceChildren();
    };
  },
});
