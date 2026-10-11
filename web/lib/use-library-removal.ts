import { useCallback, useEffect, useRef, useState } from "react";
import { getLibraryRemoval, removeLibraryAndWait, type LibraryRemoval, type MediaLibrary } from "@/lib/api/libraries";
import { waitForLibraryRemoval } from "@/lib/library-removal";

export interface RemovalViewState {
  operation?: LibraryRemoval;
  pending: boolean;
  error?: string;
}

/** One waiter per library, with lifecycle-owned cancellation and durable server resume. */
export function useLibraryRemoval(
  libraries: MediaLibrary[] | null,
  onSuccess: (library: MediaLibrary) => void,
  libraryId?: string,
) {
  const [states, setStates] = useState<Record<string, RemovalViewState>>({});
  const controllers = useRef(new Map<string, AbortController>());
  const observed = useRef(new Set<string>());
  const mounted = useRef(false);
  const success = useRef(onSuccess);
  success.current = onSuccess;

  useEffect(() => {
    mounted.current = true;
    const active = controllers.current;
    const seen = observed.current;
    return () => {
      mounted.current = false;
      for (const controller of active.values()) controller.abort();
      active.clear();
      seen.clear();
    };
  }, [libraryId]);

  const track = useCallback((library: MediaLibrary, resume?: LibraryRemoval) => {
    if (!mounted.current || controllers.current.has(library.id)) return;
    const controller = new AbortController();
    controllers.current.set(library.id, controller);
    setStates((prev) => ({ ...prev, [library.id]: { operation: resume, pending: true } }));
    const options = {
      signal: controller.signal,
      onProgress: (operation: LibraryRemoval) => {
        if (controller.signal.aborted) return;
        observed.current.add(operation.operation_id);
        setStates((prev) => ({ ...prev, [library.id]: { operation, pending: operation.state !== "failed" && operation.state !== "succeeded" } }));
      },
    };
    const action = resume
      ? waitForLibraryRemoval(resume, getLibraryRemoval, options)
      : removeLibraryAndWait(library.id, options);
    void action.then(() => {
      if (controller.signal.aborted) return;
      setStates((prev) => ({ ...prev, [library.id]: { ...prev[library.id], pending: false } }));
      success.current(library);
    }).catch((error: unknown) => {
      if (controller.signal.aborted) return;
      setStates((prev) => ({ ...prev, [library.id]: { ...prev[library.id], pending: false, error: error instanceof Error ? error.message : "删除状态读取失败，请重试" } }));
    }).finally(() => {
      if (controllers.current.get(library.id) === controller) controllers.current.delete(library.id);
    });
  }, []);

  useEffect(() => {
    for (const library of libraries ?? []) {
      if (libraryId && library.id !== libraryId) continue;
      const operation = library.removal;
      if (!operation || operation.state === "failed" || operation.state === "succeeded") continue;
      if (!observed.current.has(operation.operation_id)) track(library, operation);
    }
  }, [libraries, libraryId, track]);

  return { states, remove: (library: MediaLibrary) => track(library), resume: (library: MediaLibrary, operation: LibraryRemoval) => track(library, operation) };
}
