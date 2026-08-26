// Plugin main thread.
//
// Has the figma.* API and no network (PROTOCOL.md C1), so it does the work and
// the UI iframe carries the socket. Everything crosses by postMessage.

import { handlers, NotFound } from "./handlers";
import { codes, err, ok, progress, type ToMain, type ToUi } from "./protocol";

const UI_SIZE = { width: 340, height: 320 };

function send(message: ToUi): void {
  figma.ui.postMessage(message);
}

function sendStatus(): void {
  send({
    kind: "status",
    fileName: figma.root.name,
    pageName: figma.currentPage.name,
    selectionCount: figma.currentPage.selection.length,
  });
}

async function dispatch(frame: { id: number; method: string; params?: unknown }): Promise<void> {
  const handler = handlers[frame.method];
  if (!handler) {
    send({ kind: "reply", frame: err(frame.id, codes.METHOD_NOT_FOUND, `unknown method ${frame.method}`) });
    return;
  }

  const params = (frame.params ?? {}) as Record<string, unknown>;
  const emit = (pct: number, note?: string) =>
    send({ kind: "reply", frame: progress(frame.id, pct, note) });

  try {
    const result = await handler(params, emit);
    send({ kind: "reply", frame: ok(frame.id, result) });
  } catch (error) {
    send({ kind: "reply", frame: err(frame.id, classify(error), describe(error)) });
  }
}

function classify(error: unknown): number {
  if (error instanceof NotFound) return codes.NOT_FOUND;
  if (error instanceof TypeError) return codes.INVALID_PARAMS;
  return codes.HOST_ERROR;
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

figma.showUI(__html__, { ...UI_SIZE, themeColors: true });

figma.ui.onmessage = (message: ToMain) => {
  switch (message?.kind) {
    case "ui-ready":
      sendStatus();
      break;
    case "request":
      void dispatch(message.frame);
      break;
  }
};

// The UI shows which file and selection it is attached to, so both need to stay
// current — picking the wrong session is the mistake this display prevents.
figma.on("selectionchange", sendStatus);
figma.on("currentpagechange", sendStatus);

sendStatus();
