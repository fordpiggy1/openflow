import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles.css";

try {
  const stored = window.localStorage.getItem("openflow-theme");
  const theme = stored === "dark" || stored === "light"
    ? stored
    : window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  document.documentElement.dataset.theme = theme;
  document.documentElement.style.colorScheme = theme;
} catch { /* Theme is applied again by App after boot. */ }

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
