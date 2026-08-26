import { useEffect, useState } from "react";

import { useLink } from "./useLink";
import { type Session, useSessions } from "./useSessions";

interface Status {
  fileName: string;
  pageName: string;
  selectionCount: number;
}

export default function App() {
  const { sessions, scanning, rescan } = useSessions();
  const [selected, setSelected] = useState<Session | null>(null);
  const { state, inFlight } = useLink(selected);
  const [status, setStatus] = useState<Status | null>(null);

  useEffect(() => {
    const onMessage = (event: MessageEvent) => {
      const message = event.data?.pluginMessage;
      if (message?.kind === "status") {
        setStatus({
          fileName: message.fileName,
          pageName: message.pageName,
          selectionCount: message.selectionCount,
        });
      }
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, []);

  // A session that has gone away should not stay selected: the reconnect loop
  // would retry a port nothing is listening on.
  useEffect(() => {
    if (selected && !sessions.some((s) => s.port === selected.port)) {
      setSelected(null);
    }
  }, [sessions, selected]);

  return (
    <div className="app">
      <header>
        <div className="file" title={status?.fileName}>
          {status?.fileName ?? "—"}
        </div>
        <div className="sub">
          {status?.pageName ?? "—"} · {status?.selectionCount ?? 0} selected
        </div>
      </header>

      {selected ? (
        <Connected
          session={selected}
          state={state}
          inFlight={inFlight}
          onDisconnect={() => setSelected(null)}
        />
      ) : (
        <Picker
          sessions={sessions}
          scanning={scanning}
          onRescan={rescan}
          onPick={setSelected}
        />
      )}

      <footer>
        <span className="product">Rusty Figma MCP</span>
        <span className="author">made by Spiritsurge</span>
      </footer>
    </div>
  );
}

function Picker({
  sessions,
  scanning,
  onRescan,
  onPick,
}: {
  sessions: Session[];
  scanning: boolean;
  onRescan: () => void;
  onPick: (s: Session) => void;
}) {
  return (
    <>
      <div className="section-label">
        Sessions
        <button className="link-button" onClick={onRescan} disabled={scanning}>
          {scanning ? "scanning…" : "rescan"}
        </button>
      </div>

      {sessions.length === 0 ? (
        <div className="empty">
          {scanning ? (
            "Looking for servers…"
          ) : (
            <>
              <p>No server found.</p>
              <p className="hint">
                Start your MCP client with <code>figma-mcp</code> configured, then rescan.
              </p>
            </>
          )}
        </div>
      ) : (
        <ul className="sessions">
          {sessions.map((session) => (
            <li key={session.port}>
              <button className="session" onClick={() => onPick(session)}>
                <span className="label">{session.label}</span>
                {session.connected ? (
                  <span className="in-use" title="Another Figma window is using this session">
                    in use
                  </span>
                ) : (
                  <span className="port">:{session.port}</span>
                )}
              </button>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

function Connected({
  session,
  state,
  inFlight,
  onDisconnect,
}: {
  session: Session;
  state: string;
  inFlight: number;
  onDisconnect: () => void;
}) {
  return (
    <div className="connected">
      <div className={`badge ${state}`}>
        <span className="dot" />
        {state === "connected"
          ? inFlight > 0
            ? `working — ${inFlight} request${inFlight > 1 ? "s" : ""}`
            : "connected"
          : state === "connecting"
            ? "connecting…"
            : "connection lost — retrying"}
      </div>

      <div className="target">
        <div className="label">{session.label}</div>
        <div className="port">localhost:{session.port}</div>
      </div>

      <button className="disconnect" onClick={onDisconnect}>
        Disconnect
      </button>
    </div>
  );
}
