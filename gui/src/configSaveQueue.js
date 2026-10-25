function copyPayload(payload) {
  if (typeof structuredClone === "function") {
    return structuredClone(payload);
  }
  return JSON.parse(JSON.stringify(payload));
}

export function createConfigSaveQueue({
  save,
  onStart,
  onSaved,
  onError,
  onIdle,
}) {
  let inFlight = false;
  let pending = null;
  let destroyed = false;

  async function startNext() {
    if (destroyed || inFlight || !pending) {
      return;
    }

    const request = pending;
    pending = null;
    inFlight = true;
    onStart?.(request);

    try {
      const config = await save(request.payload);
      if (!destroyed) {
        onSaved?.(config, request);
      }
    } catch (error) {
      if (!destroyed) {
        onError?.(error, request);
      }
    } finally {
      inFlight = false;
      if (destroyed) {
        return;
      }
      if (pending) {
        void startNext();
      } else {
        onIdle?.();
      }
    }
  }

  return {
    enqueue(revision, payload) {
      pending = {
        revision,
        payload: copyPayload(payload),
      };
      void startNext();
    },
    isBusy() {
      return inFlight || pending !== null;
    },
    destroy() {
      destroyed = true;
      pending = null;
    },
  };
}
