// Methods that change the document.
//
// Writes are held to a stricter standard than reads: a bad read wastes a call,
// a bad write damages someone's file. Every handler validates before touching
// anything, names what it created so the user can find it in the layer list,
// and returns enough for the caller to address the node afterwards.

/** Thrown when params are present but unusable. */
class Invalid extends TypeError {}

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

export const writeHandlers: Record<
  string,
  (params: Record<string, unknown>) => Promise<unknown>
> = {
  "figma/createImage": createImage,
};
