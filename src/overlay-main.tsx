import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource/sora/400.css";
import "@fontsource/sora/600.css";
import "@fontsource/sora/700.css";
import "@fontsource/noto-sans-sc/400.css";
import "@fontsource/noto-sans-sc/500.css";
import "@fontsource/noto-sans-sc/700.css";
import { Overlay } from "./components/Overlay";
import "./App.css";

// 悬浮字幕窗的独立入口（overlay.html），与主 app 完全解耦，只渲染 Overlay。
ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <Overlay />
  </React.StrictMode>
);
