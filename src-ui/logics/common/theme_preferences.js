export const THEME_KEY = "vrct-0.theme";
export const THEMES = ["dark", "light", "system"];

export function createThemePreferences({ storage, media, root, events }) {
    const listeners = new Set();
    let preference = "system";
    try {
        const saved = storage.getItem(THEME_KEY);
        if (THEMES.includes(saved)) preference = saved;
    } catch { /* Storage may be unavailable; keep an in-memory preference. */ }

    const apply = () => {
        root.dataset.theme = preference === "system" ? (media.matches ? "dark" : "light") : preference;
    };
    const notify = () => {
        apply();
        listeners.forEach(listener => listener());
    };
    const onStorage = event => {
        if (event.key !== THEME_KEY && event.key !== null) return;
        preference = THEMES.includes(event.newValue) ? event.newValue : "system";
        notify();
    };
    media.addEventListener("change", apply);
    events.addEventListener("storage", onStorage);
    apply();
    return {
        getSnapshot: () => preference,
        subscribe: listener => {
            listeners.add(listener);
            return () => listeners.delete(listener);
        },
        set: value => {
            if (!THEMES.includes(value)) return;
            preference = value;
            try { storage.setItem(THEME_KEY, value); } catch { /* Continue in memory. */ }
            notify();
        },
        dispose: () => {
            media.removeEventListener("change", apply);
            events.removeEventListener("storage", onStorage);
            listeners.clear();
        },
    };
}
