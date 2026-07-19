import React from "react";
import ReactDOM from "react-dom/client";
import { clientApi } from "./api";
import { App } from "./App";
import "./styles.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App api={clientApi} />
  </React.StrictMode>,
);
