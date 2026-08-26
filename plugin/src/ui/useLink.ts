import { useCallback, useEffect, useRef, useState } from "react";

import { PROTOCOL_VERSION } from "../protocol";
import type { Session } from "./useSessions";

export type LinkState = "idle" | "connecting" | "connected" | "lost";

const RECONNECT_DELAY_MS = 1500;

/**
 * Holds the socket and relays between it and the plugin main thread.
 *
 * The iframe is the only half with network access, and the main thread is the
 * only half with `figma.*`, so every frame makes the same round trip:
 * socket -> postMessage -> handler -> postMessage -> socket.
 */
export function useLink(session: Session | null) {
  const [state, setState] = useState<LinkState>("idle");
  const [inFlight, setInFlight] = useState(0);
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
    setInFlight(0);
  }, []);

  useEffect(() => {
    if (!session) {
      disconnect();
      return;
    }

    let cancelled = false;

    const connect = () => {
      if (cancelled) return;
      setState("connecting");

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
          setInFlight((n) => n + 1);
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
        setInFlight(0);
        reconnectRef.current = window.setTimeout(connect, RECONNECT_DELAY_MS);
      };

      socket.onerror = () => {
        // onclose always follows; reconnecting is handled there so it happens
        // exactly once.
      };
    };

    connect();

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
      // Progress notifications are not terminal, so they must not decrement.
      if (message.frame?.method !== "$/progress") {
        setInFlight((n) => Math.max(0, n - 1));
      }
    };

    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, []);

  return { state, inFlight };
}
