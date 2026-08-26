// The figma/* methods. One function per method, each returning plain JSON.
//
// A handler throws to signal failure; main.ts turns that into a JSON-RPC error.
// Nothing here touches the socket — this file only knows about Figma.

import { serializeNode, serializeStyles, serializeVariables } from "./serialize";
import { writeHandlers } from "./write-handlers";

export type Emit = (pct: number, note?: string) => void;

type Handler = (params: Record<string, unknown>, emit: Emit) => Promise<unknown>;

/** Thrown when a node, page or style id does not resolve. */
export class NotFound extends Error {}

function requireString(params: Record<string, unknown>, key: string): string {
  const value = params[key];
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${key} is required and must be a string`);
  }
  return value;
}

function optionalDepth(params: Record<string, unknown>): number | undefined {
  const value = params.depth;
  if (value === undefined || value === null) return undefined;
  if (typeof value !== "number" || value < 0) throw new TypeError("depth must be a positive number");
  return value;
}

async function getMetadata(): Promise<unknown> {
  return {
    fileName: figma.root.name,
    pageName: figma.currentPage.name,
    pageId: figma.currentPage.id,
    selectionCount: figma.currentPage.selection.length,
    editorType: figma.editorType,
  };
}

async function getPages(): Promise<unknown> {
  return {
    currentPageId: figma.currentPage.id,
    pages: figma.root.children.map((page) => ({ id: page.id, name: page.name })),
  };
}

async function getSelection(params: Record<string, unknown>): Promise<unknown> {
  const depth = optionalDepth(params) ?? 2;
  const selection = figma.currentPage.selection;
  return {
    count: selection.length,
    nodes: selection.map((node) => serializeNode(node, { depth })),
  };
}

async function getNode(params: Record<string, unknown>): Promise<unknown> {
  const nodeId = requireString(params, "node_id");
  const node = await figma.getNodeByIdAsync(nodeId);
  if (!node) throw new NotFound(`no node with id ${nodeId} in this file`);
  return serializeNode(node, { depth: optionalDepth(params) });
}

async function getDocument(params: Record<string, unknown>, emit: Emit): Promise<unknown> {
  const depth = optionalDepth(params);

  // Under dynamic-page access, pages other than the current one are not loaded
  // until asked for. This is the slow part of the call, so it reports progress:
  // the server extends the deadline on each notification (PROTOCOL.md §6).
  const pages = figma.root.children;
  const serialized = [];
  for (let i = 0; i < pages.length; i++) {
    await pages[i].loadAsync();
    emit(Math.round(((i + 1) / pages.length) * 100), `loading ${pages[i].name}`);
    serialized.push(serializeNode(pages[i], { depth }));
  }

  return { name: figma.root.name, pages: serialized };
}

async function getStyles(): Promise<unknown> {
  return serializeStyles();
}

async function getVariableDefs(): Promise<unknown> {
  return serializeVariables();
}

async function getScreenshot(params: Record<string, unknown>): Promise<unknown> {
  const nodeId = typeof params.node_id === "string" ? params.node_id : undefined;
  const scale = typeof params.scale === "number" ? params.scale : 2;

  const target: BaseNode | null = nodeId
    ? await figma.getNodeByIdAsync(nodeId)
    : figma.currentPage;
  if (!target) throw new NotFound(`no node with id ${nodeId} in this file`);
  if (!("exportAsync" in target)) {
    throw new TypeError(`${target.type} nodes cannot be exported as an image`);
  }

  const bytes = await (target as ExportMixin).exportAsync({
    format: "PNG",
    constraint: { type: "SCALE", value: scale },
  });

  return {
    nodeId: target.id,
    name: target.name,
    format: "png",
    scale,
    base64: figma.base64Encode(bytes),
  };
}

export const handlers: Record<string, Handler> = {
  ...writeHandlers,
  "figma/getMetadata": getMetadata,
  "figma/getPages": getPages,
  "figma/getSelection": getSelection,
  "figma/getNode": getNode,
  "figma/getDocument": getDocument,
  "figma/getStyles": getStyles,
  "figma/getVariableDefs": getVariableDefs,
  "figma/getScreenshot": getScreenshot,
};
