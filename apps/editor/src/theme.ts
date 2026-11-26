// Dark is the default; light is a full theme; "system" follows the operating system.

export type Theme = "dark" | "light" | "system";
const KEY = "renderly:theme";

export function savedTheme(): Theme {
  try {
    const value = localStorage.getItem(KEY);
    return value === "light" || value === "system" ? value : "dark";
  } catch {
    return "dark";
  }
}

export function applyTheme(theme: Theme): void {
  if (theme === "system") document.documentElement.removeAttribute("data-theme");
  else document.documentElement.dataset["theme"] = theme;
  try {
    localStorage.setItem(KEY, theme);
  } catch {
    // private mode: the choice lasts for this page only
  }
}

/** The theme on screen now: the chosen one, or the operating system's for "system". */
export function resolvedTheme(): "dark" | "light" {
  const set = document.documentElement.dataset["theme"];
  if (set === "light" || set === "dark") return set;
  return matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

/** Calls `listener` whenever the theme on screen changes. Returns a function that stops it. */
export function onThemeChange(listener: () => void): () => void {
  const watch = new MutationObserver(listener);
  watch.observe(document.documentElement, { attributeFilter: ["data-theme"] });
  const scheme = matchMedia("(prefers-color-scheme: light)");
  scheme.addEventListener("change", listener);
  return () => {
    watch.disconnect();
    scheme.removeEventListener("change", listener);
  };
}
