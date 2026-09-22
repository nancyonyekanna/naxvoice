import { useConfig } from "../lib/useConfig.js";

// Built from what the code accepts, not from design/wireframes.html — that
// frame still shows Cmd+Shift+Space and a separate "dictate raw" key, which is
// the design replaced by one key you hold or double-tap to latch.
// Every key the watcher supports. Short on purpose: each keycode is confirmed
// against Apple's Events.h and tao's table before being offered, because a
// wrong one produces a key that silently never fires.
const KEYS = [
  { id: "RightCommand", label: "Right Command" },
  { id: "LeftCommand", label: "Left Command" },
  { id: "RightOption", label: "Right Option" },
];

export default function Hotkeys() {
  const { config, note, edit, save } = useConfig();

  if (!config) {
    return (
      <>
        <Head />
        <div className="empty">{note ?? "Reading the configuration."}</div>
      </>
    );
  }

  return (
    <>
      <Head />

      <div className="rows">
        <div className="row">
          <span>
            Dictate
            <span className="meta"> · hold and speak, or double-tap to latch</span>
          </span>
          <select
            value={config.hotkeys.dictate_key}
            onChange={(e) => edit(["hotkeys", "dictate_key"], e.target.value)}
          >
            {KEYS.map((k) => (
              <option key={k.id} value={k.id}>
                {k.label}
              </option>
            ))}
          </select>
        </div>

        <div className="row">
          <span>
            Latch window
            <span className="meta"> · how long a second tap still counts</span>
          </span>
          <span>
            <input
              type="range"
              min={150}
              max={800}
              step={25}
              value={config.hotkeys.latch_ms}
              onChange={(e) => edit(["hotkeys", "latch_ms"], Number(e.target.value))}
            />{" "}
            <span className="mono">{config.hotkeys.latch_ms}ms</span>
          </span>
        </div>

        <div className="row">
          <span>
            Read selection aloud
            <span className="meta"> · tap to start, tap again to stop</span>
          </span>
          <select
            value={config.hotkeys.read_aloud_key}
            onChange={(e) => edit(["hotkeys", "read_aloud_key"], e.target.value)}
          >
            {KEYS.map((k) => (
              <option key={k.id} value={k.id}>
                {k.label}
              </option>
            ))}
          </select>
        </div>
        {config.hotkeys.read_aloud_key === config.hotkeys.dictate_key ? (
          <div className="row">
            <span className="meta">
              This is the same key as dictation. One key cannot do both, so pick a
              different one.
            </span>
          </div>
        ) : null}

        <div className="row">
          <span>
            Stop
            <span className="meta"> · not registered</span>
          </span>
          <span className="mono meta">press the read-aloud key again</span>
        </div>
      </div>

      {/* Derived from KEYS rather than written out, because the last version
          said "only the two Command keys" and stayed that way after Right
          Option was added. A count beside a list it does not read is a
          sentence waiting to go stale. */}
      <p className="hint">
        {KEYS.length} keys are offered: {KEYS.map((k) => k.label).join(", ")}.
        Each keycode is checked against Apple's Events.h and tao's own table
        before it appears here, because a wrong one produces a key that
        silently never fires.
      </p>
      <p className="hint">
        Escape is left unregistered on purpose: binding it globally would
        swallow it from every other application.
      </p>
      <p className="hint">
        macOS needs two separate permissions. Input Monitoring lets the dictate
        key fire at all; Accessibility lets the paste land. Missing either shows
        on the Status screen.
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
      <h2>Hotkeys</h2>
    </div>
  );
}
