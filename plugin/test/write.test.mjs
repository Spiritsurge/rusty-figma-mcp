// Page creation and cross-page cloning, against a stub of the parts of the
// Figma API the handlers touch.

import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

let nextId;
let pages;
let nodes;
let loaded;

function makePage(name) {
  const page = {
    id: `P:${nextId++}`,
    name,
    type: "PAGE",
    parent: null,
    children: [],
    selection: [],
    appendChild(child) {
      child.parent?.children.splice(child.parent.children.indexOf(child), 1);
      child.parent = page;
      page.children.push(child);
    },
    loadAsync: async () => {
      loaded.push(page.id);
    },
  };
  nodes.set(page.id, page);
  return page;
}

function makeFrame(parent, name, x, y) {
  const frame = {
    id: `F:${nextId++}`,
    name,
    type: "FRAME",
    x,
    y,
    width: 1920,
    height: 1080,
    parent,
    children: [],
    appendChild(child) {
      child.parent?.children.splice(child.parent.children.indexOf(child), 1);
      child.parent = frame;
      frame.children.push(child);
    },
    clone() {
      const copy = makeFrame(this.parent, this.name, this.x, this.y);
      return copy;
    },
  };
  parent.children.push(frame);
  nodes.set(frame.id, frame);
  return frame;
}

let currentPage;
let root;

beforeEach(() => {
  nextId = 1;
  nodes = new Map();
  loaded = [];
  currentPage = makePage("UI kit");
  root = {
    children: [currentPage],
    insertChild(index, page) {
      this.children.splice(this.children.indexOf(page), 1);
      this.children.splice(index, 0, page);
    },
  };
  pages = root.children;
  globalThis.figma = {
    mixed: Symbol("mixed"),
    root,
    get currentPage() {
      return currentPage;
    },
    createPage() {
      const page = makePage("Page");
      root.children.push(page);
      return page;
    },
    getNodeByIdAsync: async (id) => nodes.get(id) ?? null,
  };
});

const { writeHandlers } = await import("../src/write-handlers.ts");

test("create_page adds a named page at the end and does not switch to it", async () => {
  const out = await writeHandlers["figma/createPage"]({ name: "Homepage options" });
  assert.equal(out.name, "Homepage options");
  assert.equal(out.index, 1);
  assert.equal(pages.length, 2);
  assert.equal(currentPage.name, "UI kit");
});

test("create_page honours an index and refuses a taken name or a bad index", async () => {
  const out = await writeHandlers["figma/createPage"]({ name: "First", index: 0 });
  assert.equal(out.index, 0);
  assert.equal(pages[0].name, "First");
  await assert.rejects(writeHandlers["figma/createPage"]({ name: "First" }), /already exists/);
  await assert.rejects(writeHandlers["figma/createPage"]({ name: "X", index: 9 }), /index must be/);
  await assert.rejects(writeHandlers["figma/createPage"]({}), /name is required/);
});

test("clone_node with parent_id puts the copy on another page and keeps the original", async () => {
  const source = makePage("First concepts");
  const original = makeFrame(source, "Homepage 1", 100, 200);
  const target = (await writeHandlers["figma/createPage"]({ name: "Homepage options" })).id;

  const out = await writeHandlers["figma/cloneNode"]({
    node_id: original.id,
    parent_id: target,
    name: "[First concepts] Homepage 1",
  });

  assert.equal(out.name, "[First concepts] Homepage 1");
  assert.equal(out.pageId, target);
  assert.equal(out.x, 100);
  assert.equal(out.y, 200);
  assert.equal(source.children.length, 1, "the original stays where it was");
  assert.equal(original.name, "Homepage 1");
  assert.equal(nodes.get(target).children.length, 1);
  assert.ok(loaded.includes(source.id) && loaded.includes(target), "both pages were loaded first");
  assert.deepEqual(currentPage.selection, [], "selecting across pages is skipped");
});

test("clone_node without parent_id still copies beside the original and selects it", async () => {
  const original = makeFrame(currentPage, "A", 0, 0);
  const out = await writeHandlers["figma/cloneNode"]({ node_id: original.id, x: 50 });
  assert.equal(currentPage.children.length, 2);
  assert.equal(out.x, 50);
  assert.equal(currentPage.selection.length, 1);
});

test("clone_node refuses a parent that cannot hold children, and a missing one", async () => {
  const original = makeFrame(currentPage, "A", 0, 0);
  nodes.set("T:1", { id: "T:1", type: "TEXT", name: "t", parent: currentPage });
  await assert.rejects(writeHandlers["figma/cloneNode"]({ node_id: original.id, parent_id: "T:1" }), /cannot hold children/);
  await assert.rejects(writeHandlers["figma/cloneNode"]({ node_id: original.id, parent_id: "9:9" }), /no node with id 9:9/);
  assert.equal(currentPage.children.length, 1, "nothing was cloned on a rejected call");
});
