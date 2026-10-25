// Setup Tauri mock for all tests
const noop = () => {};
global.window = {
  __TAURI__: {
    core: { invoke: () => {} },
    event: { listen: () => {} },
  },
  addEventListener: noop,
  removeEventListener: noop,
  document: {
    addEventListener: noop,
    removeEventListener: noop,
    getElementById: () => null,
    querySelector: () => null,
    querySelectorAll: () => [],
    createElement: () => ({ 
      style: {}, 
      appendChild: () => {},
      setAttribute: () => {},
      classList: { add: () => {}, remove: () => {} },
    }),
  },
  getComputedStyle: () => ({}),
  requestAnimationFrame: (fn) => { fn(); return 0; },
  cancelAnimationFrame: () => {},
};
