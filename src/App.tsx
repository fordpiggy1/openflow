import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Screen = "onboarding" | "main" | "history" | "settings" | "plugins";
type RecordingState = "idle" | "recording" | "transcribing";
type OnboardingStep = "provider" | "apikey" | "test" | "done";

interface Transcription {
  id: string; raw_text: string; formatted_text: string | null; provider: string;
  duration_ms: number | null; context_type: string | null; window_title: string | null;
  language: string | null; created_at: string;
}

interface PluginInfo {
  manifest: { id: string; name: string; version: string; description: string; author: string | null; hooks: string[] };
  enabled: boolean; path: string;
}

interface ModelInfo { id: string; name: string; model_type: string; }

const TRANSCRIPTION_PROVIDERS: Record<string, { label: string; hint: string; placeholder: string; description: string }> = {
  groq: { label: "Groq", hint: "console.groq.com/keys", placeholder: "gsk_...", description: "Fastest. Free tier available." },
  openai: { label: "OpenAI", hint: "platform.openai.com/api-keys", placeholder: "sk-...", description: "Most reliable. Pay-per-use." },
  openrouter: { label: "OpenRouter", hint: "openrouter.ai/keys", placeholder: "sk-or-...", description: "300+ models including Whisper. Pay-per-use." },
  deepgram: { label: "Deepgram", hint: "console.deepgram.com", placeholder: "...", description: "Nova models. Free tier available." },
  custom: { label: "Custom", hint: "", placeholder: "API key", description: "Any OpenAI-compatible endpoint. Self-hosted, local, or third-party." },
};

const FORMATTING_PROVIDERS: Record<string, { label: string; hint: string; placeholder: string; description: string }> = {
  ...TRANSCRIPTION_PROVIDERS,
};

