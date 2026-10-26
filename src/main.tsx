import ReactDOM from "react-dom/client";
import App from "./App";
import { initI18n } from "./i18n";
import { loadPrefs } from "./prefs";

// Full-window app: the page itself never scrolls (the content area scrolls inside).
for (const el of [document.documentElement, document.body, document.getElementById("root")!]) {
  Object.assign(el.style, { height: "100%", margin: "0" });
}
document.body.style.overflow = "hidden";

// Preferences first: the theme and language read from them.
loadPrefs()
  .catch((e) => console.warn("could not load preferences", e))
  .then(() => initI18n())
  .finally(() => {
    ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(<App />);
  });
