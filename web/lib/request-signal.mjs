// @ts-check

/**
 * @typedef {Object} RequestSignalLease
 * @property {AbortSignal} signal
 * @property {() => void} release
 * @property {() => boolean} isLocalTimeout
 */

/**
 * Links an external AbortSignal to a per-request timeout controller.
 * Returns a lease with the linked signal, an idempotent release function,
 * and a predicate to test whether the abort came from local timeout.
 *
 * @param {AbortSignal | null | undefined} externalSignal
 * @param {number} [timeoutMs=15000]
 * @returns {RequestSignalLease}
 */
export function linkRequestSignal(externalSignal, timeoutMs = 15000) {
  const controller = new AbortController();
  let timedOut = false;
  let timeoutId = undefined;
  let released = false;

  // Named abort listener so removeEventListener can detach it cleanly
  const onExternalAbort = () => {
    controller.abort(externalSignal?.reason);
  };

  if (externalSignal) {
    if (externalSignal.aborted) {
      controller.abort(externalSignal.reason);
    } else {
      externalSignal.addEventListener("abort", onExternalAbort);
    }
  }

  if (!externalSignal && timeoutMs > 0 && !controller.signal.aborted) {
    timeoutId = setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, timeoutMs);
  }

  const release = () => {
    if (released) return;
    released = true;
    if (timeoutId !== undefined) {
      clearTimeout(timeoutId);
      timeoutId = undefined;
    }
    if (externalSignal) {
      externalSignal.removeEventListener("abort", onExternalAbort);
    }
  };

  return {
    signal: controller.signal,
    release,
    isLocalTimeout: () => timedOut,
  };
}
