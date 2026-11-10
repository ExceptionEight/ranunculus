// The theme chosen in this browser, on <html> before the page draws (src/shared/theme.js keeps it after).
try {
  var t = localStorage.getItem('ranunculus.theme')
  if (t === 'light' || t === 'dark') document.documentElement.dataset.theme = t
} catch {
  /* private mode: the system's */
}
