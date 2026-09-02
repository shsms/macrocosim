// Headless boot smoke for the UI's ES-module graph.
//
//   node tools/boot-smoke.mjs
//
// Imports ui-assets/app.js under a minimal DOM shim, so the whole
// import graph (and app.js's own init() at the end of its module
// body) actually executes in plain node. A curl-200 on the asset
// server proves only that a file is served; this proves the modules
// parse, link, and run — it is what catches a TDZ / class-ordering
// error, an import cycle, or a renamed export before it ships.
// Prints "boot smoke: module graph loaded" and exits 0 on success;
// any module-level throw exits non-zero with the stack.
//
// The shim answers every property access with another callable stub,
// which is enough for the DOM/browser API surface app.js touches at
// import time. When it grows a gap, extend the shim here — never the
// app code.

// The Proxy target has to be a real `function`, not an arrow: a Proxy
// over an arrow is not constructible, and the shim gets `new`ed
// (WebSocket, ResizeObserver). Sharing one target is fine — every trap
// answers from the handler, never from the target.
function noop() {}

const stub = () =>
  new Proxy(noop, {
    get(_t, p) {
      if (p === Symbol.toPrimitive || p === "toString") return () => "";
      if (p === "then") return undefined;
      return stub();
    },
    apply: () => stub(),
    construct: () => stub(),
  });

globalThis.document = stub();
globalThis.window = globalThis;
// Node 24 pre-defines globalThis.navigator with a getter only, so a
// plain assignment throws — redefine the property instead.
Object.defineProperty(globalThis, "navigator", { value: stub(), configurable: true });
globalThis.localStorage = stub();
globalThis.location = { hash: "", pathname: "/", search: "" };
// routing.js's applyInitialRoute() calls history.replaceState during init().
globalThis.history = stub();
globalThis.WebSocket = stub();
globalThis.requestAnimationFrame = () => 0;
globalThis.addEventListener = () => {};
globalThis.getComputedStyle = stub();
globalThis.CSS = stub();
globalThis.ResizeObserver = stub();
globalThis.MutationObserver = stub();
globalThis.fetch = () => new Promise(() => {});

await import(new URL("../ui-assets/app.js", import.meta.url).href);
console.log("boot smoke: module graph loaded");
