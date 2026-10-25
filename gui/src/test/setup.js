import { vi } from "vitest";

const commandHandlers = new Map();
const eventListeners = new Map();

const mockInvoke = vi.fn(async (command, payload) => {
  const handler = commandHandlers.get(command);
  if (!handler) {
    throw new Error(`[test] No mock handler for command: ${command}`);
  }
  return handler(payload);
});

const mockListen = vi.fn(async (eventName, callback) => {
  eventListeners.set(eventName, callback);
  return () => {
    eventListeners.delete(eventName);
  };
});

window.__TAURI__ = {
  core: { invoke: mockInvoke },
  event: { listen: mockListen },
};

const storageStore = new Map();
Object.defineProperty(window, "localStorage", {
  value: {
    getItem: (key) => storageStore.get(key) ?? null,
    setItem: (key, value) => { storageStore.set(key, String(value)); },
    removeItem: (key) => { storageStore.delete(key); },
    clear: () => { storageStore.clear(); },
    get length() { return storageStore.size; },
    key: (index) => Array.from(storageStore.keys())[index] ?? null,
  },
  writable: true,
  configurable: true,
});

if (typeof globalThis.DOMPoint === "undefined" || !DOMPoint.prototype.matrixTransform) {
  globalThis.DOMPoint = class DOMPoint {
    constructor(x = 0, y = 0, z = 0, w = 1) {
      this.x = x;
      this.y = y;
      this.z = z;
      this.w = w;
    }
    matrixTransform(matrix) {
      return new DOMPoint(
        this.x * matrix.a + this.y * matrix.c + matrix.e,
        this.x * matrix.b + this.y * matrix.d + matrix.f,
      );
    }
  };
}

if (typeof globalThis.DOMMatrix === "undefined") {
  globalThis.DOMMatrix = class DOMMatrix {
    constructor(values) {
      if (Array.isArray(values)) {
        this.a = values[0] ?? 1;
        this.b = values[1] ?? 0;
        this.c = values[2] ?? 0;
        this.d = values[3] ?? 1;
        this.e = values[4] ?? 0;
        this.f = values[5] ?? 0;
      } else {
        this.a = 1; this.b = 0; this.c = 0; this.d = 1; this.e = 0; this.f = 0;
      }
    }
  };
}

export {
  commandHandlers,
  eventListeners,
  mockInvoke,
  mockListen,
};
