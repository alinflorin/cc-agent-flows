import { useTranslation } from "react-i18next";
import { Field, Select } from "@fluentui/react-components";
import { LANGUAGE_NAMES, SUPPORTED_LANGUAGES, setLanguage, type Language } from "../i18n";
import { THEMES, setTheme, useThemePref, type Theme } from "../theme";

/** Theme and language, applied as soon as they are picked. Used by Settings and the welcome dialog. */
export function AppearanceFields() {
  const { t, i18n } = useTranslation();
  const themePref = useThemePref();
  const lang = (SUPPORTED_LANGUAGES as readonly string[]).includes(i18n.language) ? (i18n.language as Language) : "en";
  return (
    <>
      <Field label={t("theme.label")}>
        <Select value={themePref} onChange={(_, d) => setTheme(d.value as Theme)}>
          {THEMES.map((th) => (
            <option key={th} value={th}>
              {t(`theme.${th}`)}
            </option>
          ))}
        </Select>
      </Field>
      <Field label={t("language")}>
        <Select value={lang} onChange={(_, d) => void setLanguage(d.value as Language)}>
          {SUPPORTED_LANGUAGES.map((l) => (
            <option key={l} value={l}>
              {LANGUAGE_NAMES[l]}
            </option>
          ))}
        </Select>
      </Field>
    </>
  );
}
