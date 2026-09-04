import { useCallback, useEffect, useRef, useState } from "react";

import { PROTOCOL_VERSION } from "../protocol";
import { type Activity, describe } from "./activity";
import type { Session } from "./useSessions";

/** How many finished operations to keep on screen. */
const HISTORY_LIMIT = 6;

export type LinkState = "idle" | "connecting" | "connected" | "lost" | "ended";

const RECONNECT_DELAY_MS = 1500;

/**
 * Whether the server on this port is still the one the user chose.
 *
 * A port is not an identity. If the picked server dies and another takes its
 * slot, reconnecting blindly would hand the user's session to a process they
 * never selected — silently transferring the authorization gesture that
 * picking a session rests on. pid and start time together identify the process.
 */
async function isSameServer(session: Session): Promise<boolean> {
  try {
    const response = await fetch(`http://localhost:${session.port}/hello`);
    if (!response.ok) return false;
    const identity = await response.json();
    return identity.pid === session.pid && identity.started_at_ms === session.started_at_ms;
  } catch {
    return false;
  }
}

/**
 * Holds the socket and relays between it and the plugin main thread.
 *
 * The iframe is the only half with network access, and the main thread is the
 * only half with `figma.*`, so every frame makes the same round trip:
 * socket -> postMessage -> handler -> postMessage -> socket.
 */
export function useLink(session: Session | null) {
  const [state, setState] = useState<LinkState>("idle");
  const [active, setActive] = useState<Activity[]>([]);
  const [history, setHistory] = useState<Activity[]>([]);
  const socketRef = useRef<WebSocket | null>(null);
  const reconnectRef = useRef<number | null>(null);

  const disconnect = useCallback(() => {
    if (reconnectRef.current !== null) {
      clearTimeout(reconnectRef.current);
      reconnectRef.current = null;
    }
    const socket = socketRef.current;
    if (socket) {
      // Detach first: otherwise this close fires onclose, which would schedule
      // a reconnect to a session we are deliberately leaving.
      socket.onclose = null;
      socket.close();
      socketRef.current = null;
    }
    setState("idle");
    setActive([]);
  }, []);

  useEffect(() => {
    if (!session) {
      disconnect();
      return;
    }

    let cancelled = false;

    const connect = async () => {
      if (cancelled) return;
      setState("connecting");

      // Verify identity before every attempt, including the first: the picker's
      // list can be a few seconds stale.
      if (!(await isSameServer(session))) {
        if (cancelled) return;
        setState("ended");
        socketRef.current = null;
        return;
      }
      if (cancelled) return;

      const socket = new WebSocket(
        `ws://localhost:${session.port}/link?v=${PROTOCOL_VERSION}`,
      );
      socketRef.current = socket;

      socket.onopen = () => {
        if (cancelled) return;
        setState("connected");
      };

      socket.onmessage = (event) => {
        try {
          const frame = JSON.parse(event.data as string);
          if (typeof frame.id === "number" && typeof frame.method === "string") {
            setActive((current) => [
              ...current,
              {
                id: frame.id,
                method: frame.method,
                label: describe(frame.method, frame.params),
                startedAt: Date.now(),
              },
            ]);
          }
          parent.postMessage({ pluginMessage: { kind: "request", frame } }, "*");
        } catch {
          // A frame we cannot parse is the server's problem, not ours; dropping
          // it keeps the socket alive for the frames that follow.
        }
      };

      socket.onclose = () => {
        if (cancelled || socketRef.current !== socket) return;
        socketRef.current = null;
        setState("lost");
        setActive([]);
        reconnectRef.current = window.setTimeout(() => void connect(), RECONNECT_DELAY_MS);
      };

      socket.onerror = () => {
        // onclose always follows; reconnecting is handled there so it happens
        // exactly once.
      };
    };

    void connect();

    return () => {
      cancelled = true;
      disconnect();
    };
  }, [session, disconnect]);

  // Replies travelling the other way: main thread -> socket.
  useEffect(() => {
    const onMessage = (event: MessageEvent) => {
      const message = event.data?.pluginMessage;
      if (message?.kind !== "reply") return;

      const socket = socketRef.current;
      if (socket?.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify(message.frame));
      }
      const frame = message.frame;

      // Progress is not terminal: it updates the row rather than closing it.
      if (frame?.method === "$/progress") {
        const { id, pct, note } = frame.params ?? {};
        setActive((current) =>
          current.map((a) => (a.id === id ? { ...a, pct, note } : a)),
        );
        return;
      }

      if (typeof frame?.id !== "number") return;

      setActive((current) => {
        const finished = current.find((a) => a.id === frame.id);
        if (finished) {
          setHistory((past) =>
            [
              {
                ...finished,
                outcome: frame.error ? ("error" as const) : ("ok" as const),
                ms: Date.now() - finished.startedAt,
                note: frame.error?.message,
              },
              ...past,
            ].slice(0, HISTORY_LIMIT),
          );
        }
        return current.filter((a) => a.id !== frame.id);
      });
    };

    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, []);

  return { state, active, history };
}
