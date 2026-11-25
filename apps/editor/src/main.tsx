import "@renderly/ui/fonts.css";
import "@renderly/ui/styles.css";
import "./app.css";
import { createRoot } from "react-dom/client";
import { App, Unsupported } from "./App";
import { probe } from "./probe";
import { applyTheme, savedTheme } from "./theme";

applyTheme(savedTheme());
const missing = probe();
const root = createRoot(document.getElementById("root") as HTMLElement);
root.render(missing.length > 0 ? <Unsupported missing={missing} /> : <App />);
Object.assign(window, { appReadyAt: performance.now() });
