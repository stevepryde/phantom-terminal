import { useSyncExternalStore } from "react";

/**
 * A minimal observable store — a deliberate, dependency-free replacement for
 * TanStack Store. The project avoids TanStack to shrink its supply-chain attack
 * surface (a terminal handles secrets), so this is the entire state primitive.
 */
export interface Store<T> {
  readonly state: T;
  setState: (updater: T | ((prev: T) => T)) => void;
  subscribe: (listener: () => void) => () => void;
}

export function createStore<T>(initial: T): Store<T> {
  let state = initial;
  const listeners = new Set<() => void>();
  return {
    get state() {
      return state;
    },
    setState(updater) {
      const next = typeof updater === "function" ? (updater as (prev: T) => T)(state) : updater;
      if (Object.is(next, state)) return;
      state = next;
      for (const listener of listeners) listener();
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}

/**
 * Subscribe a component to a slice of a store. The selector must return a stable
 * reference when its inputs are unchanged (return primitives or the same array/
 * object identity), as `useSyncExternalStore` compares snapshots with `Object.is`.
 */
export function useStore<T, S>(store: Store<T>, selector: (state: T) => S): S {
  const getSnapshot = () => selector(store.state);
  return useSyncExternalStore(store.subscribe, getSnapshot, getSnapshot);
}
