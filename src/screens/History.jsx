import { useEffect, useState } from "react";
import { useConfig } from "../lib/useConfig.js";

const invoke = (...args) => window.__TAURI__?.core?.invoke(...args);

export default function History() {
  const { config, note, setNote, edit, save } = useConfig();
  const [records, setRecords] = useState(null);
  // Which destructive action is armed, if any. A two-step button rather than a
  // dialog: both of these delete something permanently, and the first click
  // should not be the last word.
  const [arming, setArming] = useState(null);

  const load = () => {
    invoke("history_recent", { limit: 100 })
      ?.then(setRecords)
      .catch((e) => setNote(String(e)));
  };

  useEffect(load, []);

  const clear = async () => {
    setArming(null);
    try {
      await invoke("clear_history");
      setRecords([]);
      setNote("History cleared. Your spending record is untouched.");
    } catch (e) {
      setNote(String(e));
    }
  };

  // Deliberately separate from clearing history. What you said and what you
  // paid are different records in different files, and one button doing both
  // would destroy one the user meant to keep.
  const clearSpend = async () => {
    setArming(null);
    try {
      await invoke("clear_spend");
      setNote("Spending record cleared. Your dictation history is untouched.");
    } catch (e) {
      setNote(String(e));
    }
  };

  const keeping = config?.history?.keep ?? true;

  return (
    <>
      <div className="pane-head">
        <h2>History</h2>
        {records ? <span className="pill">{records.length} kept</span> : null}
      </div>

      <div className="rows">
        <div className="row">
          <span>
            Keep dictations
            <span className="meta"> · writes what you dictate to disk</span>
          </span>
          <input
            type="checkbox"
            checked={keeping}
            disabled={!config}
            onChange={(e) => edit(["history", "keep"], e.target.checked)}
          />
        </div>
      </div>
      <p className="hint">
        Stored in plain text in the app data directory, capped at the most
        recent thousand. Turning this off stops new dictations being recorded;
        it does not remove what is already there.
      </p>
      <p>
        <button onClick={save} disabled={!config}>
          Save settings
        </button>{" "}
        <button
          onClick={() => (arming === "history" ? clear() : setArming("history"))}
          disabled={!records?.length}
        >
          {arming === "history"
            ? "Click again to delete everything"
            : "Delete everything recorded"}
        </button>{" "}
        <button onClick={() => (arming === "spend" ? clearSpend() : setArming("spend"))}>
          {arming === "spend"
            ? "Click again to clear spending"
            : "Clear spending record"}
        </button>
      </p>
      <p className="hint">
        These are two separate records. Deleting your dictations leaves the
        spending total intact, and clearing the spending total leaves your
        dictations intact.
      </p>

      {records === null ? (
        <div className="empty">{note ?? "Reading the history."}</div>
      ) : records.length === 0 ? (
        <div className="empty">
          {keeping
            ? "Nothing recorded yet. Dictate something and it will appear here."
            : "Recording is off, so nothing is being kept."}
        </div>
      ) : (
        <div className="rows" style={{ marginTop: 14 }}>
          {records.map((r, i) => (
            <div className="row" key={`${r.at}-${i}`} style={{ display: "block" }}>
              <div className="meta" style={{ fontSize: 11 }}>
                {new Date(r.at * 1000).toLocaleString()} · {r.app} · {r.ms}ms ·{" "}
                {r.chunks} {r.chunks === 1 ? "chunk" : "chunks"}
              </div>
              <div>{r.text}</div>
              {r.raw && r.raw !== r.text ? (
                <div className="meta mono" style={{ fontSize: 11, marginTop: 4 }}>
                  raw: {r.raw}
                </div>
              ) : null}
            </div>
          ))}
        </div>
      )}

      {note ? <p className="hint">{note}</p> : null}
    </>
  );
}
