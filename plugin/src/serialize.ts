// Figma nodes into plain JSON.
//
// Three things make this less mechanical than it looks:
//
//   - A property whose value differs across a selection or across characters
//     in a text node reads as `figma.mixed`, a Symbol. JSON.stringify turns a
//     Symbol into undefined and silently drops the key, so mixed values are
//     converted explicitly.
//   - Node trees are unbounded. Depth is capped by the caller and the cap is
//     reported in the output, so a consumer can tell a leaf from a truncation.
//   - Reading every property of every node is slow on a large file. Only the
//     properties an agent can actually use are read.

/** Figma's mixed sentinel, as something JSON can carry. */
export const MIXED = "mixed" as const;

function plain<T>(value: T | typeof figma.mixed): T | typeof MIXED | undefined {
  if (value === figma.mixed) return MIXED;
  return value as T;
}

/** Round to 2dp. Figma's floats carry noise that bloats output and helps nobody. */
function round(n: number): number {
  return Math.round(n * 100) / 100;
}

function has<K extends string>(node: object, key: K): node is object & Record<K, unknown> {
  return key in node;
}

/** Two hex digits from a Figma channel, which runs 0..1 rather than 0..255. */
function channel(value: number): string {
  return Math.round(Math.max(0, Math.min(1, value)) * 255)
    .toString(16)
    .padStart(2, "0");
}

/**
 * Figma colours as CSS hex.
 *
 * The raw form is three or four floats — `0.08235294371843338` for one
 * channel — which is both unreadable and a large share of the payload on a
 * document with many fills. Hex is what a consumer writing CSS actually wants,
 * and it is lossless at 8-bit, which is the precision Figma renders at anyway.
 */
export function toHex(color: RGB | RGBA): string {
  const hex = `#${channel(color.r)}${channel(color.g)}${channel(color.b)}`;
  const alpha = (color as RGBA).a;
  return alpha === undefined || alpha >= 1 ? hex : `${hex}${channel(alpha)}`;
}

/** Strip keys whose value is the Figma default and therefore carries nothing. */
function compact<T extends Record<string, unknown>>(object: T): T {
  for (const key of Object.keys(object)) {
    const value = object[key];
    if (
      value === undefined ||
      (key === "visible" && value === true) ||
      (key === "blendMode" && value === "NORMAL") ||
      (key === "opacity" && value === 1) ||
      (value !== null && typeof value === "object" && Object.keys(value).length === 0)
    ) {
      delete object[key];
    }
  }
  return object;
}

function serializePaint(paint: Paint): Record<string, unknown> {
  const base: Record<string, unknown> = {
    type: paint.type,
    visible: paint.visible,
    opacity: paint.opacity,
    blendMode: paint.blendMode,
  };

  if (paint.type === "SOLID") {
    base.color = toHex(paint.color);
  } else if (paint.type === "IMAGE") {
    base.scaleMode = paint.scaleMode;
    base.imageHash = paint.imageHash;
  } else if (paint.type === "VIDEO") {
    base.scaleMode = paint.scaleMode;
  } else if (
    paint.type === "GRADIENT_LINEAR" ||
    paint.type === "GRADIENT_RADIAL" ||
    paint.type === "GRADIENT_ANGULAR" ||
    paint.type === "GRADIENT_DIAMOND"
  ) {
    base.gradientStops = paint.gradientStops.map((stop) => ({
      position: Math.round(stop.position * 1000) / 1000,
      color: toHex(stop.color),
    }));
    base.gradientTransform = paint.gradientTransform;
  }
  // PATTERN and SHADER paints carry no colour of their own to convert; the
  // type alone is what a consumer can act on.

  return compact(base);
}

export function serializePaints(paints: readonly Paint[]): Record<string, unknown>[] {
  return paints.map(serializePaint);
}

export function serializeEffects(effects: readonly Effect[]): Record<string, unknown>[] {
  return effects.map((effect) => {
    const base: Record<string, unknown> = {
      type: effect.type,
      visible: effect.visible,
      radius: (effect as BlurEffect).radius,
    };
    if (effect.type === "DROP_SHADOW" || effect.type === "INNER_SHADOW") {
      base.color = toHex(effect.color);
      base.offset = { x: round(effect.offset.x), y: round(effect.offset.y) };
      base.spread = effect.spread || undefined;
      base.blendMode = effect.blendMode;
    }
    return compact(base);
  });
}

export interface SerializeOptions {
  /** Levels of children to include. 0 means this node only. */
  depth?: number;
}

