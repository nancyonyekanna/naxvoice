import { useEffect, useState } from "react";
import { useConfig } from "../lib/useConfig.js";

const invoke = (...args) => window.__TAURI__?.core?.invoke(...args);

// DESIGN.md 02. Options are labelled with their trade-off rather than just
// their name, because "deepgram/nova-3" tells you nothing about why you would
// pick it.
const TRANSCRIPTION = [
  { id: "deepgram/nova-3", label: "deepgram/nova-3 · fastest" },
  { id: "openai/whisper-large-v3", label: "openai/whisper-large-v3 · most accurate" },
];

const CLEANUP = [
  { id: "anthropic/claude-haiku-4.5", label: "anthropic/claude-haiku-4.5 · fastest" },
  { id: "anthropic/claude-sonnet-4.5", label: "anthropic/claude-sonnet-4.5 · best rewrites" },
];

const OFFLINE = [
  { id: "paste_raw", label: "Paste the raw transcript" },
  { id: "queue", label: "Queue until reconnected" },
];

export default function Models() {
  const { config, note, setNote, edit, save } = useConfig();
  const [key, setKey] = useState(null);
  const [typed, setTyped] = useState("");

  // The key is not part of config.yaml and never will be, so it loads
  // separately from the keychain rather than through useConfig.
  useEffect(() => {
    invoke("api_key_status")?.then(setKey).catch((e) => setNote(String(e)));
  }, [setNote]);

  if (!config) {
    return (
      <>
        <Head />
        <div className="empty">{note ?? "Reading the configuration."}</div>
      </>
    );
  }

  const saveKey = async () => {
    try {
      setKey(await invoke("set_api_key", { key: typed }));
      setTyped("");
      setNote("Key saved to the keychain.");
    } catch (e) {
      setNote(String(e));
    }
  };

  const clearKey = async () => {
    try {
      setKey(await invoke("clear_api_key"));
      setNote("Key removed from the keychain.");
    } catch (e) {
      setNote(String(e));
    }
  };

  return (
    <>
      <Head />

      <div className="rows">
        <div className="row">
          <span>
            OpenRouter key
            {key?.present ? (
              <span className="meta">
                {" "}
                {/* "stored in the environment" read like a filing error. Both
                    sources are supported, so the label states which one is in
                    use rather than implying one of them is a mistake. */}
                · {key.source === "environment" ? "read from your environment" : "saved in your keychain"}{" "}
                <span className="mono">{key.hint}</span>
              </span>
            ) : (
              <span className="meta"> · not set</span>
            )}
          </span>
          <span>
            <input
              type="password"
              value={typed}
              placeholder={key?.present ? "Replace the key" : "sk-or-v1-…"}
              onChange={(e) => setTyped(e.target.value)}
              style={{ width: 200 }}
            />{" "}
            <button onClick={saveKey} disabled={!typed.trim()}>
              Save
            </button>{" "}
            {key?.present && key.source === "keychain" ? (
              <button onClick={clearKey}>Remove</button>
            ) : null}
          </span>
        </div>
      </div>

      {/* This used to open "The key is stored in the OS keychain", which
          contradicted the label above it whenever the key came from the
          environment. The label was not the problem; a screen that asserts one
          thing and displays another is what reads as a warning. */}
      <p className="hint">
        Either source works. Saving one here puts it in the OS keychain, where
        it survives a reboot and takes precedence. An environment variable, or
        a <span className="mono">.env</span> beside{" "}
        <span className="mono">config.yaml</span>, is read when no key is saved,
        which is the quickest way to get going. Neither ever goes into{" "}
        <span className="mono">config.yaml</span>.
      </p>
      {key?.present && key.source === "environment" ? (
        <p className="hint">
          Yours is coming from the environment right now. Paste it above and
          save to move it into the keychain; the environment copy then takes
          second place, and removing it is something only you can do, since the
          app cannot edit your <span className="mono">.env</span>.
        </p>
      ) : null}

      <div className="rows" style={{ marginTop: 16 }}>
        <Select
          label="Transcription model"
          options={TRANSCRIPTION}
          value={config.transcription.model}
          onChange={(v) => edit(["transcription", "model"], v)}
        />
        <Select
          label="Cleanup model"
          options={CLEANUP}
          value={config.cleanup.model}
          onChange={(v) => edit(["cleanup", "model"], v)}
        />
        <Select
          label="If offline"
          options={OFFLINE}
          value={config.cleanup.offline_behavior}
          onChange={(v) => edit(["cleanup", "offline_behavior"], v)}
        />
      </div>

      <h3 style={{ margin: "20px 0 8px", fontSize: 13 }}>Chunking</h3>
      <div className="rows">
        <Slider
          label="Pause threshold"
          min={200}
          max={1200}
          step={50}
          value={config.chunking.pause_threshold_ms}
          onChange={(v) => edit(["chunking", "pause_threshold_ms"], v)}
        />
        <Slider
          label="Chunk overlap"
          min={0}
          max={500}
          step={25}
          value={config.chunking.overlap_ms}
          onChange={(v) => edit(["chunking", "overlap_ms"], v)}
        />
      </div>
      <p className="hint">
        A lower threshold sends sooner but risks cutting mid-thought.
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
      <h2>Models and routing</h2>
    </div>
  );
}

function Select({ label, options, value, onChange }) {
  // An unrecognised value is offered back rather than silently replaced: the
  // config may name a model this build does not list, and quietly switching it
  // on the user's behalf would be a change they never asked for.
  const known = options.some((o) => o.id === value);
  return (
    <div className="row">
      <span>{label}</span>
      <select value={value} onChange={(e) => onChange(e.target.value)}>
        {!known ? <option value={value}>{value} · from config</option> : null}
        {options.map((o) => (
          <option key={o.id} value={o.id}>
            {o.label}
          </option>
        ))}
      </select>
    </div>
  );
}

function Slider({ label, min, max, step, value, onChange }) {
  return (
    <div className="row">
      <span>{label}</span>
      <span>
        <input
          type="range"
          min={min}
          max={max}
          step={step}
          value={value}
          onChange={(e) => onChange(Number(e.target.value))}
        />{" "}
        <span className="mono">{value}ms</span>
      </span>
    </div>
  );
}
