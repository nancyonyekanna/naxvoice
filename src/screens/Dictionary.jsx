import { useState } from "react";
import { useConfig } from "../lib/useConfig.js";

export default function Dictionary() {
  const { config, note, edit, save } = useConfig();
  const [term, setTerm] = useState("");
  const [soundsLike, setSoundsLike] = useState("");

  if (!config) {
    return (
      <>
        <Head />
        <div className="empty">{note ?? "Reading the configuration."}</div>
      </>
    );
  }

  const terms = config.dictionary.terms ?? [];

  const add = () => {
    const name = term.trim();
    if (!name) return;
    const heard = soundsLike
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    edit(["dictionary", "terms"], [...terms, { term: name, sounds_like: heard }]);
    setTerm("");
    setSoundsLike("");
  };

  return (
    <>
      <Head />

      <div className="rows">
        <div className="row">
          <span>
            <input
              placeholder="Term"
              value={term}
              onChange={(e) => setTerm(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && add()}
              style={{ width: 150 }}
            />{" "}
            <input
              placeholder="Sounds like (comma separated)"
              value={soundsLike}
              onChange={(e) => setSoundsLike(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && add()}
              style={{ width: 220 }}
            />
          </span>
          <button onClick={add} disabled={!term.trim()}>
            Add
          </button>
        </div>
      </div>

      <div className="rows" style={{ marginTop: 14 }}>
        {terms.length === 0 ? (
          <div className="row">
            <span className="meta">
              Add a term the transcription keeps getting wrong.
            </span>
          </div>
        ) : null}
        {terms.map((t, i) => (
          <div className="row" key={`${t.term}-${i}`}>
            <span className="mono">{t.term}</span>
            <span>
              <span className="meta">
                {t.sounds_like?.length ? t.sounds_like.join(", ") : "no variants"}
              </span>{" "}
              <button
                onClick={() =>
                  edit(["dictionary", "terms"], terms.filter((_, j) => j !== i))
                }
              >
                Remove
              </button>
            </span>
          </div>
        ))}
      </div>

      <div className="rows" style={{ marginTop: 14 }}>
        <div className="row">
          <span>
            Prompt budget
            <span className="meta"> · how many terms bias the transcription</span>
          </span>
          <span>
            <input
              type="number"
              min={0}
              max={200}
              value={config.dictionary.prompt_budget}
              onChange={(e) =>
                edit(["dictionary", "prompt_budget"], Number(e.target.value))
              }
              style={{ width: 70 }}
            />
          </span>
        </div>
      </div>

      <p className="hint">
        The first {config.dictionary.prompt_budget} terms are sent with the audio
        to bias transcription; the rest are left to the cleanup model.
      </p>
      <p className="hint">
        DESIGN.md sorts this table by how often each term has been corrected, and
        uses that count to decide which terms make the budget. Nothing records
        corrections yet, so the order here is the order in the file.
      </p>

      <p style={{ marginTop: 18 }}>
        <button onClick={save}>Save settings</button>
      </p>
      {note ? <p className="hint">{note}</p> : null}
    </>
  );
}

function Head() {
  return (
    <div className="pane-head">
      <h2>Dictionary</h2>
    </div>
  );
}
