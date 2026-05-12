import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Screen = "onboarding" | "main" | "history" | "settings";
type RecordingState = "idle" | "recording" | "transcribing";

interface Transcription {
  id: string;
  raw_text: string;
  formatted_text: string | null;
  provider: string;
  duration_ms: number | null;
  context_type: string | null;
  window_title: string | null;
  language: string | null;
  created_at: string;
}

function App() {
  const [screen, setScreen] = useState<Screen>("onboarding");
  const [apiKey, setApiKey] = useState("");
  const [status, setStatus] = useState<RecordingState>("idle");
  const [lastTranscription, setLastTranscription] = useState("");
  const [error, setError] = useState("");
  const [history, setHistory] = useState<Transcription[]>([]);
  const [searchQuery, setSearchQuery] = useState("");
  const [language, setLanguage] = useState("auto");
  const [theme, setTheme] = useState<"dark" | "light">("dark");
  const [formatEnabled, setFormatEnabled] = useState(true);
  const [notification, setNotification] = useState("");

  useEffect(() => {
    invoke<string | null>("get_api_key").then((key) => {
      if (key) {
        setApiKey(key);
        setScreen("main");
      }
    });
    invoke<string | null>("get_setting", { key: "language" }).then((val) => {
      if (val) setLanguage(val);
    });
    invoke<string | null>("get_setting", { key: "theme" }).then((val) => {
      if (val) setTheme(val as "dark" | "light");
    });
    invoke<string | null>("get_setting", { key: "format_enabled" }).then((val) => {
      if (val === "false") setFormatEnabled(false);
    });
    loadHistory();
  }, []);

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
  }, [theme]);

  useEffect(() => {
    const unlisten1 = listen<string>("recording-state", (event) => {
      setStatus(event.payload as RecordingState);
    });
    const unlisten2 = listen<Transcription>("transcription-result", (event) => {
      const t = event.payload;
      setLastTranscription(t.formatted_text || t.raw_text);
      setHistory((prev) => [t, ...prev].slice(0, 50));
      setError("");
    });
    const unlisten3 = listen<string>("transcription-error", (event) => {
      setError(event.payload);
    });
    const unlisten4 = listen<string>("recopy-success", (event) => {
      showNotification(event.payload);
    });

    return () => {
      unlisten1.then((f) => f());
      unlisten2.then((f) => f());
      unlisten3.then((f) => f());
      unlisten4.then((f) => f());
    };
  }, []);

  const showNotification = (msg: string) => {
    setNotification(msg);
    setTimeout(() => setNotification(""), 2000);
  };

  const loadHistory = async () => {
    try {
      const h = await invoke<Transcription[]>("get_history", { limit: 50 });
      setHistory(h);
    } catch {}
  };

  const handleSearch = async () => {
    if (!searchQuery.trim()) {
      loadHistory();
      return;
    }
    try {
      const h = await invoke<Transcription[]>("search_history", { query: searchQuery });
      setHistory(h);
    } catch {}
  };

  const handleSaveKey = async () => {
    if (!apiKey.trim()) return;
    try {
      await invoke("set_api_key", { key: apiKey.trim() });
      setScreen("main");
      setError("");
    } catch (e) {
      setError(String(e));
    }
  };

  const handleStartRecording = async () => {
    setError("");
    setStatus("recording");
    try {
      await invoke("start_recording");
    } catch (e) {
      setError(String(e));
      setStatus("idle");
    }
  };

  const handleStopRecording = async () => {
    setStatus("transcribing");
    try {
      const result = await invoke<Transcription>("stop_recording_and_transcribe");
      setLastTranscription(result.formatted_text || result.raw_text);
      setHistory((prev) => [result, ...prev].slice(0, 50));
      setStatus("idle");
      setError("");
    } catch (e) {
      setError(String(e));
      setStatus("idle");
    }
  };

  const handleLanguageChange = async (lang: string) => {
    setLanguage(lang);
    const value = lang === "auto" ? "" : lang;
    await invoke("set_setting", { key: "language", value });
  };

  const handleThemeToggle = async () => {
    const newTheme = theme === "dark" ? "light" : "dark";
    setTheme(newTheme);
    await invoke("set_setting", { key: "theme", value: newTheme });
  };

  const handleFormatToggle = async () => {
    const newVal = !formatEnabled;
    setFormatEnabled(newVal);
    await invoke("set_setting", { key: "format_enabled", value: String(newVal) });
  };

  // Onboarding
  if (screen === "onboarding") {
    return (
      <main className="container">
        <h1>OpenFlow</h1>
        <p className="subtitle">Open-source voice transcription</p>
        <div className="onboarding">
          <p>Enter your Groq API key to get started.</p>
          <p className="hint">
            Get one free at{" "}
            <a href="https://console.groq.com/keys" target="_blank">console.groq.com/keys</a>
          </p>
          <input
            type="password"
            value={apiKey}
            onChange={(e) => setApiKey(e.target.value)}
            placeholder="gsk_..."
            onKeyDown={(e) => e.key === "Enter" && handleSaveKey()}
          />
          <button onClick={handleSaveKey} disabled={!apiKey.trim()}>Save & Continue</button>
          {error && <p className="error">{error}</p>}
        </div>
      </main>
    );
  }

  // History
  if (screen === "history") {
    return (
      <main className="container history-screen">
        <div className="nav-row">
          <button className="nav-btn" onClick={() => setScreen("main")}>Back</button>
          <h2>History</h2>
          <div />
        </div>
        <div className="search-row">
          <input
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            placeholder="Search transcriptions..."
            onKeyDown={(e) => e.key === "Enter" && handleSearch()}
          />
        </div>
        <div className="history-list">
          {history.length === 0 && <p className="empty">No transcriptions yet</p>}
          {history.map((item) => (
            <div
              key={item.id}
              className="history-card"
              onClick={() => {
                const text = item.formatted_text || item.raw_text;
                navigator.clipboard.writeText(text);
                showNotification("Copied!");
              }}
            >
              <p className="history-card-text">{item.formatted_text || item.raw_text}</p>
              <div className="history-card-meta">
                <span>{new Date(item.created_at).toLocaleString()}</span>
                {item.duration_ms && <span>{(item.duration_ms / 1000).toFixed(1)}s</span>}
                {item.language && <span>{item.language}</span>}
              </div>
            </div>
          ))}
        </div>
      </main>
    );
  }

  // Settings
  if (screen === "settings") {
    return (
      <main className="container settings-screen">
        <div className="nav-row">
          <button className="nav-btn" onClick={() => setScreen("main")}>Back</button>
          <h2>Settings</h2>
          <div />
        </div>
        <div className="settings-list">
          <div className="setting-item">
            <label>API Key</label>
            <input
              type="password"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              onBlur={() => apiKey && invoke("set_api_key", { key: apiKey })}
            />
          </div>
          <div className="setting-item">
            <label>Language</label>
            <select value={language} onChange={(e) => handleLanguageChange(e.target.value)}>
              <option value="auto">Auto-detect</option>
              <option value="en">English</option>
              <option value="es">Spanish</option>
              <option value="fr">French</option>
              <option value="de">German</option>
              <option value="it">Italian</option>
              <option value="pt">Portuguese</option>
              <option value="nl">Dutch</option>
              <option value="ja">Japanese</option>
              <option value="ko">Korean</option>
              <option value="zh">Chinese</option>
              <option value="ar">Arabic</option>
              <option value="hi">Hindi</option>
              <option value="ru">Russian</option>
            </select>
          </div>
          <div className="setting-item">
            <label>LLM Formatting</label>
            <button className="toggle-btn" onClick={handleFormatToggle}>
              {formatEnabled ? "On" : "Off"}
            </button>
          </div>
          <div className="setting-item">
            <label>Theme</label>
            <button className="toggle-btn" onClick={handleThemeToggle}>
              {theme === "dark" ? "Dark" : "Light"}
            </button>
          </div>
          <div className="setting-item">
            <label>Record Hotkey</label>
            <span className="setting-value">Ctrl+Shift+Space</span>
          </div>
          <div className="setting-item">
            <label>Re-copy Hotkey</label>
            <span className="setting-value">Ctrl+Shift+V</span>
          </div>
        </div>
      </main>
    );
  }

  // Main
  return (
    <main className="container">
      {notification && <div className="toast">{notification}</div>}

      <div className="top-row">
        <button className="icon-btn" onClick={() => setScreen("history")}>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/>
          </svg>
        </button>
        <h1>OpenFlow</h1>
        <button className="icon-btn" onClick={() => setScreen("settings")}>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <circle cx="12" cy="12" r="3"/><path d="M12 1v2m0 18v2m-9-11h2m18 0h2m-4.2-5.8l-1.4 1.4M5.6 18.4l-1.4 1.4m0-13.8l1.4 1.4m12.8 12.8l1.4 1.4"/>
          </svg>
        </button>
      </div>

      <div className="status-ring" data-status={status}>
        <div className="status-inner">
          {status === "idle" && "Ready"}
          {status === "recording" && "Recording..."}
          {status === "transcribing" && "Transcribing..."}
        </div>
      </div>

      <button
        className="record-btn"
        onMouseDown={handleStartRecording}
        onMouseUp={handleStopRecording}
        disabled={status === "transcribing"}
      >
        {status === "idle" && "Hold to Record"}
        {status === "recording" && "Release to Transcribe"}
        {status === "transcribing" && "Processing..."}
      </button>

      <p className="hint">
        <strong>Ctrl+Shift+Space</strong> anywhere &middot; <strong>Ctrl+Shift+V</strong> re-copy
      </p>

      {error && <p className="error">{error}</p>}

      {lastTranscription && (
        <div className="result">
          <p className="result-label">Copied to clipboard</p>
          <p className="result-text">{lastTranscription}</p>
        </div>
      )}
    </main>
  );
}

export default App;
