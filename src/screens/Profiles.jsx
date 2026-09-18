import { useState } from "react";
import { useConfig } from "../lib/useConfig.js";

const invoke = (...args) => window.__TAURI__?.core?.invoke(...args);

const DEFAULT_KEY = "default";

const SAMPLE =
  "um so i think we should uh ship the the thing on friday and then like " +
  "circle back on the metrics after";

export default function Profiles() {
  const { config, note, setNote, edit, save } = useConfig();
  const [selected, setSelected] = useState(null);
  const [sample, setSample] = useState(SAMPLE);
  const [preview, setPreview] = useState(null);
  const [previewing, setPreviewing] = useState(false);
  const [newPattern, setNewPattern] = useState("");

  if (!config) {
    return (
      <>
        <Head />
        <div className="empty">{note ?? "Reading the configuration."}</div>
      </>
    );
  }

  const profiles = config.profiles ?? {};
  // Everything except `default`, which is the fallback rather than a pattern
  // and sits apart at the bottom.
  const patterns = Object.keys(profiles).filter((k) => k !== DEFAULT_KEY);
  const current = selected && profiles[selected] ? selected : DEFAULT_KEY;
  const profile = profiles[current];

  const runPreview = async () => {
    setPreviewing(true);
    setPreview(null);
    try {
      setPreview(
        await invoke("preview_cleanup", { transcript: sample, profile }),
      );
    } catch (e) {
      setNote(String(e));
    } finally {
      setPreviewing(false);
    }
  };

  // An inline field rather than window.prompt(): a WKWebView only shows a
  // prompt dialog if the host implements the text-input panel, and Tauri does
  // not, so prompt() would return null and the button would do nothing.
  const addProfile = () => {
    const pattern = newPattern.trim();
    if (!pattern || profiles[pattern]) return;
    edit(["profiles"], {
      ...profiles,
      [pattern]: {
        prompt: "",
        strip_fillers: true,
        auto_capitalize: false,
        model: null,
      },
    });
    setSelected(pattern);
    setNewPattern("");
  };

  const removeProfile = () => {
    const { [current]: _gone, ...rest } = profiles;
    edit(["profiles"], rest);
    setSelected(null);
  };

  return (
    <>
      <Head />

      <div style={{ display: "grid", gridTemplateColumns: "190px 1fr", gap: 16 }}>
        <div>
          <div className="rows">
            {patterns.length === 0 ? (
              <div className="row">
                <span className="meta">No per-app profiles yet.</span>
              </div>
            ) : null}
            {patterns.map((key) => (
              <div
                className="row"
                key={key}
                onClick={() => setSelected(key)}
                style={{ cursor: "pointer", display: "block" }}
              >
                <div>{key.split("|")[0]}</div>
                <div className="meta mono" style={{ fontSize: 11 }}>
                  {key}
                </div>
              </div>
            ))}
          </div>

          <div className="rows" style={{ marginTop: 10 }}>
            <div
              className="row"
              onClick={() => setSelected(DEFAULT_KEY)}
              style={{ cursor: "pointer" }}
            >
              <span>Default</span>
              <span className="meta">fallback</span>
            </div>
          </div>

          <p style={{ marginTop: 10 }}>
            <input
              className="mono"
              placeholder="Code|Code.exe|cursor"
              value={newPattern}
              onChange={(e) => setNewPattern(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && addProfile()}
              style={{ width: "100%", marginBottom: 6 }}
            />
            <button onClick={addProfile} disabled={!newPattern.trim()}>
              Add profile
            </button>
          </p>
          <p className="hint" style={{ fontSize: 11 }}>
            Pipe-separated app identifiers, matched against the focused app.
          </p>
        </div>

        <div>
          <p className="mono meta" style={{ marginTop: 0 }}>
            {current}
          </p>

          <textarea
            rows={4}
            value={profile.prompt}
            onChange={(e) => edit(["profiles", current, "prompt"], e.target.value)}
            style={{ width: "100%", font: "inherit" }}
          />

          <div className="rows" style={{ marginTop: 10 }}>
            <div className="row">
              <span>Model</span>
              <input
                className="mono"
                placeholder="inherit"
                value={profile.model ?? ""}
                onChange={(e) =>
                  edit(
                    ["profiles", current, "model"],
                    e.target.value.trim() === "" ? null : e.target.value,
                  )
                }
                style={{ width: 220 }}
              />
            </div>
            <div className="row">
              <span>Strip fillers</span>
              <input
                type="checkbox"
                checked={!!profile.strip_fillers}
                onChange={(e) =>
                  edit(["profiles", current, "strip_fillers"], e.target.checked)
                }
              />
            </div>
            <div className="row">
              <span>Auto-capitalize</span>
              <input
                type="checkbox"
                checked={!!profile.auto_capitalize}
                onChange={(e) =>
                  edit(["profiles", current, "auto_capitalize"], e.target.checked)
                }
              />
            </div>
          </div>

          <h3 style={{ margin: "18px 0 8px", fontSize: 13 }}>Preview</h3>
          <textarea
            rows={3}
            value={sample}
            onChange={(e) => setSample(e.target.value)}
            className="mono"
            style={{ width: "100%", font: "inherit", fontFamily: "var(--mono)" }}
          />
          <p style={{ margin: "8px 0" }}>
            <button onClick={runPreview} disabled={previewing}>
              {previewing ? "Cleaning…" : "Clean this"}
            </button>
          </p>
          {preview !== null ? (
            <div className="rows">
              <div className="row mono" style={{ display: "block" }}>
                {preview}
              </div>
            </div>
          ) : null}
          <p className="hint">
            The preview calls the real cleanup model with the profile as edited
            here, including changes you have not saved. It costs a request.
          </p>

          {current !== DEFAULT_KEY ? (
            <p style={{ marginTop: 14 }}>
              <button onClick={removeProfile}>Remove this profile</button>
            </p>
          ) : null}
        </div>
      </div>

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
      <h2>Prompt profiles</h2>
    </div>
  );
}
