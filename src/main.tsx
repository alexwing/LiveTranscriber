import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";

import App from "./App";
import Overlay from "./Overlay";
import "./styles.css";

// Las dos ventanas comparten bundle; cada una monta lo suyo segun su etiqueta.
const isOverlay = getCurrentWindow().label === "overlay";
if (isOverlay) {
  document.body.classList.add("overlay-body");
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{isOverlay ? <Overlay /> : <App />}</React.StrictMode>
);
