import { useCallback, useEffect, useState } from "react";

const invoke = (...args) => window.__TAURI__?.core?.invoke(...args);

/// Load, edit and save config.yaml.
///
/// Every editable screen needs the same three things, and the editing is done
/// by path so a screen only names the key it changes — the rest of the
/// structure round-trips back to Rust exactly as it arrived, which is what
/// keeps a screen from dropping fields it does not know about.
export function useConfig() {
  const [config, setConfig] = useState(null);
  const [note, setNote] = useState(null);

  useEffect(() => {
    const call = invoke("get_config");
    if (!call) {
      setNote("Not running inside the app, so there is no configuration to read.");
      return;
    }
    call.then(setConfig).catch((e) => setNote(String(e)));
  }, []);

  const edit = useCallback((path, value) => {
    setConfig((prev) => {
      const next = structuredClone(prev);
      let node = next;
      for (const part of path.slice(0, -1)) node = node[part];
      node[path[path.length - 1]] = value;
      return next;
    });
  }, []);

  const save = useCallback(async () => {
    try {
      await invoke("save_config", { config });
      // Deliberately explicit: Config is built once at startup, so the file
      // changes but the running app does not. Implying otherwise would have
      // people wondering why a slider did nothing.
      setNote("Saved. Changes take effect when naxvoice restarts.");
    } catch (e) {
      setNote(String(e));
    }
  }, [config]);

  return { config, note, setNote, edit, save };
}
