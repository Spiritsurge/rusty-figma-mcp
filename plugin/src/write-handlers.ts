// Methods that change the document.
//
// Writes are held to a stricter standard than reads: a bad read wastes a call,
// a bad write damages someone's file. Every handler validates before touching
// anything, names what it created so the user can find it in the layer list,
// and returns enough for the caller to address the node afterwards.

import { serializeNode } from "./serialize";

/** Thrown when params are present but unusable. */
class Invalid extends TypeError {}

/** Thrown when an id does not resolve. Mirrors handlers.ts NotFound. */
class Missing extends Error {}

function requireString(params: Record<string, unknown>, key: string): string {
  const value = params[key];
  if (typeof value !== "string" || value.length === 0) {
    throw new Invalid(`${key} is required and must be a non-empty string`);
  }
  return value;
}

function requireNumber(params: Record<string, unknown>, key: string): number {
  const value = params[key];
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new Invalid(`${key} is required and must be a finite number`);
  }
  return value;
}

function optionalNumber(params: Record<string, unknown>, key: string): number | undefined {
  const value = params[key];
  if (value === undefined || value === null) return undefined;
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) {
    throw new Invalid(`${key} must be a positive number`);
  }
  return value;
}

/**
 * Place an image on the current page as a rectangle with an image fill.
 *
 * Figma has no standalone image node: an image is a fill, and something has to
 * carry it. A rectangle sized to the image's own dimensions is the closest
 * thing to "just the picture".
 */
async function createImage(params: Record<string, unknown>): Promise<unknown> {
  const base64 = requireString(params, "base64");
  const x = requireNumber(params, "x");
  const y = requireNumber(params, "y");

  const bytes = figma.base64Decode(base64);
  const image = figma.createImage(bytes);
  const natural = await image.getSizeAsync();

  const width = optionalNumber(params, "width") ?? natural.width;
  const height = optionalNumber(params, "height") ?? natural.height;

  const rect = figma.createRectangle();
  rect.name = typeof params.name === "string" && params.name ? params.name : "Image";
  rect.x = x;
  rect.y = y;
  rect.resize(width, height);
  rect.fills = [{ type: "IMAGE", imageHash: image.hash, scaleMode: "FILL" }];

  figma.currentPage.appendChild(rect);

  // Selecting it is how the user finds what just appeared on a large canvas.
  figma.currentPage.selection = [rect];

  return {
    id: rect.id,
    name: rect.name,
    x: rect.x,
    y: rect.y,
    width: rect.width,
    height: rect.height,
    naturalWidth: natural.width,
    naturalHeight: natural.height,
    pageId: figma.currentPage.id,
  };
}

/**
 * Duplicate a node, keeping every child, style and effect intact.
 *
 * Cloning beats rebuilding: a reconstruction has to re-derive gradients,
 * effects, image hashes and auto-layout, and any property the serializer does
 * not carry is lost silently. A clone is exact by construction, and the caller
 * can then delete what it does not want.
 *
 * The clone's subtree is returned so the caller can address the copies — clone
 * ids are new, and nothing else reveals which copy corresponds to which
 * original.
 */
async function cloneNode(params: Record<string, unknown>): Promise<unknown> {
  const nodeId = requireString(params, "node_id");
  const node = await figma.getNodeByIdAsync(nodeId);
  if (!node) throw new Missing(`no node with id ${nodeId} in this file`);

  if (!("clone" in node) || typeof (node as SceneNode).clone !== "function") {
    throw new Invalid(`${node.type} nodes cannot be cloned`);
  }

  const clone = (node as SceneNode & { clone(): SceneNode }).clone();

  if (typeof params.name === "string" && params.name) clone.name = params.name;

  // Parent to the original's own parent, so the copy keeps its context rather
  // than landing at the page root when the original was nested.
  const parent = node.parent;
  if (parent && "appendChild" in parent) {
    (parent as ChildrenMixin & BaseNode).appendChild(clone);
  }

  const x = optionalNumberAllowingZero(params, "x");
  const y = optionalNumberAllowingZero(params, "y");
  if (x !== undefined) clone.x = x;
  if (y !== undefined) clone.y = y;

  figma.currentPage.selection = [clone];

  const depth = typeof params.depth === "number" ? params.depth : 2;
  return serializeNode(clone, { depth });
}

/** Like optionalNumber, but 0 is a legal coordinate. */
function optionalNumberAllowingZero(
  params: Record<string, unknown>,
  key: string,
): number | undefined {
  const value = params[key];
  if (value === undefined || value === null) return undefined;
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new Invalid(`${key} must be a finite number`);
  }
  return value;
}

/**
 * Remove nodes by id.
 *
 * An id that no longer resolves is reported rather than thrown: deleting a
 * batch where one entry was already gone has still achieved what was asked,
 * and failing the whole call would leave the caller unsure what was removed.
 */
async function deleteNodes(params: Record<string, unknown>): Promise<unknown> {
  const raw = params.node_ids;
  if (!Array.isArray(raw) || raw.length === 0) {
    throw new Invalid("node_ids is required and must be a non-empty array");
  }

  const removed: { id: string; name: string }[] = [];
  const missing: string[] = [];

  for (const entry of raw) {
    if (typeof entry !== "string") throw new Invalid("node_ids must contain only strings");
    const node = await figma.getNodeByIdAsync(entry);
    if (!node || node.removed) {
      missing.push(entry);
      continue;
    }
    removed.push({ id: node.id, name: node.name });
    node.remove();
  }

  return { removed, missing };
}

/**
 * Replace the contents of a text node.
 *
 * Figma refuses to mutate text whose font is not loaded, and a node with mixed
 * formatting carries several. Every font in the existing run is loaded first,
 * which is also what preserves that formatting: assigning `characters` keeps
 * the styling of the first run rather than resetting to a default.
 */
async function setText(params: Record<string, unknown>): Promise<unknown> {
  const nodeId = requireString(params, "node_id");
  const characters = params.characters;
  if (typeof characters !== "string") {
    throw new Invalid("characters is required and must be a string");
  }

  const node = await figma.getNodeByIdAsync(nodeId);
  if (!node) throw new Missing(`no node with id ${nodeId} in this file`);
  if (node.type !== "TEXT") {
    throw new Invalid(`${node.type} is not a text node; set_text needs a TEXT node`);
  }

  const text = node as TextNode;

  const fonts: FontName[] = [];
  if (text.characters.length > 0) {
    fonts.push(...text.getRangeAllFontNames(0, text.characters.length));
  } else if (text.fontName !== figma.mixed) {
    // An empty node has no range to inspect, but still has one font to load.
    fonts.push(text.fontName);
  }

  try {
    await Promise.all(fonts.map((font) => figma.loadFontAsync(font)));
  } catch (error) {
    const names = fonts.map((f) => `${f.family} ${f.style}`).join(", ");
    throw new Error(
      `could not load the fonts this text uses (${names}): ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }

  const before = text.characters;
  text.characters = characters;

  return {
    id: text.id,
    name: text.name,
    before,
    after: text.characters,
    width: text.width,
    height: text.height,
    autoResize: text.textAutoResize,
  };
}

export const writeHandlers: Record<
  string,
  (params: Record<string, unknown>) => Promise<unknown>
> = {
  "figma/createImage": createImage,
  "figma/cloneNode": cloneNode,
  "figma/deleteNodes": deleteNodes,
  "figma/setText": setText,
};
