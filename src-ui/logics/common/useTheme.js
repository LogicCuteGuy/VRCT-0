import { useSyncExternalStore } from "react";
import { createThemePreferences } from "./theme_preferences";

// A getter keeps blocked localStorage access inside the preferences' try/catch.
export const themePreferences = createThemePreferences({
    storage: {
        getItem: key => window.localStorage.getItem(key),
        setItem: (key, value) => window.localStorage.setItem(key, value),
    },
    media: window.matchMedia("(prefers-color-scheme: dark)"),
    root: document.documentElement,
    events: window,
});

export const useTheme = () => ({
    theme: useSyncExternalStore(themePreferences.subscribe, themePreferences.getSnapshot),
    setTheme: themePreferences.set,
});

if (import.meta.hot) import.meta.hot.dispose(() => themePreferences.dispose());
