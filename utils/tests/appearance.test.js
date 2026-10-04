import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import yaml from "js-yaml";
import i18next from "i18next";
import { createThemePreferences, THEME_KEY } from "../../src-ui/logics/common/theme_preferences.js";

const readLocale = lang => yaml.load(readFileSync(new URL(`../../locales/${lang}.yml`, import.meta.url), "utf8"));
function flatten(value, prefix = "", result = {}) {
    for (const [key, item] of Object.entries(value)) {
        const path = prefix ? `${prefix}.${key}` : key;
        if (typeof item === "object") flatten(item, path, result);
        else result[path] = item;
    }
    return result;
}

test("Thai covers every English key and preserves interpolation variables", async () => {
    const en = flatten(readLocale("en"));
    const th = flatten(readLocale("th"));
    assert.deepEqual(Object.keys(th).sort(), Object.keys(en).sort());
    const placeholders = value => (value.match(/{{[^}]+}}/g) || []).sort();
    for (const [key, value] of Object.entries(en)) {
        assert.equal(typeof th[key], "string", key);
        assert.ok(th[key].length > 0, key);
        assert.deepEqual(placeholders(th[key]), placeholders(value), key);
    }
    const instance = i18next.createInstance();
    await instance.init({ lng: "th", fallbackLng: "en", resources: {
        en: { translation: readLocale("en") }, th: { translation: readLocale("th") },
    } });
    assert.equal(instance.t("config_page.appearance.theme.light"), "สว่าง");
    assert.equal(instance.t("config_page.common.version", { version: "3.5.1" }), "เวอร์ชัน 3.5.1");
    assert.equal(instance.t("common_error.threshold_invalid_value", { min: 0, max: 100 }), "ระบุค่าระหว่าง 0 ถึง 100");
});

test("existing UI languages have translated theme choices", () => {
    for (const lang of ["en", "ja", "ko", "zh-Hant", "zh-Hans", "th"]) {
        const appearance = readLocale(lang).config_page.appearance;
        for (const key of ["label", "desc", "dark", "light", "system"]) assert.ok(appearance.theme[key], `${lang}.${key}`);
    }
});

function harness(saved, dark = false, blocked = false) {
    const changes = new EventTarget();
    const events = new EventTarget();
    const values = new Map(saved === undefined ? [] : [[THEME_KEY, saved]]);
    const root = { dataset: {} };
    const media = Object.assign(changes, { matches: dark });
    const storage = {
        getItem: key => { if (blocked) throw Error("denied"); return values.get(key) ?? null; },
        setItem: (key, value) => { if (blocked) throw Error("denied"); values.set(key, value); },
    };
    return { root, media, events, storage, values,
        preferences: createThemePreferences({ root, media, events, storage }),
        changeSystem: value => { media.matches = value; media.dispatchEvent(new Event("change")); },
    };
}

test("default System follows OS changes; explicit themes override OS", () => {
    const h = harness();
    assert.equal(h.preferences.getSnapshot(), "system");
    assert.equal(h.root.dataset.theme, "light");
    h.changeSystem(true);
    assert.equal(h.root.dataset.theme, "dark");
    h.preferences.set("light");
    h.changeSystem(true);
    assert.equal(h.root.dataset.theme, "light");
    assert.equal(h.values.get(THEME_KEY), "light");
    h.preferences.set("system");
    assert.equal(h.root.dataset.theme, "dark");
    h.changeSystem(false);
    assert.equal(h.root.dataset.theme, "light");
    h.preferences.dispose();
});

test("saved theme survives recreation, malformed values fall back, invalid sets are ignored", () => {
    const h = harness("dark");
    assert.equal(h.root.dataset.theme, "dark");
    h.preferences.set("light");
    const restored = createThemePreferences(h);
    assert.equal(restored.getSnapshot(), "light");
    h.preferences.set("bogus");
    assert.equal(h.preferences.getSnapshot(), "light");
    assert.equal(harness("bogus", true).preferences.getSnapshot(), "system");
    restored.dispose();
    h.preferences.dispose();
});

test("blocked storage keeps theme usable in memory", () => {
    const h = harness("dark", false, true);
    assert.equal(h.preferences.getSnapshot(), "system");
    h.preferences.set("dark");
    assert.equal(h.preferences.getSnapshot(), "dark");
    assert.equal(h.root.dataset.theme, "dark");
    h.preferences.dispose();
});

test("storage changes notify subscribers; unsubscribe and dispose clean up", () => {
    const h = harness();
    let notified = 0;
    const unsubscribe = h.preferences.subscribe(() => notified++);
    h.preferences.set("dark");
    const storageEvent = value => Object.assign(new Event("storage"), { key: THEME_KEY, newValue: value });
    h.events.dispatchEvent(storageEvent("light"));
    assert.equal(h.preferences.getSnapshot(), "light");
    assert.equal(h.root.dataset.theme, "light");
    assert.equal(notified, 2);
    unsubscribe();
    h.events.dispatchEvent(storageEvent(null));
    assert.equal(h.preferences.getSnapshot(), "system");
    assert.equal(notified, 2);
    h.preferences.dispose();
    h.events.dispatchEvent(storageEvent("dark"));
    assert.equal(h.preferences.getSnapshot(), "system");
});
