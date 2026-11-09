// Runs before the body paints (a plain script of the app's own: the CSP allows no inline one):
// a theme pinned in the cockpit (light or dark) colours the launch splash too, not only the OS's.
// The key is THEME_KEY in src/lib/theme.ts.
try {
  var pinned = window.localStorage.getItem("mirante.theme");
  if (pinned === "light" || pinned === "dark") document.documentElement.dataset.theme = pinned;
} catch (e) {
  // Storage blocked: the splash follows the OS.
}
