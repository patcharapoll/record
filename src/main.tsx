import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";
import { api } from "./lib/api";

// With XSPECTOR_DEBUG set, script errors are printed to the terminal (the webview has no console here).
window.addEventListener("error", (e) => void api.debugLog(`JS error: ${e.message} @ ${e.filename}:${e.lineno}`));
window.addEventListener("unhandledrejection", (e) => void api.debugLog(`JS rejection: ${String(e.reason)}`));
void api.debugLog("UI started");

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
