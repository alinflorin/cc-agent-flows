import i18n from "i18next";
import HttpBackend from "i18next-http-backend";
import { initReactI18next } from "react-i18next";
import { getPref, setPref } from "./prefs";

export const SUPPORTED_LANGUAGES = ["en", "ro"] as const;
export type Language = (typeof SUPPORTED_LANGUAGES)[number];

export const LANGUAGE_NAMES: Record<Language, string> = { en: "English", ro: "Română" };

const STORAGE_KEY = "language";

function detectLanguage(): Language {
  const saved = getPref<string>(STORAGE_KEY);
  if (saved && (SUPPORTED_LANGUAGES as readonly string[]).includes(saved)) {
    return saved as Language;
  }
  const candidates = navigator.languages?.length ? navigator.languages : [navigator.language];
  for (const lang of candidates) {
    const base = lang.toLowerCase().split("-")[0];
    if ((SUPPORTED_LANGUAGES as readonly string[]).includes(base)) {
      return base as Language;
    }
  }
  return "en";
}

export function setLanguage(lang: Language) {
  setPref(STORAGE_KEY, lang);
  return i18n.changeLanguage(lang);
}

/** Call once, after `loadPrefs()`, so the saved language is known. */
export function initI18n() {
  return i18n
    .use(HttpBackend)
    .use(initReactI18next)
    .init({
      lng: detectLanguage(),
      fallbackLng: "en",
      supportedLngs: SUPPORTED_LANGUAGES,
      defaultNS: "translation",
      backend: { loadPath: "/locales/{{lng}}/{{ns}}.json" },
      interpolation: { escapeValue: false },
    });
}

export default i18n;
