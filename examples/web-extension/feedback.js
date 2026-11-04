// Example consumer of the additive ui.feedback/1 capability.
window.AmaterasuExtensions.v1.register("feedback", {
  title: "Feedback example",
  mount(container, sdk) {
    if (sdk.version !== 1 || sdk.ui?.version !== 1 ||
        !sdk.features?.includes("ui.feedback/1")) {
      const notice = document.createElement("p");
      notice.textContent = "This host does not support reusable feedback controls.";
      container.append(notice);
      return;
    }

    const status = sdk.ui.status({
      message: { source: "Preparing upload…", translations: { fr: "Préparation du téléversement…" } },
      busy: true,
    });
    const alert = sdk.ui.alert({
      message: "The upload permission expired. Reconnect before retrying.",
      severity: "error",
    });
    const progress = sdk.ui.progress({
      label: "Upload progress",
      value: null,
      message: "Waiting for the server to report progress.",
    });

    container.append(status.element, alert.element, progress.element);

    // Demonstrate switching from indeterminate to bounded progress.
    progress.setValue(0);
    progress.setMessage("Uploading 3 files.");
    status.setMessage("Upload started.");
    status.setBusy(false);
    status.setMessage("Upload queued successfully.");

    return () => {
      status.dispose();
      alert.dispose();
      progress.dispose();
    };
  },
});

