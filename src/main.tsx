import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles.css";

// Note: StrictMode is intentionally omitted — its dev double-mount would spawn
// (and then kill) a second PTY per tab. Terminals must own a stable side effect.
ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(<App />);
