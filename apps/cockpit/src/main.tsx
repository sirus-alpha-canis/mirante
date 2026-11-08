// xterm's own stylesheet (the Terminal tab renders unstyled without it), then ours.
import "@xterm/xterm/css/xterm.css";
import "./styles.css";
import { openUrl } from "@tauri-apps/plugin-opener";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import type { Env } from "./env";
import { daemonStart, rpc, subscribe } from "./lib/daemon";
import { notify } from "./lib/notify";
import { createCockpit } from "./store/cockpit";

const env: Env = {
  cockpit: createCockpit({ rpc, subscribe }),
  rpc,
  storage: window.localStorage,
  openUrl: (url) => openUrl(url),
  startDaemon: daemonStart,
  notify: (n) => void notify(n).catch(() => {}),
};

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App env={env} />
  </StrictMode>,
);
