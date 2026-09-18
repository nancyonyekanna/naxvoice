import { useConfig } from "../lib/useConfig.js";

export default function Voice() {
  const { config, note, edit, save } = useConfig();

  if (!config) {
    return (
      <>
        <Head />
        <div className="empty">{note ?? "Reading the configuration."}</div>
      </>
    );
  }

  const rules = config.pronunciation ?? [];

  const setRule = (index, field, value) => {
    const next = rules.map((r, i) => (i === index ? { ...r, [field]: value } : r));
    edit(["pronunciation"], next);
  };

  return (
    <>
      <Head />

      <div className="rows">
        <div className="row">
          <span>Engine</span>
          <span className="meta">
            Kokoro — the only one implemented
          </span>
        </div>
        <div className="row">
          <span>
            Voice
            <span className="meta"> · a voice pack in src-tauri/assets</span>
          </span>
          <input
            className="mono"
            value={config.tts.main.voice}
            onChange={(e) => {
              edit(["tts", "main", "voice"], e.target.value);
              edit(["tts", "first_sentence", "voice"], e.target.value);
            }}
            style={{ width: 180 }}
          />
        </div>
        <div className="row">
          <span>Speed</span>
          <span>
            <input
              type="range"
              min={0.5}
              max={2}
              step={0.05}
              value={config.tts.speed}
              onChange={(e) => edit(["tts", "speed"], Number(e.target.value))}
            />{" "}
            <span className="mono">{Number(config.tts.speed).toFixed(2)}×</span>
          </span>
        </div>
      </div>

      <p className="hint">
        The voice name is the file name of a pack in{" "}
        <span className="mono">src-tauri/assets</span>, without the{" "}
        <span className="mono">.bin</span>. Naming one that is not there fails
        when you press the key, and says which path was missing.
      </p>
      <p className="hint">
        Expression is not shown. It was a Chatterbox-only control, and Kokoro
        ignores it.
      </p>

      <h3 style={{ margin: "20px 0 8px", fontSize: 13 }}>Before speaking</h3>
      <div className="rows">
        <Check
          label="Skip code blocks"
          checked={config.tts.skip_code_blocks}
          onChange={(v) => edit(["tts", "skip_code_blocks"], v)}
        />
        <Check
          label="Skip URLs"
          checked={config.tts.skip_urls}
          onChange={(v) => edit(["tts", "skip_urls"], v)}
        />
        <Check
          label="Expand numbers"
          checked={config.tts.expand_numbers}
          onChange={(v) => edit(["tts", "expand_numbers"], v)}
        />
      </div>

      <h3 style={{ margin: "20px 0 8px", fontSize: 13 }}>Pronunciation</h3>
      <div className="rows">
        {rules.length === 0 ? (
          <div className="row">
            <span className="meta">
              Add a word the voice keeps getting wrong.
            </span>
          </div>
        ) : null}
        {rules.map((rule, i) => (
          <div className="row" key={i}>
            <span>
              <input
                className="mono"
                value={rule.match}
                onChange={(e) => setRule(i, "match", e.target.value)}
                style={{ width: 130 }}
              />{" "}
              <span className="meta">→</span>{" "}
              <input
                className="mono"
                value={rule.say}
                onChange={(e) => setRule(i, "say", e.target.value)}
                style={{ width: 150 }}
              />
            </span>
            <button
              onClick={() => edit(["pronunciation"], rules.filter((_, j) => j !== i))}
            >
              Remove
            </button>
          </div>
        ))}
      </div>
      <p style={{ marginTop: 10 }}>
        <button onClick={() => edit(["pronunciation"], [...rules, { match: "", say: "" }])}>
          Add rule
        </button>
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
      <h2>Voice and read-aloud</h2>
    </div>
  );
}

function Check({ label, checked, onChange }) {
  return (
    <div className="row">
      <span>{label}</span>
      <input
        type="checkbox"
        checked={!!checked}
        onChange={(e) => onChange(e.target.checked)}
      />
    </div>
  );
}
