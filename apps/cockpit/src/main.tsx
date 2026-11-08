// xterm's own stylesheet (the Terminal tab renders unstyled without it), then ours.
import "@xterm/xterm/css/xterm.css";
import "./styles.css";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import type { Env } from "./env";
import { daemonStart, homeDir, pathKind, rpc, subscribe } from "./lib/daemon";
import { notify } from "./lib/notify";
import { createCockpit } from "./store/cockpit";

const env: Env = {
  cockpit: createCockpit({ rpc, subscribe }),
  rpc,
  storage: window.localStorage,
  openUrl: (url) => openUrl(url),
  startDaemon: daemonStart,
  notify: (n) => void notify(n).catch(() => {}),
  // The native picker: one folder, nothing else (the capability grants `dialog:allow-open` only).
  pickFolder: async () => {
    const picked = await open({ directory: true, multiple: false, title: "Add a repository" });
    return typeof picked === "string" ? picked : null;
  },
  homeDir,
  pathKind,
};

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App env={env} />
  </StrictMode>,
);
