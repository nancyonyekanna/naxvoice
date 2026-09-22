import { useCallback, useEffect, useState } from "react";
import { useConfig } from "../lib/useConfig.js";

const invoke = (...args) => window.__TAURI__?.core?.invoke(...args);

// First run, for someone who installed the app and never read INSTALL.md.
//
// Without this they press the key, nothing happens, and there is nothing on
// screen to explain why: the two macOS permissions are separate grants, neither
// is prompted for, and a missing one looks exactly like a broken app.
//
// It takes the whole window rather than sitting in the sidebar. Someone whose
// dictation key does not fire has no use for the Dictionary screen yet.
export default function Setup({ onClose }) {
  const { config } = useConfig();
  const [setup, setSetup] = useState(null);
  const [key, setKey] = useState("");
  const [note, setNote] = useState(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    invoke("setup_status")
      ?.then(setSetup)
      .catch((e) => setNote(String(e)));
  }, []);

  useEffect(() => {
    refresh();
    // On focus rather than on a timer. Both permissions are read once, when
    // naxvoice starts, so nothing can change while this window sits in front.
    // The one moment the answer can differ is the trip to System Settings and
    // back, and that is exactly when focus returns.
    addEventListener("focus", refresh);
    return () => removeEventListener("focus", refresh);
  }, [refresh]);

  const openPane = async (which) => {
    try {
      await invoke("open_permission_settings", { which });
    } catch (e) {
      setNote(String(e));
    }
  };

  const saveKey = async () => {
    setBusy(true);
    setNote("Saving, then checking it works.");
    try {
      await invoke("set_api_key", { key });
      setKey("");
      // Saved is not the same as working. A typo in a key is stored perfectly
      // happily and then fails on the first dictation, which is the worst place
      // to find out.
      setNote(await invoke("verify_api_key"));
      refresh();
    } catch (e) {
      setNote(String(e));
    } finally {
      setBusy(false);
    }
  };

  const check = async () => {
    setBusy(true);
    setNote("Checking.");
    try {
      setNote(await invoke("verify_api_key"));
    } catch (e) {
      setNote(String(e));
    } finally {
      setBusy(false);
    }
  };

  if (!setup) {
    return (
      <div className="setup">
        <div className="pane-head">
          <h2>Setting up naxvoice</h2>
        </div>
        <div className="empty">{note ?? "Checking what is already done."}</div>
      </div>
    );
  }

  const dictate = config?.hotkeys?.dictate_key ?? "Right Command";
  const read = config?.hotkeys?.read_aloud_key ?? "Right Option";

  return (
    <div className="setup">
      <div className="pane-head">
        <h2>Setting up naxvoice</h2>
        <span className="pill">{setup.complete ? "ready" : "3 steps"}</span>
      </div>

      <div className="rows">
        <Step
          done={setup.input_monitoring}
          title="Input Monitoring"
          line="Lets the dictation key fire. Without it nothing is recorded and the app looks dead."
        >
          <button onClick={() => openPane("input_monitoring")}>
            Open Input Monitoring
          </button>
        </Step>

        <Step
          done={setup.accessibility}
          title="Accessibility"
          line="Lets the text be pasted. Without it dictation records and transcribes, then nothing appears."
        >
          <button onClick={() => openPane("accessibility")}>
            Open Accessibility
          </button>
        </Step>

        <Step
          done={setup.key}
          title="OpenRouter key"
          line={
            setup.key
              ? `Found in the ${setup.key_source}. Transcription and cleanup run on it.`
              : "Pays for transcription and cleanup. Read-aloud works without one."
          }
        >
          <input
            className="mono"
            type="password"
            value={key}
            placeholder={setup.key ? "Replace the key" : "sk-or-v1-…"}
            onChange={(e) => setKey(e.target.value)}
            style={{ minWidth: 220 }}
          />{" "}
          <button onClick={saveKey} disabled={!key.trim() || busy}>
            Save
          </button>{" "}
          {setup.key ? (
            <button onClick={check} disabled={busy}>
              Check it works
            </button>
          ) : null}
        </Step>
      </div>

      {setup.complete ? (
        <>
          <p className="hint">
            That is everything. Hold <b>{dictate}</b> and speak, and the text
            appears where your cursor is. Select any text and tap <b>{read}</b>{" "}
            to hear it read back.
          </p>
          <p>
            <button onClick={onClose}>Try it now</button>
          </p>
        </>
      ) : (
        <>
          <p className="hint">
            Both permissions are only read when naxvoice starts, so granting one
            changes nothing until it restarts. Grant what you need, then use the
            button below.
          </p>
          <p>
            <button onClick={() => invoke("restart_app")}>
              Restart naxvoice
            </button>{" "}
            <button onClick={onClose}>Skip for now</button>
          </p>
        </>
      )}

      {note ? <p className="hint">{note}</p> : null}
    </div>
  );
}

function Step({ done, title, line, children }) {
  return (
    <div className="row" style={{ display: "block" }}>
      <div>
        <span className={`dot ${done ? "ok" : "bad"}`} />
        {title}
        <span className="meta"> · {done ? "done" : "not yet"}</span>
      </div>
      <div className="meta" style={{ margin: "4px 0 8px 14px" }}>
        {line}
      </div>
      <div style={{ marginLeft: 14 }}>{children}</div>
    </div>
  );
}
