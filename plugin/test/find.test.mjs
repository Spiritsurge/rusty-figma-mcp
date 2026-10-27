// find.ts is pure apart from getMainComponentAsync, so it runs against stub nodes.

import assert from "node:assert/strict";
import { test } from "node:test";

globalThis.figma = { mixed: Symbol("mixed") };
const { findNodes, normalizeColor } = await import("../src/find.ts");

const solid = (r, g, b, boundId) => ({
  type: "SOLID",
  visible: true,
  color: { r, g, b },
  ...(boundId ? { boundVariables: { color: { type: "VARIABLE_ALIAS", id: boundId } } } : {}),
});
const RED = [245 / 255, 63 / 255, 63 / 255]; // #f53f3f

const set = { id: "S:1", type: "COMPONENT_SET", name: "Button" };
const tiny = { id: "C:1", type: "COMPONENT", name: "Tiny 32", parent: set };
const giant = { id: "C:2", type: "COMPONENT", name: "Giant 48", parent: set };
const inst = (id, main) => ({ id, name: `btn ${id}`, type: "INSTANCE", getMainComponentAsync: async () => main, children: [] });

const tree = {
  id: "P:1",
  name: "UI kit",
  type: "PAGE",
  children: [
    {
      id: "F:1",
      name: "Header",
      type: "FRAME",
      fills: [solid(...RED, "VariableID:1:1")],
      fillStyleId: "S:red",
      children: [
        inst("I:1", tiny),
        inst("I:2", tiny),
        inst("I:3", giant),
        { id: "T:1", name: "Title", type: "TEXT", textStyleId: "S:h1", fills: [solid(...RED)] },
      ],
    },
  ],
};

test("a hard-coded colour is found whether or not it is bound, in any case or alpha spelling", async () => {
  const out = await findNodes(tree, { fillColor: normalizeColor("#F53F3Fcc") });
  assert.deepEqual(out.matches.map((m) => m.id), ["F:1", "T:1"]);
  assert.equal(out.matches[1].path, "UI kit / Header");
});

test("a variable matches only the fills bound to it", async () => {
  const out = await findNodes(tree, { variableId: "VariableID:1:1" });
  assert.deepEqual(out.matches.map((m) => m.id), ["F:1"]);
});

test("style ids match on any of the four style slots", async () => {
  assert.deepEqual((await findNodes(tree, { styleId: "S:h1" })).matches.map((m) => m.id), ["T:1"]);
  assert.deepEqual((await findNodes(tree, { styleId: "S:red" })).matches.map((m) => m.id), ["F:1"]);
});

test("instance counts resolve through the component, and through its component set", async () => {
  assert.equal((await findNodes(tree, { componentId: "C:1" })).total, 2);
  assert.equal((await findNodes(tree, { componentId: "C:2" })).total, 1);
  assert.equal((await findNodes(tree, { componentId: "S:1" })).total, 3);
});

test("limit 0 counts without listing, and total survives a limit", async () => {
  const none = await findNodes(tree, { types: ["INSTANCE"] }, { limit: 0 });
  assert.equal(none.total, 3);
  assert.deepEqual(none.matches, []);
  const two = await findNodes(tree, { types: ["INSTANCE"] }, { limit: 2 });
  assert.equal(two.total, 3);
  assert.equal(two.matches.length, 2);
  assert.equal(two.truncated, true);
});

test("name and type combine, and a bad colour is rejected", async () => {
  assert.equal((await findNodes(tree, { types: ["INSTANCE"], name: "BTN i:2" })).total, 1);
  assert.throws(() => normalizeColor("red"), TypeError);
});
