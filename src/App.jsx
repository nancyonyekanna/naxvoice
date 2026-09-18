import { useState } from "react";
import Status from "./screens/Status.jsx";
import Models from "./screens/Models.jsx";
import Dictionary from "./screens/Dictionary.jsx";
import Voice from "./screens/Voice.jsx";
import Hotkeys from "./screens/Hotkeys.jsx";
import Profiles from "./screens/Profiles.jsx";
import History from "./screens/History.jsx";

const BUILT = {
  status: Status,
  models: Models,
  profiles: Profiles,
  dictionary: Dictionary,
  voice: Voice,
  history: History,
  hotkeys: Hotkeys,
};

// DESIGN.md's sidebar, in its order. Five of these are specified in DESIGN.md,
// Hotkeys has a wireframe only, and History has neither — it appears in the
// sidebar list and nowhere else. Unbuilt screens say so rather than rendering
// an empty shell that looks broken.
const SCREENS = [
  { id: "status", label: "Status", spec: true },
  { id: "models", label: "Models", spec: true },
  { id: "profiles", label: "Profiles", spec: true },
  { id: "dictionary", label: "Dictionary", spec: true },
  { id: "voice", label: "Voice", spec: true },
  { id: "history", label: "History", spec: true },
  { id: "hotkeys", label: "Hotkeys", spec: true },
];

export default function App() {
  const [screen, setScreen] = useState("status");

  return (
    <div className="app">
      <nav className="side">
        <p className="brand">naxvoice</p>
        {SCREENS.map((s) => (
          <button
            key={s.id}
            className={s.id === screen ? "on" : ""}
            onClick={() => setScreen(s.id)}
          >
            {s.label}
          </button>
        ))}
      </nav>

      <div className="pane">
        {(() => {
          const Screen = BUILT[screen];
          return Screen ? (
            <Screen />
          ) : (
            <NotBuilt screen={SCREENS.find((s) => s.id === screen)} />
          );
        })()}
      </div>
    </div>
  );
}

function NotBuilt({ screen }) {
  return (
    <>
      <div className="pane-head">
        <h2>{screen.label}</h2>
      </div>
      <div className="empty">
        Not built yet.{" "}
        {screen.spec
          ? "The design for this screen is in DESIGN.md."
          : "This screen has no design: it is named in the sidebar but has no section in DESIGN.md and no wireframe frame."}
      </div>
    </>
  );
}
