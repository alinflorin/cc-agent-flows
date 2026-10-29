import i18n from "i18next";
/** Backend errors come as plain lowercase sentences: shown to the user they start with a capital. */
export const capitalize = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

/** Dates and times follow the app language, not the system's. */
const lang = () => i18n.language || undefined;
export const fmtDateTime = (secs: number) => new Date(secs * 1000).toLocaleString(lang());
export const fmtTime = (secs: number) => new Date(secs * 1000).toLocaleTimeString(lang());
