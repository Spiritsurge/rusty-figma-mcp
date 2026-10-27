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

test("a solid fill reports the variable it is bound to, so equal hex values can be told apart", async () => {
  const paint = (id) => ({
    type: "SOLID",
    visible: true,
    opacity: 1,
    blendMode: "NORMAL",
    color: { r: 0.235, g: 0.255, b: 0.306 },
    ...(id ? { boundVariables: { color: { type: "VARIABLE_ALIAS", id } } } : {}),
  });
  const node = (id, fills) => ({ id, name: id, type: "RECTANGLE", fills, boundVariables: { topLeftRadius: { type: "VARIABLE_ALIAS", id: "VariableID:9:1" } } });

  const bound = await serializeNode(node("a", [paint("VariableID:1:2")]));
  const bare = await serializeNode(node("b", [paint(null)]));

  assert.equal(bound.fills[0].color, bare.fills[0].color);
  assert.equal(bound.fills[0].variableId, "VariableID:1:2");
  assert.equal("variableId" in bare.fills[0], false);
  assert.deepEqual(bound.boundVariables, { topLeftRadius: "VariableID:9:1" });
});

test("effect bindings and per-range text bindings come back as ids", async () => {
  const shadow = {
    type: "DROP_SHADOW",
    visible: true,
    radius: 4,
    color: { r: 0, g: 0, b: 0, a: 0.25 },
    offset: { x: 0, y: 2 },
    blendMode: "NORMAL",
    boundVariables: { color: { type: "VARIABLE_ALIAS", id: "VariableID:3:3" } },
  };
  const out = await serializeNode({
    id: "t",
    name: "t",
    type: "TEXT",
    characters: "x",
    fontSize: 12,
    fontName: { family: "Inter", style: "Regular" },
    textAlignHorizontal: "LEFT",
    letterSpacing: { value: 0, unit: "PIXELS" },
    lineHeight: { unit: "AUTO" },
    effects: [shadow],
    boundVariables: { fontSize: [{ type: "VARIABLE_ALIAS", id: "VariableID:4:4" }], fills: [{ type: "VARIABLE_ALIAS", id: "VariableID:5:5" }] },
  });
  assert.deepEqual(out.effects[0].boundVariables, { color: "VariableID:3:3" });
  assert.deepEqual(out.boundVariables, { fontSize: ["VariableID:4:4"] });
});
