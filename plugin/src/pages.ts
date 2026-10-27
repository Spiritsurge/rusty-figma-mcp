// Page access helpers shared by the read and write handlers.

/** The page a node lives on, or null for the document root itself. */
export function owningPage(node: BaseNode): PageNode | null {
  let cursor: BaseNode | null = node;
  while (cursor && cursor.type !== "PAGE") cursor = cursor.parent;
  return cursor as PageNode | null;
}

/**
 * Under dynamic-page access a node on a page other than the current one cannot
 * be read or changed until that page is loaded. Without this, a call works only
 * for the page the user has open and otherwise hangs until they switch to it.
 */
export async function loadOwningPage(node: BaseNode): Promise<void> {
  await owningPage(node)?.loadAsync();
}
