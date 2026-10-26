// Regression test: instances must serialize under dynamic-page access, where
// the synchronous `mainComponent` getter throws. Run with `npm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

globalThis.figma = { mixed: Symbol("mixed") };
const { serializeNode } = await import("../src/serialize.ts");

function instance(id, main) {
  return {
    id,
    name: `instance ${id}`,
    type: "INSTANCE",
    get mainComponent() {
      throw new Error("Cannot call with documentAccess: dynamic-page. Use node.getMainComponentAsync instead.");
    },
    getMainComponentAsync: async () => main,
    children: [],
  };
}

const button = { id: "1:1", name: "Button", type: "COMPONENT" };

test("an instance two levels down serializes with its component resolved", async () => {
  const tree = {
    id: "0:1",
    name: "Frame",
    type: "FRAME",
    children: [{ id: "0:2", name: "Row", type: "FRAME", children: [instance("0:3", button)] }],
  };
  const out = await serializeNode(tree, { depth: 3 });
  const leaf = out.children[0].children[0];
  assert.equal(leaf.componentId, "1:1");
  assert.equal(leaf.componentName, "Button");
});

test("an instance whose component is unreachable reports a null componentId", async () => {
  const out = await serializeNode(instance("0:4", null));
  assert.equal(out.componentId, null);
  assert.equal("componentName" in out, false);
});
