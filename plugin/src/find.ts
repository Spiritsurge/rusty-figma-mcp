// Search a subtree for nodes, by what an agent actually asks about a design
// system: where is this colour, style, variable or component used?
//
// It returns ids and paths, not subtrees. get_node on an id gives the detail;
// this is for finding which ids to look at, and for counting uses.

import { toHex } from "./serialize.ts";

export interface Criteria {
  /** Node types to keep, e.g. ["INSTANCE", "TEXT"]. */
  types?: string[];
  /** Case-insensitive substring of the layer name. */
  name?: string;
  /** A solid fill colour, as #rrggbb. Alpha is ignored. */
  fillColor?: string;
  /** A solid stroke colour, as #rrggbb. Alpha is ignored. */
  strokeColor?: string;
  /** A paint, text, stroke or effect style id applied to the node. */
  styleId?: string;
  /** A variable id bound to any field, paint or effect of the node. */
  variableId?: string;
  /** Instances of this component (or of a variant in this component set). */
  componentId?: string;
}

export interface Match {
  id: string;
  name: string;
  type: string;
  /** Layer names from the page down to the node's parent, joined by " / ". */
  path: string;
}

export interface FindResult {
  /** Every node that matched, which can exceed the matches returned. */
  total: number;
  matches: Match[];
  truncated: boolean;
  /** Nodes examined, so an empty result can be told from an empty search. */
  scanned: number;
}

/** "#F53F3F", "f53f3f" and "#f53f3fcc" all name the same colour. */
export function normalizeColor(input: string): string {
  const hex = input.trim().toLowerCase().replace(/^#/, "");
  if (!/^([0-9a-f]{6}|[0-9a-f]{8})$/.test(hex)) {
    throw new TypeError(`"${input}" is not a hex colour; use #rrggbb`);
  }
  return `#${hex.slice(0, 6)}`;
}

function hasSolid(paints: unknown, color: string): boolean {
  if (!Array.isArray(paints)) return false; // figma.mixed is a symbol
  return paints.some(
    (paint: Paint) =>
      paint.type === "SOLID" && paint.visible !== false && toHex(paint.color).slice(0, 7) === color,
  );
}

function boundIds(node: object): Set<string> {
  const ids = new Set<string>();
  const add = (value: unknown) => {
    if (Array.isArray(value)) value.forEach(add);
    else if (value && typeof value === "object") {
      const id = (value as { id?: unknown }).id;
      if (typeof id === "string") ids.add(id);
      else Object.values(value).forEach(add); // componentProperties nests one level
    }
  };
  const n = node as { boundVariables?: unknown; fills?: unknown; strokes?: unknown; effects?: unknown };
  add(n.boundVariables);
  for (const list of [n.fills, n.strokes, n.effects]) {
    if (!Array.isArray(list)) continue;
    for (const item of list) add((item as { boundVariables?: unknown }).boundVariables);
  }
  return ids;
}

function appliedStyleIds(node: object): string[] {
  const n = node as Record<string, unknown>;
  return ["fillStyleId", "strokeStyleId", "textStyleId", "effectStyleId"]
    .map((key) => n[key])
    .filter((id): id is string => typeof id === "string" && id !== "");
}

/**
 * Whether one node satisfies every given criterion. The component criterion is
 * resolved by the caller, which has to await it, and passed in as `mainId` and
 * `setId`.
 */
export function matches(
  node: BaseNode,
  criteria: Criteria,
  main?: { id: string; setId?: string } | null,
): boolean {
  const { types, name, fillColor, strokeColor, styleId, variableId, componentId } = criteria;

  if (types && types.length > 0 && !types.includes(node.type)) return false;
  if (name && !node.name.toLowerCase().includes(name.toLowerCase())) return false;
  if (fillColor && !("fills" in node && hasSolid(node.fills, fillColor))) return false;
  if (strokeColor && !("strokes" in node && hasSolid(node.strokes, strokeColor))) return false;
  if (styleId && !appliedStyleIds(node).includes(styleId)) return false;
  if (variableId && !boundIds(node).has(variableId)) return false;
  if (componentId) {
    if (node.type !== "INSTANCE" || !main) return false;
    if (main.id !== componentId && main.setId !== componentId) return false;
  }
  return true;
}

export interface FindOptions {
  /** Matches to return. The total is always counted in full. Default 200. */
  limit?: number;
}

/** Walk `root` and everything beneath it. */
export async function findNodes(
  root: BaseNode,
  criteria: Criteria,
  options: FindOptions = {},
): Promise<FindResult> {
  const limit = options.limit ?? 200;
  const result: FindResult = { total: 0, matches: [], truncated: false, scanned: 0 };

  const visit = async (node: BaseNode, path: string[]): Promise<void> => {
    result.scanned++;

    let main: { id: string; setId?: string } | null = null;
    if (criteria.componentId && node.type === "INSTANCE") {
      const component = await (node as InstanceNode).getMainComponentAsync();
      if (component) {
        const parent = component.parent;
        main = { id: component.id, setId: parent?.type === "COMPONENT_SET" ? parent.id : undefined };
      }
    }

    if (matches(node, criteria, main)) {
      result.total++;
      if (result.matches.length < limit) {
        result.matches.push({ id: node.id, name: node.name, type: node.type, path: path.join(" / ") });
      } else {
        result.truncated = true;
      }
    }

    if ("children" in node) {
      const here = [...path, node.name];
      for (const child of (node as ChildrenMixin).children) await visit(child, here);
    }
  };

  // The root itself is a candidate; its own name starts the path of its children.
  await visit(root, []);
  return result;
}
