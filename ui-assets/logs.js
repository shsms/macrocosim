// The logs panel's rows: the server's lines from /api/logs and the WebSocket,
// and the UI's own entries. Capped at 500 rows so a chatty session does not
// freeze the panel.
import * as zone from "./zone.js";

export function appendLog(ev) {
  const box = document.getElementById("logs");
  if (!box) return;
  const row = document.createElement("div");
  row.className = `log-line ${(ev.level || "info").toLowerCase()}`;
  const ts = document.createElement("span");
  ts.className = "log-ts";
  ts.innerHTML = zone.timeHtml(ev.ts, "hms");
  const level = document.createElement("span");
  level.className = "log-lvl";
  level.textContent = ev.level || "";
  const message = document.createElement("span");
  message.className = "log-msg";
  message.textContent = ev.message || "";
  row.append(ts, level, message);
  // Follow the tail only when the user has not scrolled away from it.
  const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 30;
  box.appendChild(row);
  while (box.children.length > 500) box.removeChild(box.firstChild);
  if (atBottom) box.scrollTop = box.scrollHeight;
}
