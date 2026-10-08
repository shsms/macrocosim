// localStorage, guarded: a browser that blocks site storage throws on every
// access. The values then live in memory for the session (the router keeps the
// selected microgrid here, so it must still round-trip) and are not remembered.

const session = new Map();

export function readStorage(key) {
  try {
    return localStorage.getItem(key);
  } catch (_) {
    return session.get(key) ?? null;
  }
}

export function writeStorage(key, value) {
  try {
    localStorage.setItem(key, value);
  } catch (_) {
    session.set(key, String(value));
  }
}

export function removeStorage(key) {
  try {
    localStorage.removeItem(key);
  } catch (_) {
    session.delete(key);
  }
}
