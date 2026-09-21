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
        {/* naxvoice's own spending, from the local ledger, never the
            OpenRouter account total. One key is usually shared with other
            tools, so the account figure would report spending this app never
            did. Read-aloud is absent from it because Kokoro runs on this
            machine and costs nothing. */}
        <Tile
          k="Spend"
          v={money(snapshot.spend?.all)}
          s={spendNote(snapshot.spend)}
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

// One dictation costs a fraction of a cent, so a flat two decimal places would
// print almost every real figure as "$0.00". Precision grows as the amount
// shrinks, which keeps small totals legible without padding large ones.
function money(amount) {
  const n = Number(amount) || 0;
  if (n >= 1) return `$${n.toFixed(2)}`;
  if (n >= 0.01) return `$${n.toFixed(3)}`;
  return `$${n.toFixed(4)}`;
}

function spendNote(spend) {
  if (!spend || !spend.all) return "nothing spent yet";
  // "at least" rather than a figure that reads as exact: a provider that
  // returns no price leaves the ledger unable to account for that call, and
  // counting it as zero would understate the total silently.
  const floor = spend.partial ? "at least " : "";
  return `${floor}${money(spend.day)} last 24 hours, ${money(spend.month)} last 30 days`;
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
