// The figma/* methods. One function per method, each returning plain JSON.
//
// A handler throws to signal failure; main.ts turns that into a JSON-RPC error.
// Nothing here touches the socket — this file only knows about Figma.

import { type Criteria, findNodes, normalizeColor } from "./find";
import { loadOwningPage } from "./pages";
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
    nodes: await Promise.all(selection.map((node) => serializeNode(node, { depth }))),
  };
}

async function getNode(params: Record<string, unknown>): Promise<unknown> {
  const nodeId = requireString(params, "node_id");
  const node = await figma.getNodeByIdAsync(nodeId);
  if (!node) throw new NotFound(`no node with id ${nodeId} in this file`);
  await loadOwningPage(node);
  return serializeNode(node, { depth: optionalDepth(params) });
}

async function getDocument(params: Record<string, unknown>, emit: Emit): Promise<unknown> {
  const depth = optionalDepth(params);

  // Under dynamic-page access, pages other than the current one are not loaded
  // until asked for. This is the slow part of the call, so it reports progress:
  // the server extends the deadline on each notification.
  const pages = figma.root.children;
  const serialized = [];
  for (let i = 0; i < pages.length; i++) {
    await pages[i].loadAsync();
    emit(Math.round(((i + 1) / pages.length) * 100), `loading ${pages[i].name}`);
    serialized.push(await serializeNode(pages[i], { depth }));
  }

  return { name: figma.root.name, pages: serialized };
}

function optionalString(params: Record<string, unknown>, key: string): string | undefined {
  const value = params[key];
  if (value === undefined || value === null || value === "") return undefined;
  if (typeof value !== "string") throw new TypeError(`${key} must be a string`);
  return value;
}

/**
 * Search for nodes by type, name, colour, style, variable or component.
 *
 * Scopes to `root_id` when given, otherwise the current page, or every page
 * with `all_pages`. Pages other than the open one are loaded first, which is
 * the slow part, so that path reports progress.
 */
async function findNodesHandler(params: Record<string, unknown>, emit: Emit): Promise<unknown> {
  const color = (key: string) => {
    const value = optionalString(params, key);
    return value === undefined ? undefined : normalizeColor(value);
  };
  const types = params.types;
  if (types !== undefined && !(Array.isArray(types) && types.every((t) => typeof t === "string"))) {
    throw new TypeError("types must be an array of strings");
  }
  const criteria: Criteria = {
    types: types as string[] | undefined,
    name: optionalString(params, "name"),
    fillColor: color("fill_color"),
    strokeColor: color("stroke_color"),
    styleId: optionalString(params, "style_id"),
    variableId: optionalString(params, "variable_id"),
    componentId: optionalString(params, "component_id"),
  };
  if (Object.values(criteria).every((v) => v === undefined)) {
    throw new TypeError("give at least one filter: types, name, fill_color, stroke_color, style_id, variable_id or component_id");
  }
  const limit = params.limit === undefined ? undefined : Number(params.limit);
  if (limit !== undefined && (!Number.isInteger(limit) || limit < 0)) {
    throw new TypeError("limit must be a non-negative integer");
  }

  const rootId = optionalString(params, "root_id");
  let roots: BaseNode[];
  if (rootId) {
    const node = await figma.getNodeByIdAsync(rootId);
    if (!node) throw new NotFound(`no node with id ${rootId} in this file`);
    await loadOwningPage(node);
    roots = [node];
  } else if (params.all_pages === true) {
    roots = [...figma.root.children];
  } else {
    roots = [figma.currentPage];
  }

  const total = { total: 0, matches: [] as unknown[], truncated: false, scanned: 0 };
  for (let i = 0; i < roots.length; i++) {
    if (roots[i].type === "PAGE") await (roots[i] as PageNode).loadAsync();
    if (roots.length > 1) emit(Math.round((i / roots.length) * 100), `scanning ${roots[i].name}`);
    const remaining = limit === undefined ? undefined : Math.max(0, limit - total.matches.length);
    const found = await findNodes(roots[i], criteria, { limit: remaining });
    total.total += found.total;
    total.scanned += found.scanned;
    total.matches.push(...found.matches);
    total.truncated ||= found.truncated;
  }
  return total;
}

async function getStyles(): Promise<unknown> {
  return serializeStyles();
}

async function getVariableDefs(): Promise<unknown> {
  return serializeVariables();
}

async function exportSvg(params: Record<string, unknown>): Promise<unknown> {
  const nodeId = requireString(params, "node_id");
  const node = await figma.getNodeByIdAsync(nodeId);
  if (!node) throw new NotFound(`no node with id ${nodeId} in this file`);
  if (!("exportAsync" in node)) {
    throw new TypeError(`${node.type} nodes cannot be exported as SVG`);
  }
  await loadOwningPage(node);

  // Text stays text only when asked: outlined glyphs render the same anywhere,
  // live text needs the font, so outlines are the safe default for an icon.
  const svg = await (node as ExportMixin).exportAsync({
    format: "SVG_STRING",
    svgOutlineText: params.outline_text !== false,
  });
  return { nodeId: node.id, name: node.name, svg };
}

async function getImageBytes(params: Record<string, unknown>): Promise<unknown> {
  const hash = requireString(params, "hash");
  const image = figma.getImageByHash(hash);
  if (!image) throw new NotFound(`no image with hash ${hash} in this file`);

  const [bytes, size] = await Promise.all([image.getBytesAsync(), image.getSizeAsync()]);
  return { hash, width: size.width, height: size.height, base64: figma.base64Encode(bytes) };
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
  "figma/findNodes": findNodesHandler,
  "figma/getStyles": getStyles,
  "figma/getVariableDefs": getVariableDefs,
  "figma/getScreenshot": getScreenshot,
  "figma/exportSvg": exportSvg,
  "figma/getImageBytes": getImageBytes,
};
