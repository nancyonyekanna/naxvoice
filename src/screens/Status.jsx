import { useEffect, useState } from "react";

// The landing screen answers "is it working, and what is it costing me".
//
// Everything here comes from Rust. Nothing is faked: a figure the app cannot
// actually measure yet is shown as "—" with a note, because a plausible
// invented number on a status screen is worse than an obvious gap.
export default function Status() {
  const [snapshot, setSnapshot] = useState(null);
  const [error, setError] = useState(null);

  useEffect(() => {
    const invoke = window.__TAURI__?.core?.invoke;
    if (!invoke) {
      setError("Not running inside the app, so there is nothing to report.");
      return;
    }
    invoke("status_snapshot").then(setSnapshot).catch((e) => setError(String(e)));
  }, []);

  if (error) {
    return (
      <>
        <Head state="offline" />
        <div className="empty">{error}</div>
      </>
    );
  }

  if (!snapshot) {
    return (
      <>
        <Head state="…" />
        <div className="empty">Reading the current state.</div>
      </>
    );
  }

  return (
    <>
      <Head state={snapshot.state} />

      <div className="tiles">
        {/* Mostly a fixed cost, not a per-dictation one: chunks transcribe
            while you speak, so on release only the tail is left. Cleanup and
            the deliberate waits dominate. Measured here: a 70-word dictation
            came back in 1926ms and a 3-word one took 4433ms. Saying "median"
            alone made it read as though long dictations were slow. */}
        <Tile
          k="Round trip"
          v={snapshot.round_trip_ms ? `${snapshot.round_trip_ms}ms` : "—"}
          s={
            snapshot.round_trip_ms
              ? "median from release to text · mostly fixed, not per word"
              : "nothing recorded yet"
          }
        />
        <Tile
          k="Words spoken"
          v={snapshot.words_today ?? "—"}
          s={
            snapshot.dictations_today != null
              ? `${snapshot.dictations_today} dictations, last 24 hours`
              : "nothing recorded yet"
          }
        />
        {/* Spend is never populated: nothing reads OpenRouter's credit
            endpoint. The old subtitle said "needs an OpenRouter key", which
            blamed a missing key for a feature that was never built — and read
            as an error to anyone who had a key working perfectly well. */}
        <Tile
          k="Spend"
          v={snapshot.spend ?? "—"}
          s={snapshot.spend ? "this month" : "not tracked yet"}
        />
      </div>

      <div className="rows">
        {snapshot.engines.map((e) => (
          <div className="row" key={e.name}>
            <span>
              <span className={`dot ${e.dot}`} />
              {e.name}
            </span>
            <span className="meta">{e.state}</span>
          </div>
        ))}
      </div>

      {snapshot.warnings.map((w) => (
        <p className="hint" key={w}>
          {w}
        </p>
      ))}
    </>
  );
}

function Head({ state }) {
  return (
    <div className="pane-head">
      <h2>Status</h2>
      <span className="pill">{state}</span>
    </div>
  );
}

function Tile({ k, v, s }) {
  return (
    <div className="tile">
      <p className="k">{k}</p>
      <p className="v">{v}</p>
      <p className="s">{s}</p>
    </div>
  );
}
