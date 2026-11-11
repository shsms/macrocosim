// The Microgrids landing-page list: polls /api/microgrids, renders a
// card grid, and responds to clicks / WS pushes by re-fetching +
// re-rendering. Also the selected microgrid's header state and the
// create, import and export flows.

import { escapeHtml, mutate, selectMicrogrid } from "./app.js";
import { bgFetch, errorText } from "./http.js";
import { countText } from "./live.js";
import { clearFormError, notify, showFormError } from "./notices.js";
import { publishMgList, readSelectedMg, reconcileSelection } from "./routing.js";
import { unsavedChip } from "./unsaved-chip.js";

// Lowest id `POST /api/microgrids` allocates when none is asked
// for (`DEFAULT_MICROGRID_ID` server-side). The create dialog
// pre-fills the same choice the server would make, computed off the
// list the panel already holds, so the field shows a real id rather
// than a blank the user has to guess at.
const FIRST_MICROGRID_ID = 2200;

export const microgridsPanel = (() => {
  let cached = []; // last /api/microgrids snapshot
  let pollTimer = null;

  function gridEl() { return document.getElementById("mglist-grid"); }
  // Warn when a microgrid was created but its gRPC server failed.
  function warnFailedRuntime(id, runtime) {
    if (runtime?.status === "failed") {
      notify(`Microgrid #${id} was created, but its gRPC server failed: ${runtime.error}`);
    }
  }
  function breadcrumbNameEl() { return document.getElementById("mg-breadcrumb-name"); }
  function breadcrumbTsoEl() { return document.getElementById("mg-breadcrumb-tso"); }

  function renderList() {
    const grid = gridEl();
    if (!grid) return;
    grid.innerHTML = "";
    for (const m of cached) {
      const card = document.createElement("button");
      card.type = "button";
      card.className = "mglist-card";
      card.dataset.id = m.id;
      const tso = m.tso ? `<span class="mg-tso">${escapeHtml(m.tso)}</span>` : "";
      // The `unsaved` chip, and a `failed` one when its gRPC server
      // did not start.
      const runtime = m.runtime;
      const chips = [
        unsavedChip(m),
        runtime?.status === "failed"
          ? `<span class="mg-chip failed" title="${escapeHtml(runtime.error ?? "")}">failed</span>`
          : "",
      ].join("");
      // A starting runtime shows "starting…", a started one its
      // bound address or "not bound"; the configured port stands in
      // when no runtime was started.
      let grpc = `:${m.grpc_port}`;
      if (runtime?.status === "starting") grpc = "starting…";
      else if (runtime) grpc = runtime.grpc_addr ?? "not bound";
      card.innerHTML = `
        <span class="mglist-id">#${m.id}</span>
        <h3 class="mglist-name">${escapeHtml(m.name || "(unnamed)")}</h3>
        ${tso}${chips}
        <span class="mglist-meta muted">${countText(m.component_count, m.hidden_component_count)} · gRPC ${escapeHtml(grpc)}</span>
      `;
      card.addEventListener("click", () => selectMicrogrid(m.id));
      grid.appendChild(card);
    }
    // Trailing [+ New microgrid] card: opens the create dialog,
    // which POSTs /api/microgrids and selects the new entry.
    const newCard = document.createElement("button");
    newCard.type = "button";
    newCard.className = "mglist-card mglist-new";
    newCard.id = "mglist-new-btn";
    newCard.innerHTML = `<span class="mglist-plus">+</span><span>New microgrid</span>`;
    newCard.addEventListener("click", () => showCreateMgDialog());
    grid.appendChild(newCard);
    // Trailing [⇪ Import site…] card: picks a site export
    // (components.json + connections.json together) and POSTs
    // /api/microgrids/import, which creates a real simulated
    // microgrid with the export's capacity / SoC / rated bounds.
    const importCard = document.createElement("button");
    importCard.type = "button";
    importCard.className = "mglist-card mglist-new";
    importCard.id = "mglist-import-btn";
    importCard.title =
      "Load a microgrid API site export: pick components.json and connections.json together (connections optional)";
    importCard.innerHTML = `<span class="mglist-plus">⇪</span><span>Import site…</span>`;
    importCard.addEventListener("click", () => {
      document.getElementById("import-files").click();
    });
    grid.appendChild(importCard);
  }

  // The lowest free microgrid id — the same choice the server makes
  // for a create that names none. Walks the ascending taken ids from
  // FIRST_MICROGRID_ID, mirroring `next_free_id_in` server-side.
  function nextFreeId() {
    const taken = cached.map((m) => m.id).sort((a, b) => a - b);
    let candidate = FIRST_MICROGRID_ID;
    for (const id of taken) {
      if (id === candidate) candidate += 1;
      else if (id > candidate) break;
    }
    return candidate;
  }

  function showCreateMgDialog() {
    const dlg = document.getElementById("create-mg-dialog");
    if (!dlg) return;
    document.getElementById("create-mg-name").value = "";
    // Pre-filled, not fixed: the server re-checks the id under the
    // structural lock, and a taken one comes back as the inline 409.
    document.getElementById("create-mg-id").value = String(nextFreeId());
    document.getElementById("create-mg-port").value = "";
    dlg.showModal();
  }

  function setupCreateMgDialog() {
    const dlg = document.getElementById("create-mg-dialog");
    if (!dlg) return;
    document
      .getElementById("create-mg-close")
      .addEventListener("click", () => dlg.close());
    dlg.addEventListener("click", (e) => {
      if (e.target === dlg) dlg.close();
    });
    document
      .getElementById("create-mg-form")
      .addEventListener("submit", async (e) => {
        e.preventDefault();
        const err = document.getElementById("create-mg-error");
        clearFormError(err);
        const name = document.getElementById("create-mg-name").value.trim();
        if (!name) return;
        // A new microgrid is a topology document with one empty
        // microgrid in it, kept in memory only.
        const mg = { name, components: [], connections: [] };
        // A blank number field means "let the server allocate" — an
        // explicit 0 would be a claim on a real (and invalid) id.
        const id = document.getElementById("create-mg-id").value.trim();
        if (id !== "") mg.id = Number(id);
        const port = document.getElementById("create-mg-port").value.trim();
        if (port !== "") mg.grpc_port = Number(port);
        let created;
        try {
          const body = { schema_version: 1, microgrids: [mg] };
          created = (await (await mutate("POST", "/api/microgrids", body)).json()).microgrids[0];
        } catch (ex) {
          // A taken id or port comes back 409 with the server's own
          // wording — shown in the dialog, which stays open so the
          // user can pick another without retyping the name.
          showFormError(err, ex.message);
          return;
        }
        dlg.close();
        warnFailedRuntime(created.id, created.runtime);
        await refresh();
        selectMicrogrid(created.id);
      });
  }

  // Settles the pending ask with the next answer: `{ name, mid }` from a
  // submit, which leaves the dialog open, or null when it closes.
  let settleImport = null;

  // Ask for the imported microgrid's name and id. Resolves to
  // `{ name, mid }` — `mid` null for "let the server allocate" — or
  // null if the user backed out. The dialog opens fresh if it is closed;
  // after a refused import it is still open, with the user's answer and
  // the server's error in it, and this waits for the next submit.
  function askImportDetails() {
    const dlg = document.getElementById("import-mg-dialog");
    if (!dlg) return Promise.resolve(null);
    if (!dlg.open) {
      document.getElementById("import-mg-name").value = "";
      // Pre-filled the way the create dialog pre-fills its id — the
      // lowest free one — and just as editable: the server re-checks
      // under the structural lock, and a taken one comes back as a 409.
      document.getElementById("import-mg-id").value = String(nextFreeId());
      dlg.showModal();
    }
    return new Promise((resolve) => {
      settleImport = resolve;
    });
  }

  function settleImportWith(answer) {
    const settle = settleImport;
    settleImport = null;
    settle?.(answer);
  }

  function setupImportMgDialog() {
    const dlg = document.getElementById("import-mg-dialog");
    if (!dlg) return;
    document
      .getElementById("import-mg-close")
      .addEventListener("click", () => dlg.close());
    dlg.addEventListener("click", (e) => {
      if (e.target === dlg) dlg.close();
    });
    dlg.addEventListener("close", () => settleImportWith(null));
    document.getElementById("import-mg-form").addEventListener("submit", (e) => {
      e.preventDefault();
      const err = document.getElementById("import-mg-error");
      const name = document.getElementById("import-mg-name").value.trim();
      if (!name) return;
      clearFormError(err);
      // A blank id field means "let the server allocate" — the same
      // bargain the create dialog offers, and the only way to reach
      // the import endpoint's auto-assign path from the UI. A filled
      // one has to be a positive whole number; whether it is FREE is
      // the server's half of the answer, and comes back as the
      // dialog's inline error on the next pass.
      const raw = document.getElementById("import-mg-id").value.trim();
      let mid = null;
      if (raw !== "") {
        mid = Number(raw);
        if (!/^\d+$/.test(raw) || !Number.isSafeInteger(mid) || mid < 1) {
          err.textContent = "Microgrid id must be a positive whole number.";
          err.hidden = false;
          return;
        }
      }
      settleImportWith({ name, mid });
    });
  }

  // One import flow at a time. The dialog's resolver is module-scoped
  // and an import spans two awaits it does not own the screen for —
  // the POST, and any retry after a refusal — so a second file pick
  // landing in between would clobber the first flow's resolver and
  // then call showModal() on an already-open dialog: an
  // InvalidStateError thrown after the damage, leaving the second
  // dialog open and settling nothing while the first flow dies as an
  // unhandled rejection. Guarding the whole flow makes the resolver
  // single-owner by construction, so nothing below has to juggle it.
  let importInFlight = false;

  async function importSiteFiles(files) {
    if (importInFlight) {
      notify("An import is already in progress.");
      return;
    }
    importInFlight = true;
    try {
      await runImport(files);
    } finally {
      importInFlight = false;
    }
  }

  // The site-export files are identified by content — the object key
  // names — not by their file names, so renamed downloads still
  // import. A file that parses but matches neither key is an error;
  // a file that doesn't parse at all is too.
  async function runImport(files) {
    let components = null;
    let connections = null;
    for (const file of files) {
      let parsed;
      try {
        parsed = JSON.parse(await file.text());
      } catch (e) {
        notify(`${file.name}: not valid JSON (${e.message})`);
        return;
      }
      // JSON.parse also accepts null / numbers / strings — anything
      // that isn't an object with one of the two keys is an error,
      // not a crash (`null.electricalComponents` would throw into an
      // unhandled rejection: no toast, import silently dead).
      if (parsed?.electricalComponents) {
        if (components) {
          notify(`${file.name}: second components export — pick only one`);
          return;
        }
        components = parsed;
      } else if (parsed?.electricalComponentConnections) {
        if (connections) {
          notify(`${file.name}: second connections export — pick only one`);
          return;
        }
        connections = parsed;
      } else {
        notify(`${file.name}: neither components nor connections export`);
        return;
      }
    }
    if (!components) {
      notify("Pick the components.json file (connections.json is optional).");
      return;
    }
    // The parsed exports stay in scope across retries: a refused id keeps
    // the dialog open with the server's error rather than ending the
    // import, so correcting it costs one edit instead of another trip
    // through the file picker (the input was cleared the moment it
    // fired). Cancelling the dialog still abandons the whole thing.
    const dlg = document.getElementById("import-mg-dialog");
    for (;;) {
      const chosen = await askImportDetails();
      if (!chosen) return;
      const body = { name: chosen.name, components, connections };
      // Omitted entirely when blank, so the server allocates.
      if (chosen.mid !== null) body.id = chosen.mid;
      try {
        const res = await mutate("POST", "/api/microgrids/import", body);
        const m = await res.json();
        dlg.close();
        notify(
          `Imported ${m.components} components, ${m.connections} connections.`,
          "success",
        );
        warnFailedRuntime(m.id, m.runtime);
        selectMicrogrid(m.id);
        return;
      } catch (e) {
        const message = `Import failed: ${e.message}`;
        // Dismissed while the request was out: say so in a toast rather
        // than open the dialog again unasked.
        if (!dlg.open) {
          notify(message);
          return;
        }
        showFormError(document.getElementById("import-mg-error"), message);
      }
    }
  }

  function renderBreadcrumb() {
    const id = readSelectedMg();
    if (id == null) return;
    const entry = cached.find((m) => m.id === id);
    if (breadcrumbNameEl()) {
      breadcrumbNameEl().textContent = entry
        ? `#${entry.id} ${entry.name || "(unnamed)"}`
        : `#${id} (unknown)`;
    }
    if (breadcrumbTsoEl()) {
      breadcrumbTsoEl().textContent = entry?.tso ? `· ${entry.tso}` : "";
    }
    renderHeaderState(entry);
  }

  // The selected microgrid's file state in its header: the chip the
  // card carries.
  function renderHeaderState(entry) {
    const chips = document.getElementById("mg-file-chips");
    if (chips) chips.innerHTML = entry ? unsavedChip(entry) : "";
  }

  // The selected microgrid as a Python module, from the engine's
  // export (GET /api/mg/{mg}/spec.py). Throws with the server's
  // message: 503 without a usable Python, 429 while another export
  // runs.
  async function exportPython() {
    const id = readSelectedMg();
    const res = await fetch(`/api/mg/${id}/spec.py`);
    if (!res.ok) throw new Error(await errorText(res));
    return { id, text: await res.text() };
  }

  // The clipboard write starts in the click itself, with the export
  // as a promise: Safari and Firefox refuse a write that comes after
  // an await. A browser without ClipboardItem writes the text once it
  // has it, and one without a clipboard at all (a page served over
  // plain http on another host) is told to use Save as .py. A failed
  // export shows the export's own message, not the clipboard's.
  async function copyAsPython() {
    const id = readSelectedMg();
    if (!navigator.clipboard) {
      notify(
        "Copy as Python needs the browser's clipboard, which a page served over plain http from another host does not get; use Save as .py.",
      );
      return;
    }
    const exported = exportPython();
    try {
      if (typeof ClipboardItem === "undefined") {
        await navigator.clipboard.writeText((await exported).text);
      } else {
        const blob = exported.then(({ text }) => new Blob([text], { type: "text/plain" }));
        await navigator.clipboard.write([new ClipboardItem({ "text/plain": blob })]);
      }
      notify(`Copied microgrid #${id} as Python.`, "success");
    } catch (e) {
      const err = await exported.then(
        () => e,
        (x) => x,
      );
      notify(`Copy as Python failed: ${err.message}`);
    }
  }

  async function saveAsPython() {
    try {
      const { id, text } = await exportPython();
      const a = document.createElement("a");
      a.href = URL.createObjectURL(new Blob([text], { type: "text/x-python" }));
      a.download = `microgrid_${id}.py`;
      a.click();
      // Some browsers cancel a download whose URL goes at once.
      setTimeout(() => URL.revokeObjectURL(a.href), 60_000);
    } catch (e) {
      notify(`Save as .py failed: ${e.message}`);
    }
  }

  // Shared by refresh() and the 5 s poll. A non-ok response keeps
  // the previous list. A fetch that fails outright (network-level)
  // blanks it for refresh() and keeps it for the poll
  // (`blankOnFailure`). Two fetches can be in flight at once (the
  // poll's and the one a selection write triggers), so outcomes land
  // in issue order or not at all: a list older than the one on screen
  // is dropped, and a failure blanks only while nothing newer has
  // answered at all (a newer non-ok answer is still an answer). A
  // list fresh from the server is the one place the selection can be
  // checked against what the server has — unless the selection moved
  // while the response was in flight: the user can pick a microgrid
  // the list was fetched too early to know (create, import, a card
  // click), so such a list is kept but not made a judge. The fetch
  // the selection write itself triggers, or the next poll, judges it
  // instead.
  let listIssued = 0;
  let listApplied = 0; // the request whose list is on screen
  let listAnswered = 0; // the newest request the server answered
  async function fetchList(blankOnFailure) {
    const seq = ++listIssued;
    const mg = readSelectedMg();
    let rows = null; // null: the fetch failed outright
    try {
      const res = await bgFetch("microgrids", "/api/microgrids");
      listAnswered = Math.max(listAnswered, seq);
      if (!res.ok) return;
      rows = await res.json();
    } catch (_) {
      if (!blankOnFailure || seq < listAnswered) return;
    }
    if (seq < listApplied) return;
    // A blank holds no sequence slot: an older body still in flight
    // is a real list, and repaints over it.
    if (rows) listApplied = seq;
    cached = rows ?? [];
    // Only a list fresh from the server may drop the selection.
    if (rows && readSelectedMg() === mg) reconcileSelection(rows);
  }
  function renderAll() {
    // The smoke reads the list on screen here.
    window.__mgPanelCache = cached;
    publishMgList(cached);
    renderList();
    renderBreadcrumb();
  }

  async function refresh() {
    await fetchList(true);
    renderAll();
    schedulePoll();
  }

  // The server unloaded microgrid `id`: drop its card at once, then
  // refetch the list, which leaves a view of `id` for the list unless
  // the microgrid was created again meanwhile.
  async function removed(id) {
    cached = cached.filter((m) => m.id !== id);
    renderAll();
    await refresh();
  }

  function schedulePoll() {
    if (pollTimer) clearInterval(pollTimer);
    if (document.body.dataset.mode !== "microgrids") return;
    pollTimer = setInterval(async () => {
      if (document.body.dataset.mode !== "microgrids") {
        clearInterval(pollTimer);
        pollTimer = null;
        return;
      }
      // Unlike refresh(), a transient poll failure keeps the old
      // list — blanking the cards every 5 s while the server
      // restarts would just flicker.
      await fetchList(false);
      renderAll();
    }, 5000);
  }

  // Module scripts run after the DOM is parsed, so the hidden file
  // input exists by now. Wired once here — the card in renderList is
  // re-created per render and only clicks the input.
  document.getElementById("import-files").addEventListener("change", (ev) => {
    const files = [...ev.target.files];
    ev.target.value = ""; // same files pickable again
    if (files.length) importSiteFiles(files);
  });

  setupCreateMgDialog();
  setupImportMgDialog();
  // The header buttons are static markup, so one listener each
  // outlives every renderBreadcrumb.
  document.getElementById("mg-copy-py-btn")?.addEventListener("click", copyAsPython);
  document.getElementById("mg-save-py-btn")?.addEventListener("click", saveAsPython);
  return { refresh, removed };
})();
