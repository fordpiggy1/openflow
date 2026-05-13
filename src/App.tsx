import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Screen = "onboarding" | "main" | "history" | "settings" | "plugins";
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

interface PluginInfo {
  manifest: { id: string; name: string; version: string; description: string; author: string | null; hooks: string[] };
  enabled: boolean;
  path: string;
}

interface ModelInfo {
  id: string;
  name: string;
  model_type: string;
}

const PROVIDERS: Record<string, { label: string; hint: string; placeholder: string }> = {
  groq: { label: "Groq (recommended)", hint: "console.groq.com/keys", placeholder: "gsk_..." },
  openai: { label: "OpenAI", hint: "platform.openai.com/api-keys", placeholder: "sk-..." },
  openrouter: { label: "OpenRouter", hint: "openrouter.ai/keys", placeholder: "sk-or-..." },
  deepgram: { label: "Deepgram", hint: "console.deepgram.com", placeholder: "..." },
};

function App() {
  const [screen, setScreen] = useState<Screen>("onboarding");
  const [apiKey, setApiKey] = useState("");
  const [status, setStatus] = useState<RecordingState>("idle");
  const [lastTranscription, setLastTranscription] = useState("");
  const [error, setError] = useState("");
  const [history, setHistory] = useState<Transcription[]>([]);
  const [searchQuery, setSearchQuery] = useState("");
  const [language, setLanguage] = useState("auto");
  const [provider, setProvider] = useState("groq");
  const [theme, setTheme] = useState<"dark" | "light">("light");
  const [formatEnabled, setFormatEnabled] = useState(true);
  const [notification, setNotification] = useState("");
  const [plugins, setPlugins] = useState<PluginInfo[]>([]);
  const [sttModel, setSttModel] = useState("");
  const [chatModel, setChatModel] = useState("");
  const [availableModels, setAvailableModels] = useState<ModelInfo[]>([]);
  const [modelsLoading, setModelsLoading] = useState(false);

  useEffect(() => {
    invoke<string | null>("get_api_key").then((key) => {
      if (key) { setApiKey(key); setScreen("main"); }
    });
    invoke<string | null>("get_setting", { key: "language" }).then((v) => { if (v) setLanguage(v); });
    invoke<string | null>("get_setting", { key: "provider" }).then((v) => { if (v) setProvider(v); });
    invoke<string | null>("get_setting", { key: "theme" }).then((v) => { if (v) setTheme(v as "dark" | "light"); });
    invoke<string | null>("get_setting", { key: "format_enabled" }).then((v) => { if (v === "false") setFormatEnabled(false); });
    invoke<string | null>("get_setting", { key: "stt_model" }).then((v) => { if (v) setSttModel(v); });
    invoke<string | null>("get_setting", { key: "chat_model" }).then((v) => { if (v) setChatModel(v); });
    loadHistory();
  }, []);

  useEffect(() => { document.documentElement.setAttribute("data-theme", theme); }, [theme]);

  useEffect(() => {
    const u1 = listen<string>("recording-state", (e) => setStatus(e.payload as RecordingState));
    const u2 = listen<Transcription>("transcription-result", (e) => {
      const t = e.payload;
      setLastTranscription(t.formatted_text || t.raw_text);
      setHistory((prev) => [t, ...prev].slice(0, 50));
      setError("");
    });
    const u3 = listen<string>("transcription-error", (e) => setError(e.payload));
    const u4 = listen<string>("recopy-success", (e) => showNotification(e.payload));
    return () => { u1.then(f => f()); u2.then(f => f()); u3.then(f => f()); u4.then(f => f()); };
  }, []);

  const showNotification = (msg: string) => { setNotification(msg); setTimeout(() => setNotification(""), 2000); };
  const loadHistory = async () => { try { setHistory(await invoke<Transcription[]>("get_history", { limit: 50 })); } catch {} };
  const loadPlugins = async () => { try { setPlugins(await invoke<PluginInfo[]>("list_plugins")); } catch {} };
  const saveSetting = async (key: string, value: string) => { await invoke("set_setting", { key, value }); };

  const loadModels = async (providerName?: string, key?: string) => {
    setModelsLoading(true);
    try {
      const models = await invoke<ModelInfo[]>("fetch_models", {
        providerName: providerName || provider,
        apiKeyOverride: key || undefined,
      });
      setAvailableModels(models);
    } catch {
      setAvailableModels([]);
    }
    setModelsLoading(false);
  };

  const handleSearch = async () => {
    if (!searchQuery.trim()) { loadHistory(); return; }
    try { setHistory(await invoke<Transcription[]>("search_history", { query: searchQuery })); } catch {}
  };

  const handleSaveKey = async () => {
    if (!apiKey.trim()) return;
    try {
      await invoke("set_api_key", { key: apiKey.trim() });
      await saveSetting("provider", provider);
      if (sttModel) await saveSetting("stt_model", sttModel);
      if (chatModel) await saveSetting("chat_model", chatModel);
      setScreen("main");
      setError("");
    } catch (e) { setError(String(e)); }
  };

  const handleProviderChange = async (p: string) => {
    setProvider(p);
    setSttModel("");
    setChatModel("");
    setAvailableModels([]);
    await saveSetting("provider", p);
  };

  const handleStartRecording = async () => {
    setError(""); setStatus("recording");
    try { await invoke("start_recording"); } catch (e) { setError(String(e)); setStatus("idle"); }
  };

  const handleStopRecording = async () => {
    setStatus("transcribing");
    try {
      const result = await invoke<Transcription>("stop_recording_and_transcribe");
      setLastTranscription(result.formatted_text || result.raw_text);
      setHistory((prev) => [result, ...prev].slice(0, 50));
      setStatus("idle"); setError("");
    } catch (e) { setError(String(e)); setStatus("idle"); }
  };

  const sttModels = availableModels.filter(m => m.model_type === "stt");
  const chatModels = availableModels.filter(m => m.model_type === "chat");
  const providerInfo = PROVIDERS[provider] || PROVIDERS.groq;

  // ONBOARDING
  if (screen === "onboarding") {
    return (
      <main className="container">
        <h1>OpenFlow</h1>
        <p className="subtitle">Open-source voice transcription</p>
        <div className="onboarding">
          <div className="field">
            <label className="label">Provider</label>
            <select value={provider} onChange={(e) => handleProviderChange(e.target.value)}>
              {Object.entries(PROVIDERS).map(([k, v]) => (
                <option key={k} value={k}>{v.label}</option>
              ))}
            </select>
          </div>
          <p className="hint">
            Get a key at <a href={`https://${providerInfo.hint}`} target="_blank">{providerInfo.hint}</a>
          </p>
          <input
            type="password"
            value={apiKey}
            onChange={(e) => setApiKey(e.target.value)}
            placeholder={providerInfo.placeholder}
            onKeyDown={(e) => e.key === "Enter" && handleSaveKey()}
          />
          {apiKey.length > 10 && availableModels.length === 0 && !modelsLoading && (
            <button className="btn-secondary" onClick={() => loadModels(provider, apiKey)}>
              Load available models
            </button>
          )}
          {modelsLoading && <p className="hint">Loading models...</p>}
          {sttModels.length > 0 && (
            <div className="field">
              <label className="label">Speech-to-text model</label>
              <select value={sttModel} onChange={(e) => setSttModel(e.target.value)}>
                <option value="">Default</option>
                {sttModels.map(m => <option key={m.id} value={m.id}>{m.name || m.id}</option>)}
              </select>
            </div>
          )}
          {chatModels.length > 0 && (
            <div className="field">
              <label className="label">Formatting model</label>
              <select value={chatModel} onChange={(e) => setChatModel(e.target.value)}>
                <option value="">Default</option>
                {chatModels.map(m => <option key={m.id} value={m.id}>{m.name || m.id}</option>)}
              </select>
            </div>
          )}
          <button onClick={handleSaveKey} disabled={!apiKey.trim()}>Save & Continue</button>
          {error && <p className="error">{error}</p>}
        </div>
      </main>
    );
  }

  // HISTORY
  if (screen === "history") {
    return (
      <main className="container history-screen">
        <div className="nav-row">
          <button className="nav-btn" onClick={() => setScreen("main")}>Back</button>
          <h2>History</h2>
          <div />
        </div>
        <div className="search-row">
          <input value={searchQuery} onChange={(e) => setSearchQuery(e.target.value)} placeholder="Search transcriptions..." onKeyDown={(e) => e.key === "Enter" && handleSearch()} />
        </div>
        <div className="history-list">
          {history.length === 0 && <p className="empty">No transcriptions yet</p>}
          {history.map((item) => (
            <div key={item.id} className="history-card" onClick={() => { navigator.clipboard.writeText(item.formatted_text || item.raw_text); showNotification("Copied!"); }}>
              <p className="history-card-text">{item.formatted_text || item.raw_text}</p>
              <div className="history-card-meta">
                <span>{new Date(item.created_at).toLocaleString()}</span>
                {item.duration_ms && <span>{(item.duration_ms / 1000).toFixed(1)}s</span>}
                <span>{item.provider}</span>
                {item.language && <span>{item.language}</span>}
              </div>
            </div>
          ))}
        </div>
      </main>
    );
  }

  // PLUGINS
  if (screen === "plugins") {
    return (
      <main className="container settings-screen">
        <div className="nav-row">
          <button className="nav-btn" onClick={() => setScreen("settings")}>Back</button>
          <h2>Plugins</h2>
          <div />
        </div>
        <div className="settings-list">
          {plugins.length === 0 && (
            <div className="empty">
              <p>No plugins installed</p>
              <p className="hint" style={{ marginTop: 8 }}>
                Plugins live in <code>~/.openflow/plugins/</code>
              </p>
            </div>
          )}
          {plugins.map((p) => (
            <div key={p.manifest.id} className="setting-item">
              <div>
                <label>{p.manifest.name} <span className="setting-value">{p.manifest.version}</span></label>
                <p className="hint" style={{ textAlign: "left", marginTop: 4 }}>{p.manifest.description}</p>
              </div>
              <button className="toggle-btn" onClick={async () => {
                if (p.enabled) await invoke("disable_plugin", { id: p.manifest.id });
                else await invoke("enable_plugin", { id: p.manifest.id });
                loadPlugins();
              }}>
                {p.enabled ? "Enabled" : "Disabled"}
              </button>
            </div>
          ))}
        </div>
      </main>
    );
  }

  // SETTINGS
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
            <label>Provider</label>
            <select value={provider} onChange={(e) => handleProviderChange(e.target.value)}>
              {Object.entries(PROVIDERS).map(([k, v]) => (
                <option key={k} value={k}>{v.label}</option>
              ))}
            </select>
          </div>
          <div className="setting-item">
            <label>API Key</label>
            <input type="password" value={apiKey} onChange={(e) => setApiKey(e.target.value)} onBlur={() => apiKey && invoke("set_api_key", { key: apiKey })} />
          </div>
          <div className="setting-item">
            <label>Models</label>
            <button className="btn-secondary" onClick={() => loadModels()}>
              {modelsLoading ? "Loading..." : "Refresh models"}
            </button>
          </div>
          {sttModels.length > 0 && (
            <div className="setting-item">
              <label>STT Model</label>
              <select value={sttModel} onChange={(e) => { setSttModel(e.target.value); saveSetting("stt_model", e.target.value); }}>
                <option value="">Default</option>
                {sttModels.map(m => <option key={m.id} value={m.id}>{m.id}</option>)}
              </select>
            </div>
          )}
          {chatModels.length > 0 && (
            <div className="setting-item">
              <label>Chat Model</label>
              <select value={chatModel} onChange={(e) => { setChatModel(e.target.value); saveSetting("chat_model", e.target.value); }}>
                <option value="">Default</option>
                {chatModels.map(m => <option key={m.id} value={m.id}>{m.id}</option>)}
              </select>
            </div>
          )}
          <div className="setting-item">
            <label>Language</label>
            <select value={language} onChange={(e) => { setLanguage(e.target.value); saveSetting("language", e.target.value === "auto" ? "" : e.target.value); }}>
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
            <button className="toggle-btn" onClick={() => { const v = !formatEnabled; setFormatEnabled(v); saveSetting("format_enabled", String(v)); }}>
              {formatEnabled ? "On" : "Off"}
            </button>
          </div>
          <div className="setting-item">
            <label>Theme</label>
            <button className="toggle-btn" onClick={() => { const t = theme === "dark" ? "light" : "dark"; setTheme(t); saveSetting("theme", t); }}>
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
          <div className="setting-item">
            <label>Plugins</label>
            <button className="nav-btn" onClick={() => { loadPlugins(); setScreen("plugins"); }}>Manage</button>
          </div>
        </div>
      </main>
    );
  }

  // MAIN
  return (
    <main className="container">
      {notification && <div className="toast">{notification}</div>}
      <div className="top-row">
        <button className="icon-btn" onClick={() => { loadHistory(); setScreen("history"); }}>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>
        </button>
        <h1>OpenFlow</h1>
        <button className="icon-btn" onClick={() => setScreen("settings")}>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><circle cx="12" cy="12" r="3"/><path d="M12 1v2m0 18v2m-9-11h2m18 0h2m-4.2-5.8l-1.4 1.4M5.6 18.4l-1.4 1.4m0-13.8l1.4 1.4m12.8 12.8l1.4 1.4"/></svg>
        </button>
      </div>
      <div className="status-ring" data-status={status}>
        <div className="status-inner">
          {status === "idle" && "Ready"}
          {status === "recording" && "Recording..."}
          {status === "transcribing" && "Transcribing..."}
        </div>
      </div>
      <button className="record-btn" onMouseDown={handleStartRecording} onMouseUp={handleStopRecording} disabled={status === "transcribing"}>
        {status === "idle" && "Hold to Record"}
        {status === "recording" && "Release to Transcribe"}
        {status === "transcribing" && "Processing..."}
      </button>
      <p className="hint"><strong>Ctrl+Shift+Space</strong> anywhere &middot; <strong>Ctrl+Shift+V</strong> re-copy</p>
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
