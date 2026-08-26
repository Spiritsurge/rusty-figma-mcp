// What the AI is doing, in words a designer can read.
//
// The panel is the only place a user can see an agent touching their file, so
// method names are translated rather than displayed raw. `figma/getVariableDefs`
// means nothing to someone who has not read the protocol; "Reading design
// tokens" does.

export interface Activity {
  id: number;
  method: string;
  label: string;
  /** 0–100, from a $/progress notification. */
  pct?: number;
  note?: string;
  startedAt: number;
  /** Set once the operation finishes. */
  outcome?: "ok" | "error";
  ms?: number;
}

const LABELS: Record<string, string> = {
  "figma/getMetadata": "Checking the file",
  "figma/getPages": "Listing pages",
  "figma/getSelection": "Reading your selection",
  "figma/getDocument": "Reading the document",
  "figma/getStyles": "Reading styles",
  "figma/getVariableDefs": "Reading design tokens",
  "figma/createImage": "Placing an image",
  "figma/cloneNode": "Duplicating a layer",
  "figma/deleteNodes": "Deleting layers",
  "figma/setText": "Changing text",
};

/** A short, human phrase for one request. */
export function describe(method: string, params?: Record<string, unknown>): string {
  // The two methods that name a target read better with it included.
  if (method === "figma/getNode") {
    const id = typeof params?.node_id === "string" ? params.node_id : undefined;
    return id ? `Reading node ${id}` : "Reading a node";
  }
  if (method === "figma/getScreenshot") {
    const id = typeof params?.node_id === "string" ? params.node_id : undefined;
    return id ? `Rendering ${id}` : "Rendering this page";
  }

  return (
    LABELS[method] ??
    // An unknown method is still worth showing: a newer server may send
    // something this plugin has no label for, and silence would be worse than
    // an ugly name.
    method.replace(/^figma\//, "").replace(/([A-Z])/g, " $1").toLowerCase()
  );
}

/** "1.4s", "820ms" — short enough to sit at the end of a row. */
export function formatDuration(ms: number): string {
  return ms < 1000 ? `${ms}ms` : `${(ms / 1000).toFixed(1)}s`;
}
