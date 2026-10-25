import { createLayoutInteraction } from "./layoutInteraction.js";
import {
  buildConfigPayload,
  createLayoutModel,
} from "./layoutModel.js";
import { createLayoutRenderer } from "./layoutRenderer.js";

const CONFIG_SAVE_DEBOUNCE_MS = 250;

export function createLayoutController({
  state,
  ui,
  perfLog,
  isStatusTabActive,
  setConfigStatus,
  saveConfig,
  markConfigEdited,
  onDragEnd,
}) {
  function getLayoutModel() {
    return createLayoutModel(state.config, state.peers, state.status);
  }

  function scheduleConfigSave(delayMs = CONFIG_SAVE_DEBOUNCE_MS) {
    if (state.configSaveDebounceId !== null) {
      window.clearTimeout(state.configSaveDebounceId);
    }

    state.configSaveDebounceId = window.setTimeout(() => {
      state.configSaveDebounceId = null;
      saveConfig();
    }, delayMs);
  }

  function flushConfigSaveDebounce() {
    if (state.configSaveDebounceId === null) {
      return;
    }

    window.clearTimeout(state.configSaveDebounceId);
    state.configSaveDebounceId = null;
    saveConfig();
  }

  const renderLayoutDiagram = createLayoutRenderer({
    state,
    ui,
    getLayoutModel,
    perfLog,
    isStatusTabActive,
  });

  const interaction = createLayoutInteraction({
    state,
    ui,
    renderLayoutDiagram,
    scheduleConfigSave,
    flushConfigSaveDebounce,
    markConfigEdited,
    setConfigStatus,
    perfLog,
    onDragEnd,
  });

  return {
    ...interaction,
    buildConfigPayload() {
      return buildConfigPayload(state.config, state.peers);
    },
    renderLayoutDiagram,
    flushConfigSaveDebounce,
    cleanup() {
      if (state.configSaveDebounceId !== null) {
        window.clearTimeout(state.configSaveDebounceId);
        state.configSaveDebounceId = null;
      }
    },
  };
}
