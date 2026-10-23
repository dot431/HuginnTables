import { useEffect, useState } from "react";

export type ThemeId = "classic" | "huginn-light" | "huginn-dark";

export const THEMES: { id: ThemeId; name: string; description: string }[] = [
  { id: "classic", name: "Classic", description: "The original look. Follows the system light or dark setting." },
  { id: "huginn-light", name: "Huginn Light", description: "Bone and mist with fjord teal accents." },
  { id: "huginn-dark", name: "Huginn Dark", description: "Raven ink with teal and ember highlights." },
];

const STORAGE_KEY = "huginn.theme";
const DARK_QUERY = "(prefers-color-scheme: dark)";

function load(): ThemeId {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (THEMES.some((t) => t.id === saved)) return saved as ThemeId;
  } catch {
    // Storage unavailable: fall back to the default.
  }
  return "classic";
}

let current = load();
const listeners = new Set<(id: ThemeId) => void>();

/** Sets `data-theme` on <html>; styles.css keys every theme's variables off it. */
export function applyTheme() {
  document.documentElement.dataset.theme = current;
}

export function setTheme(id: ThemeId) {
  current = id;
  try {
    localStorage.setItem(STORAGE_KEY, id);
  } catch {
    // Not persisted, but still applied for this run.
  }
  applyTheme();
  listeners.forEach((l) => l(id));
}

/** The chosen theme, and whether it currently renders dark (Classic follows the system). */
export function useTheme(): { theme: ThemeId; dark: boolean } {
  const [theme, setThemeState] = useState(current);
  const [systemDark, setSystemDark] = useState(() => window.matchMedia(DARK_QUERY).matches);
  useEffect(() => {
    listeners.add(setThemeState);
    const mq = window.matchMedia(DARK_QUERY);
    const on = () => setSystemDark(mq.matches);
    mq.addEventListener("change", on);
    return () => {
      listeners.delete(setThemeState);
      mq.removeEventListener("change", on);
    };
  }, []);
  return { theme, dark: theme === "huginn-dark" || (theme === "classic" && systemDark) };
}
