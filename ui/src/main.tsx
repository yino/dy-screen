import React from "react";
import ReactDOM from "react-dom/client";
import { clientApi } from "./api";
import { App } from "./App";
import "./styles.css";

if (import.meta.env.DEV) {
  const visualParams = new URLSearchParams(window.location.search);
  const visualTheme = visualParams.get("theme");
  const visualScale = visualParams.get("scale");
  if (visualTheme === "dark") document.documentElement.dataset.theme = "dark";
  if (visualScale === "125" || visualScale === "150") {
    document.documentElement.dataset.visualScale = visualScale;
  }
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App api={clientApi} />
  </React.StrictMode>,
);