function App() {
  const [screen, setScreen] = useState<Screen>("onboarding");
  const [onboardingStep, setOnboardingStep] = useState<OnboardingStep>("provider");
  const [status, setStatus] = useState<RecordingState>("idle");
  const [lastTranscription, setLastTranscription] = useState("");
  const [error, setError] = useState("");
  const [notification, setNotification] = useState("");
  const [history, setHistory] = useState<Transcription[]>([]);
  const [searchQuery, setSearchQuery] = useState("");
  const [plugins, setPlugins] = useState<PluginInfo[]>([]);

  // Settings state
  const [transcriptionProvider, setTranscriptionProvider] = useState("groq");
  const [transcriptionKey, setTranscriptionKey] = useState("");
  const [transcriptionModel, setTranscriptionModel] = useState("");
  const [formattingProvider, setFormattingProvider] = useState("groq");
  const [formattingKey, setFormattingKey] = useState("");
  const [formattingModel, setFormattingModel] = useState("");
  const [sameProvider, setSameProvider] = useState(true);
  const [language, setLanguage] = useState("auto");
  const [theme, setTheme] = useState<"dark" | "light">("light");
  const [formatEnabled, setFormatEnabled] = useState(true);
  const [customTranscriptionUrl, setCustomTranscriptionUrl] = useState("");
  const [customTranscriptionModel, setCustomTranscriptionModel] = useState("");
  const [customFormattingUrl, setCustomFormattingUrl] = useState("");
  const [customFormattingModel, setCustomFormattingModel] = useState("");
  const [microphone, setMicrophone] = useState("");
  const [microphones, setMicrophones] = useState<{ id: string; name: string; is_default: boolean }[]>([]);
  const [transcriptionModels, setTranscriptionModels] = useState<ModelInfo[]>([]);
  const [formattingModels, setFormattingModels] = useState<ModelInfo[]>([]);
  const [modelsLoading, setModelsLoading] = useState(false);
  const [recordHotkey, setRecordHotkey] = useState("Option+V");
  const [recopyHotkey, setRecopyHotkey] = useState("Ctrl+Shift+V");
  const [editingHotkey, setEditingHotkey] = useState<null | "record" | "recopy">(null);

  useEffect(() => {
    (async () => {
      const key = await invoke<string | null>("get_api_key");
      if (key) {
        setTranscriptionKey(key);
        setFormattingKey(key);
        setScreen("main");
      }
      const tp = await invoke<string | null>("get_setting", { key: "provider" });
      if (tp) { setTranscriptionProvider(tp); setFormattingProvider(tp); }
      const sp = await invoke<string | null>("get_setting", { key: "same_provider" });
      if (sp === "false") {
        setSameProvider(false);
        const fp = await invoke<string | null>("get_setting", { key: "formatting_provider" });
        if (fp) setFormattingProvider(fp);
        const fk = await invoke<string | null>("get_setting", { key: "formatting_api_key" });
        if (fk) setFormattingKey(fk);
      }
      const lang = await invoke<string | null>("get_setting", { key: "language" });
      if (lang) setLanguage(lang);
      const th = await invoke<string | null>("get_setting", { key: "theme" });
      if (th) setTheme(th as "dark" | "light");
      const fe = await invoke<string | null>("get_setting", { key: "format_enabled" });
      if (fe === "false") setFormatEnabled(false);
      const sm = await invoke<string | null>("get_setting", { key: "stt_model" });
      if (sm) setTranscriptionModel(sm);
      const cm = await invoke<string | null>("get_setting", { key: "chat_model" });
      if (cm) setFormattingModel(cm);
      invoke<string | null>("get_setting", { key: "microphone" }).then((v) => { if (v) setMicrophone(v); });
      invoke<string | null>("get_setting", { key: "hotkey_record" }).then((v) => { if (v) setRecordHotkey(v); });
      invoke<string | null>("get_setting", { key: "hotkey_recopy" }).then((v) => { if (v) setRecopyHotkey(v); });
      loadHistory();
      loadMicrophones();
    })();
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
    const u5 = listen<string>("navigate", (e) => { if (e.payload === "history") { loadHistory(); setScreen("history"); } });
    return () => { u1.then(f => f()); u2.then(f => f()); u3.then(f => f()); u4.then(f => f()); u5.then(f => f()); };
  }, []);

  const showNotification = (msg: string) => { setNotification(msg); setTimeout(() => setNotification(""), 2000); };
  const loadHistory = async () => { try { setHistory(await invoke<Transcription[]>("get_history", { limit: 50 })); } catch {} };
  const loadPlugins = async () => { try { setPlugins(await invoke<PluginInfo[]>("list_plugins")); } catch {} };
  const loadMicrophones = async () => { try { setMicrophones(await invoke<{ id: string; name: string; is_default: boolean }[]>("list_audio_devices")); } catch {} };

  const save = async (key: string, value: string) => { await invoke("set_setting", { key, value }); };

  const loadModelsFor = async (providerName: string, apiKey: string, target: "transcription" | "formatting") => {
    setModelsLoading(true);
    try {
      const models = await invoke<ModelInfo[]>("fetch_models", { providerName, apiKeyOverride: apiKey });
      if (target === "transcription") setTranscriptionModels(models.filter(m => m.model_type === "stt"));
      else setFormattingModels(models.filter(m => m.model_type === "chat"));
    } catch { }
    setModelsLoading(false);
  };

  const handleSearch = async () => {
    if (!searchQuery.trim()) { loadHistory(); return; }
    try { setHistory(await invoke<Transcription[]>("search_history", { query: searchQuery })); } catch {}
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

  const getProviderValue = (name: string, customUrl: string) =>
    name === "custom" ? `custom:${customUrl.replace(/\/+$/, "")}` : name;

  const finishOnboarding = async () => {
    await invoke("set_api_key", { key: transcriptionKey.trim() });
    await save("provider", getProviderValue(transcriptionProvider, customTranscriptionUrl));
    await save("same_provider", String(sameProvider));
    if (transcriptionModel) await save("stt_model", transcriptionModel);
    if (customTranscriptionModel) await save("stt_model", customTranscriptionModel);
    if (!sameProvider) {
      await save("formatting_provider", getProviderValue(formattingProvider, customFormattingUrl));
      await save("formatting_api_key", formattingKey.trim());
      if (formattingModel) await save("chat_model", formattingModel);
      if (customFormattingModel) await save("chat_model", customFormattingModel);
    }
    setScreen("main");
  };

  const tProv = TRANSCRIPTION_PROVIDERS[transcriptionProvider as keyof typeof TRANSCRIPTION_PROVIDERS];
  const fProv = FORMATTING_PROVIDERS[formattingProvider as keyof typeof FORMATTING_PROVIDERS];

  // ─── ONBOARDING ─────────────────────────────────────────
  if (screen === "onboarding") {
    return (
      <main className="container">
        <img src="/laisy-blue.png" alt="OpenFlow" style={{ width: 48, height: 48 }} />
        <h1>OpenFlow</h1>
        <p className="subtitle">Open-source voice transcription</p>

        <div className="wizard">
          <div className="wizard-steps">
            <span className={onboardingStep === "provider" ? "step active" : "step"}>1</span>
            <span className="step-line" />
            <span className={onboardingStep === "apikey" ? "step active" : "step"}>2</span>
            <span className="step-line" />
            <span className={onboardingStep === "test" ? "step active" : "step"}>3</span>
          </div>

          {onboardingStep === "provider" && (
            <div className="wizard-content">
              <h2>Choose your transcription provider</h2>
              <p className="hint">This is the service that converts your voice to text.</p>
              <div className="provider-cards">
                {Object.entries(TRANSCRIPTION_PROVIDERS).map(([k, v]) => (
                  <button
                    key={k}
                    className={`provider-card ${transcriptionProvider === k ? "selected" : ""}`}
                    onClick={() => setTranscriptionProvider(k)}
                  >
                    <strong>{v.label}</strong>
                    <span>{v.description}</span>
                  </button>
                ))}
              </div>

              <div className="toggle-row">
                <label>Use same provider for text formatting?</label>
                <button className="toggle-btn" onClick={() => setSameProvider(!sameProvider)}>
                  {sameProvider ? "Yes" : "No"}
                </button>
              </div>

              {!sameProvider && (
                <>
                  <h2 style={{ marginTop: 16 }}>Formatting provider</h2>
                  <p className="hint">The LLM that cleans up punctuation and formatting.</p>
                  <div className="provider-cards">
                    {Object.entries(FORMATTING_PROVIDERS).map(([k, v]) => (
                      <button
                        key={k}
                        className={`provider-card ${formattingProvider === k ? "selected" : ""}`}
                        onClick={() => setFormattingProvider(k)}
                      >
                        <strong>{v.label}</strong>
                        <span>{v.description}</span>
                      </button>
                    ))}
                  </div>
                </>
              )}

              <button className="btn-primary" onClick={() => setOnboardingStep("apikey")}>
                Continue
              </button>
            </div>
          )}

          {onboardingStep === "apikey" && (
            <div className="wizard-content">
              <h2>Enter your API {sameProvider ? "key" : "keys"}</h2>

              {transcriptionProvider === "custom" && (
                <div className="field">
                  <label className="label">Transcription endpoint URL</label>
                  <p className="hint">OpenAI-compatible base URL (e.g. http://localhost:8080/v1)</p>
                  <input
                    value={customTranscriptionUrl}
                    onChange={(e) => setCustomTranscriptionUrl(e.target.value)}
                    placeholder="https://your-server.com/v1"
                  />
                  <label className="label" style={{ marginTop: 8 }}>Model name</label>
                  <input
                    value={customTranscriptionModel}
                    onChange={(e) => setCustomTranscriptionModel(e.target.value)}
                    placeholder="whisper-large-v3"
                  />
                </div>
              )}

              <div className="field">
                <label className="label">
                  {sameProvider ? "API Key" : `${tProv?.label || "Transcription"} key (transcription)`}
                </label>
                {tProv?.hint && (
                  <p className="hint">
                    Get one at <a href={`https://${tProv.hint}`} target="_blank">{tProv.hint}</a>
                  </p>
                )}
                <input
                  type="password"
                  value={transcriptionKey}
                  onChange={(e) => { setTranscriptionKey(e.target.value); if (sameProvider) setFormattingKey(e.target.value); }}
                  placeholder={tProv?.placeholder}
                />
              </div>

              {!sameProvider && (
                <div className="field" style={{ marginTop: 16 }}>
                  {formattingProvider === "custom" && (
                    <>
                      <label className="label">Formatting endpoint URL</label>
                      <p className="hint">OpenAI-compatible chat completions base URL</p>
                      <input
                        value={customFormattingUrl}
                        onChange={(e) => setCustomFormattingUrl(e.target.value)}
                        placeholder="https://your-server.com/v1"
                      />
                      <label className="label" style={{ marginTop: 8 }}>Model name</label>
                      <input
                        value={customFormattingModel}
                        onChange={(e) => setCustomFormattingModel(e.target.value)}
                        placeholder="llama-3.3-70b"
                      />
                    </>
                  )}
                  <label className="label" style={{ marginTop: formattingProvider === "custom" ? 8 : 0 }}>{fProv?.label || "Formatting"} key (formatting)</label>
                  {fProv?.hint && (
                    <p className="hint">
                      Get one at <a href={`https://${fProv.hint}`} target="_blank">{fProv.hint}</a>
                    </p>
                  )}
                  <input
                    type="password"
                    value={formattingKey}
                    onChange={(e) => setFormattingKey(e.target.value)}
                    placeholder={fProv?.placeholder}
                  />
                </div>
              )}

              {error && <p className="error">{error}</p>}

              <div className="wizard-nav">
                <button className="btn-secondary" onClick={() => setOnboardingStep("provider")}>Back</button>
                <button
                  className="btn-primary"
                  disabled={!transcriptionKey.trim() || (!sameProvider && !formattingKey.trim())}
                  onClick={() => { setError(""); setOnboardingStep("test"); }}
                >
                  Continue
                </button>
              </div>
            </div>
          )}

          {onboardingStep === "test" && (
            <div className="wizard-content">
              <h2>Optional: Choose models</h2>
              <p className="hint">Load available models from your provider, or use defaults.</p>

              <div className="field">
                <label className="label">Transcription model</label>
                <div className="model-row">
                  <select value={transcriptionModel} onChange={(e) => setTranscriptionModel(e.target.value)}>
                    <option value="">Default ({TRANSCRIPTION_PROVIDERS[transcriptionProvider as keyof typeof TRANSCRIPTION_PROVIDERS]?.label})</option>
                    {transcriptionModels.map(m => <option key={m.id} value={m.id}>{m.id}</option>)}
                  </select>
                  <button className="btn-secondary" onClick={() => loadModelsFor(transcriptionProvider, transcriptionKey, "transcription")}>
                    {modelsLoading ? "..." : "Load"}
                  </button>
                </div>
              </div>

              {(formatEnabled && !sameProvider) && (
                <div className="field">
                  <label className="label">Formatting model</label>
                  <div className="model-row">
                    <select value={formattingModel} onChange={(e) => setFormattingModel(e.target.value)}>
                      <option value="">Default ({FORMATTING_PROVIDERS[formattingProvider as keyof typeof FORMATTING_PROVIDERS]?.label})</option>
                      {formattingModels.map(m => <option key={m.id} value={m.id}>{m.id}</option>)}
                    </select>
                    <button className="btn-secondary" onClick={() => loadModelsFor(formattingProvider, formattingKey, "formatting")}>
                      {modelsLoading ? "..." : "Load"}
                    </button>
                  </div>
                </div>
              )}

              <div className="wizard-nav">
                <button className="btn-secondary" onClick={() => setOnboardingStep("apikey")}>Back</button>
                <button className="btn-primary" onClick={finishOnboarding}>
                  Start using OpenFlow
                </button>
              </div>
            </div>
          )}
        </div>
      </main>
    );
  }

  // ─── HISTORY ────────────────────────────────────────────
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
              </div>
            </div>
          ))}
        </div>
      </main>
    );
  }

  // ─── PLUGINS ────────────────────────────────────────────
  if (screen === "plugins") {
    return (
      <main className="container settings-screen">
        <div className="nav-row">
          <button className="nav-btn" onClick={() => setScreen("settings")}>Back</button>
          <h2>Plugins</h2>
          <div />
        </div>
        <div className="settings-list">
          {plugins.length === 0 && <div className="empty"><p>No plugins installed</p><p className="hint" style={{ marginTop: 8 }}>Plugins live in <code>~/.openflow/plugins/</code></p></div>}
          {plugins.map((p) => (
            <div key={p.manifest.id} className="setting-item">
              <div><label>{p.manifest.name} <span className="setting-value">{p.manifest.version}</span></label><p className="hint" style={{ textAlign: "left", marginTop: 4 }}>{p.manifest.description}</p></div>
              <button className="toggle-btn" onClick={async () => { if (p.enabled) await invoke("disable_plugin", { id: p.manifest.id }); else await invoke("enable_plugin", { id: p.manifest.id }); loadPlugins(); }}>{p.enabled ? "Enabled" : "Disabled"}</button>
            </div>
          ))}
        </div>
      </main>
    );
  }

  // ─── SETTINGS ───────────────────────────────────────────
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
            <label>Transcription provider</label>
            <select value={transcriptionProvider} onChange={(e) => { setTranscriptionProvider(e.target.value); save("provider", e.target.value); if (sameProvider) setFormattingProvider(e.target.value); }}>
              {Object.entries(TRANSCRIPTION_PROVIDERS).map(([k, v]) => <option key={k} value={k}>{v.label}</option>)}
            </select>
          </div>
          <div className="setting-item">
            <label>Transcription key</label>
            <input type="password" value={transcriptionKey} onChange={(e) => setTranscriptionKey(e.target.value)} onBlur={() => transcriptionKey && invoke("set_api_key", { key: transcriptionKey })} />
          </div>
          <div className="setting-item">
            <label>Same provider for formatting?</label>
            <button className="toggle-btn" onClick={() => { const v = !sameProvider; setSameProvider(v); save("same_provider", String(v)); if (v) { setFormattingProvider(transcriptionProvider); setFormattingKey(transcriptionKey); } }}>
              {sameProvider ? "Yes" : "No"}
            </button>
          </div>
          {!sameProvider && (
            <>
              <div className="setting-item">
                <label>Formatting provider</label>
                <select value={formattingProvider} onChange={(e) => { setFormattingProvider(e.target.value); save("formatting_provider", e.target.value); }}>
                  {Object.entries(FORMATTING_PROVIDERS).map(([k, v]) => <option key={k} value={k}>{v.label}</option>)}
                </select>
              </div>
              <div className="setting-item">
                <label>Formatting key</label>
                <input type="password" value={formattingKey} onChange={(e) => setFormattingKey(e.target.value)} onBlur={() => formattingKey && save("formatting_api_key", formattingKey)} />
              </div>
            </>
          )}
          <div className="setting-item">
            <label>Microphone</label>
            <select value={microphone} onChange={(e) => { setMicrophone(e.target.value); save("microphone", e.target.value); }}>
              <option value="">System default</option>
              {microphones.map(m => <option key={m.id} value={m.id}>{m.name}{m.is_default ? " (default)" : ""}</option>)}
            </select>
          </div>
          <div className="setting-item">
            <label>Language</label>
            <select value={language} onChange={(e) => { setLanguage(e.target.value); save("language", e.target.value === "auto" ? "" : e.target.value); }}>
              <option value="auto">Auto-detect</option>
              <option value="en">English</option><option value="es">Spanish</option><option value="fr">French</option>
              <option value="de">German</option><option value="it">Italian</option><option value="pt">Portuguese</option>
              <option value="nl">Dutch</option><option value="ja">Japanese</option><option value="ko">Korean</option>
              <option value="zh">Chinese</option><option value="ar">Arabic</option><option value="hi">Hindi</option>
              <option value="ru">Russian</option>
            </select>
          </div>
          <div className="setting-item">
            <label>LLM Formatting</label>
            <button className="toggle-btn" onClick={() => { const v = !formatEnabled; setFormatEnabled(v); save("format_enabled", String(v)); }}>{formatEnabled ? "On" : "Off"}</button>
          </div>
          <div className="setting-item">
            <label>Theme</label>
            <button className="toggle-btn" onClick={() => { const t = theme === "dark" ? "light" : "dark"; setTheme(t); save("theme", t); }}>{theme === "dark" ? "Dark" : "Light"}</button>
          </div>
          <div className="setting-item">
            <label>Record hotkey</label>
            {editingHotkey === "record" ? (
              <input
                className="hotkey-input"
                autoFocus
                value={recordHotkey}
                onChange={(e) => setRecordHotkey(e.target.value)}
                onBlur={async () => {
                  try { await invoke("rebind_hotkey", { action: "record", shortcutStr: recordHotkey }); } catch (e) { setError(String(e)); }
                  setEditingHotkey(null);
                }}
                onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
                placeholder="e.g. Option+V"
              />
            ) : (
              <button className="setting-value clickable" onClick={() => setEditingHotkey("record")}>{recordHotkey}</button>
            )}
          </div>
          <div className="setting-item">
            <label>Re-copy hotkey</label>
            {editingHotkey === "recopy" ? (
              <input
                className="hotkey-input"
                autoFocus
                value={recopyHotkey}
                onChange={(e) => setRecopyHotkey(e.target.value)}
                onBlur={async () => {
                  try { await invoke("rebind_hotkey", { action: "recopy", shortcutStr: recopyHotkey }); } catch (e) { setError(String(e)); }
                  setEditingHotkey(null);
                }}
                onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
                placeholder="e.g. Ctrl+Shift+V"
              />
            ) : (
              <button className="setting-value clickable" onClick={() => setEditingHotkey("recopy")}>{recopyHotkey}</button>
            )}
          </div>
          <div className="setting-item"><label>Plugins</label><button className="nav-btn" onClick={() => { loadPlugins(); setScreen("plugins"); }}>Manage</button></div>
        </div>
      </main>
    );
  }

  // ─── MAIN ───────────────────────────────────────────────
  return (
    <main className="container">
      {notification && <div className="toast">{notification}</div>}
      <div className="top-row">
        <button className="icon-btn" onClick={() => { loadHistory(); setScreen("history"); }}>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>
        </button>
        <img src="/logo-128.png" alt="OpenFlow" className="topbar-logo" />
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
      <p className="hint"><strong>{recordHotkey}</strong> anywhere &middot; <strong>{recopyHotkey}</strong> re-copy</p>
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
