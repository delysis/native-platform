(() => {
  const root = document.documentElement;
  let stored = null;
  try {
    stored = window.localStorage.getItem('loom.appearance.v1');
  } catch {
    // Unavailable renderer storage has the same semantics as no preference.
  }
  const preference = stored === 'light' || stored === 'dark' ? stored : 'system';
  let systemDark = false;
  if (preference === 'system') {
    try {
      systemDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
    } catch {
      // A missing media-query implementation fails safely to light.
    }
  }
  const resolved = preference === 'dark' || (preference === 'system' && systemDark)
    ? 'dark'
    : 'light';
  root.dataset.theme = resolved;
  root.style.colorScheme = resolved;
  const themeColor = document.querySelector('meta[name="theme-color"]');
  if (themeColor) themeColor.content = resolved === 'dark' ? '#222420' : '#f4f0e8';
})();
