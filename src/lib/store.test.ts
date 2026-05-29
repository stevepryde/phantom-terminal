import { expect, test } from "bun:test";
import { createStore } from "./store";

test("createStore notifies subscribers when state changes", () => {
  const store = createStore({ count: 0 });
  let calls = 0;
  const unsubscribe = store.subscribe(() => {
    calls += 1;
  });

  store.setState((state) => ({ count: state.count + 1 }));

  expect(store.state.count).toBe(1);
  expect(calls).toBe(1);
  unsubscribe();
});

test("createStore skips notifications for identical state references", () => {
  const initial = { count: 0 };
  const store = createStore(initial);
  let calls = 0;
  store.subscribe(() => {
    calls += 1;
  });

  store.setState(initial);

  expect(calls).toBe(0);
});
