import { createLayoutInteraction } from "./layoutInteraction.js";
import {
  buildConfigPayload,
  createLayoutModel,
  updateConfigPlacement,
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
    const model = createLayoutModel(state.config, state.peers, state.status);
    saveRepairedPlacements(model);
    return model;
  }

  // Persists placements the model moved off another display, so the runtime
  // and the other machines use the same non-overlapping layout the editor shows.
  function saveRepairedPlacements(model) {
    const repaired = model.placements.filter((item) => item.repaired);
    if (!repaired.length || state.layoutDrag) {
      return;
    }
    repaired.forEach((item) => {
      state.config = updateConfigPlacement(
        state.config,
        item.peer,
        item.x,
        item.y,
        state.peers,
      );
    });
    markConfigEdited();
    scheduleConfigSave();
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
