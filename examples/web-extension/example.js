// Signed extensions execute as trusted JavaScript in the management page.
window.AmaterasuExtensions.v1.register('hello', {
  title: 'Hello',
  mount(container, api) {
    const button = document.createElement('button');
    const message = document.createElement('p');
    button.textContent = 'Say hello';
    button.addEventListener('click', async () => {
      try {
        const result = await api.call('greet', { name: 'Amaterasu' });
        if (!api.signal.aborted) message.textContent = result.message;
      } catch {
        if (!api.signal.aborted) message.textContent = 'Greeting unavailable for this account.';
      }
    }, { signal: api.signal });
    container.replaceChildren(button, message);
  },
});
