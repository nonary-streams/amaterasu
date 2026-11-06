// Package with application-ui.json and the existing greeting example provider.
window.AmaterasuExtensions.v1.register('hello', {
  title: 'Application tools',
  mountApplication(container, sdk) {
    if (!sdk.features?.includes('ui.application-section/1')) {
      throw new Error('This host does not support application plugin sections.');
    }
    const heading = document.createElement('p');
    heading.textContent = `Tools for ${sdk.application.name}`;
    const result = document.createElement('p');
    result.setAttribute('role', 'status');
    const message = sdk.ui.textField({
      label: 'Message', value: '', maxLength: 64,
      onChange() { sdk.setDirty(true); },
    });
    const send = sdk.ui.button({
      label: 'Send to plugin', variant: 'primary',
      async onClick() {
        sdk.setBusy(true);
        try {
          const response = await sdk.call('greet', {
            name: message.value() || sdk.application.name.slice(0, 64),
          });
          if (!sdk.signal.aborted) {
            result.textContent = response.message;
            sdk.setDirty(false);
          }
        } catch (error) {
          if (!sdk.signal.aborted) result.textContent = error.message;
        } finally {
          sdk.setBusy(false);
        }
      },
    });
    container.replaceChildren(heading, message.element, send.element, result);
    return () => { message.dispose(); send.dispose(); };
  },
});
