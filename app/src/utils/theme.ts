// Light / dark / system theme, persisted locally. "system" follows the OS/Plasma
// color scheme via the `@media (prefers-color-scheme)` rule in styles.css; an
// explicit choice pins `data-theme` on <html>, which overrides that media query.

export type Theme = "light" | "dark" | "system";

const KEY = "cirrust-theme";

/** The saved preference, defaulting to "system". */
export function getTheme(): Theme {
  const v = localStorage.getItem(KEY);
  return v === "light" || v === "dark" ? v : "system";
}

/** Apply and persist a theme choice. "system" removes the override attribute. */
export function applyTheme(theme: Theme): void {
  const root = document.documentElement;
  if (theme === "system") {
    root.removeAttribute("data-theme");
  } else {
    root.setAttribute("data-theme", theme);
  }
  localStorage.setItem(KEY, theme);
}

/** Apply the saved theme on startup (call before mount to avoid a flash). */
export function initTheme(): void {
  applyTheme(getTheme());
}
