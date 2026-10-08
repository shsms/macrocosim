// Whether the server can be reached, from what the background requests see, and
// the banner that says so.
import { logUi } from "./notices.js";

// The model: the latest background request decides. One that failed to reach
// the server marks it unreachable; any that got through marks it reachable,
// whichever loop sent it, so a loop that failed and then stopped (its panel
// closed) cannot hold the banner up. `report` says whether the state changed.
export function createReachability() {
  let down = false;
  return {
    report(ok) {
      const changed = down === ok;
      down = !ok;
      return changed;
    },
    unreachable: () => down,
  };
}

const reach = createReachability();
const reachedAgain = [];

// `fn` runs when a request gets through while the server is marked unreachable,
// so a waiting reconnect can go at once.
export function onReachedAgain(fn) {
  reachedAgain.push(fn);
}

// A background request reports whether it reached the server; the banner
// follows, and each change is logged.
export function reportReachable(ok) {
  if (ok && reach.unreachable()) for (const fn of reachedAgain) fn();
  if (!reach.report(ok)) return;
  const down = reach.unreachable();
  document.getElementById("server-banner").hidden = !down;
  logUi(down ? "warn" : "info", down ? "server unreachable" : "server reachable again");
}
