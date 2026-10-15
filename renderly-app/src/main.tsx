import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@fontsource/source-sans-3/400.css";
import "@fontsource/source-sans-3/500.css";
import "@fontsource/source-sans-3/600.css";
import "@fontsource/source-sans-3/700.css";
import "./styles/globals.css";
import "./styles/agent.css";
import { bootstrapTheme } from "./lib/theme";
import { App } from "./App";

bootstrapTheme();

const root = document.getElementById("app");
if (!root) throw new Error("missing #app root element");

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