export function serializeNode(
  node: BaseNode,
  options: SerializeOptions = {},
  currentDepth = 0,
): Record<string, unknown> {
  const depth = options.depth ?? Infinity;
  const out: Record<string, unknown> = {
    id: node.id,
    name: node.name,
    type: node.type,
  };

  if (has(node, "visible")) out.visible = node.visible;
  if (has(node, "locked") && node.locked) out.locked = true;

  // Geometry
  if (has(node, "x") && has(node, "y")) {
    out.x = round(node.x as number);
    out.y = round(node.y as number);
  }
  if (has(node, "width") && has(node, "height")) {
    out.width = round(node.width as number);
    out.height = round(node.height as number);
  }
  if (has(node, "rotation") && node.rotation !== 0) out.rotation = round(node.rotation as number);
  if (has(node, "opacity") && node.opacity !== 1) out.opacity = round(node.opacity as number);
  if (has(node, "blendMode") && node.blendMode !== "PASS_THROUGH") out.blendMode = node.blendMode;

  // Paint
  if (has(node, "fills")) {
    const fills = plain(node.fills as Paint[] | typeof figma.mixed);
    if (fills === MIXED) out.fills = MIXED;
    else if (fills !== undefined && fills.length > 0) out.fills = serializePaints(fills);
  }
  if (has(node, "strokes") && (node.strokes as Paint[]).length > 0) {
    out.strokes = serializePaints(node.strokes as Paint[]);
    if (has(node, "strokeWeight")) out.strokeWeight = plain(node.strokeWeight as number);
  }
  if (has(node, "effects") && (node.effects as Effect[]).length > 0) {
    out.effects = serializeEffects(node.effects as Effect[]);
  }

  if (has(node, "cornerRadius")) {
    const radius = plain(node.cornerRadius as number | typeof figma.mixed);
    if (radius !== undefined && radius !== 0) out.cornerRadius = radius;
  }

  // Auto layout — the part that actually maps onto CSS, so it is always
  // included when present.
  if (has(node, "layoutMode") && node.layoutMode !== "NONE") {
    out.layout = {
      mode: node.layoutMode,
      primaryAxisAlignItems: (node as unknown as FrameNode).primaryAxisAlignItems,
      counterAxisAlignItems: (node as unknown as FrameNode).counterAxisAlignItems,
      itemSpacing: round((node as unknown as FrameNode).itemSpacing),
      paddingTop: (node as unknown as FrameNode).paddingTop,
      paddingRight: (node as unknown as FrameNode).paddingRight,
      paddingBottom: (node as unknown as FrameNode).paddingBottom,
      paddingLeft: (node as unknown as FrameNode).paddingLeft,
    };
  }

  if (node.type === "TEXT") {
    const text = node as TextNode;
    out.characters = text.characters;
    out.fontSize = plain(text.fontSize);
    out.fontName = plain(text.fontName);
    out.textAlignHorizontal = text.textAlignHorizontal;
    const spacing = plain(text.letterSpacing);
    if (spacing !== undefined) out.letterSpacing = spacing;
    const lineHeight = plain(text.lineHeight);
    if (lineHeight !== undefined) out.lineHeight = lineHeight;
  }

  if (node.type === "INSTANCE") {
    // mainComponent is async under dynamic-page access; the id is enough to
    // correlate an instance with its component without paying for the load.
    out.componentId = (node as InstanceNode).mainComponent?.id ?? null;
  }

  // Style bindings, which are what tie a node back to the design system.
  for (const key of ["fillStyleId", "strokeStyleId", "textStyleId", "effectStyleId"] as const) {
    if (has(node, key)) {
      const id = plain(node[key] as string | typeof figma.mixed);
      if (id) out[key] = id;
    }
  }

  if ("children" in node) {
    const children = (node as ChildrenMixin).children;
    if (currentDepth >= depth) {
      // Distinguishable from a genuine leaf, which has no childCount at all.
      out.childCount = children.length;
      out.truncated = true;
    } else {
      out.children = children.map((child) => serializeNode(child, options, currentDepth + 1));
    }
  }

  return out;
}

/** Paint, text, effect and grid styles, flattened into one list. */
export async function serializeStyles(): Promise<Record<string, unknown>> {
  const [paint, text, effect, grid] = await Promise.all([
    figma.getLocalPaintStylesAsync(),
    figma.getLocalTextStylesAsync(),
    figma.getLocalEffectStylesAsync(),
    figma.getLocalGridStylesAsync(),
  ]);

  const common = (s: BaseStyle) => ({
    id: s.id,
    name: s.name,
    description: s.description || undefined,
  });

  return {
    paint: paint.map((s) => ({ ...common(s), paints: serializePaints(s.paints) })),
    text: text.map((s) => ({
      ...common(s),
      fontName: s.fontName,
      fontSize: s.fontSize,
      lineHeight: s.lineHeight,
      letterSpacing: s.letterSpacing,
    })),
    effect: effect.map((s) => ({ ...common(s), effects: serializeEffects(s.effects) })),
    grid: grid.map((s) => ({ ...common(s), layoutGrids: s.layoutGrids })),
  };
}

/** Variable collections and their values per mode — design tokens. */
export async function serializeVariables(): Promise<Record<string, unknown>> {
  const collections = await figma.variables.getLocalVariableCollectionsAsync();

  const out = await Promise.all(
    collections.map(async (collection) => {
      const variables = await Promise.all(
        collection.variableIds.map((id) => figma.variables.getVariableByIdAsync(id)),
      );

      return {
        id: collection.id,
        name: collection.name,
        modes: collection.modes.map((m) => ({ id: m.modeId, name: m.name })),
        defaultModeId: collection.defaultModeId,
        variables: variables
          .filter((v): v is Variable => v !== null)
          .map((v) => ({
            id: v.id,
            name: v.name,
            type: v.resolvedType,
            description: v.description || undefined,
            valuesByMode: v.valuesByMode,
          })),
      };
    }),
  );

  return { collections: out };
}
