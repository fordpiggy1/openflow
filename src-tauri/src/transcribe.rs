use reqwest::multipart;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct WhisperResponse {
    text: String,
}

#[derive(Clone, Debug)]
pub enum Provider {
    Groq,
    OpenAI,
    OpenRouter,
    Deepgram,
}

impl Provider {
    pub fn from_str(s: &str) -> Self {
        match s {
            "openai" => Self::OpenAI,
            "openrouter" => Self::OpenRouter,
            "deepgram" => Self::Deepgram,
            _ => Self::Groq,
        }
    }

    fn base_url(&self) -> &str {
        match self {
            Self::Groq => "https://api.groq.com/openai/v1",
            Self::OpenAI => "https://api.openai.com/v1",
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            Self::Deepgram => "https://api.deepgram.com/v1",
        }
    }

    fn transcription_url(&self) -> String {
        match self {
            Self::Deepgram => format!("{}/listen", self.base_url()),
            _ => format!("{}/audio/transcriptions", self.base_url()),
        }
    }

    fn chat_url(&self) -> String {
        match self {
            Self::Deepgram => "https://api.groq.com/openai/v1/chat/completions".to_string(),
            _ => format!("{}/chat/completions", self.base_url()),
        }
    }

    fn models_url(&self) -> String {
        match self {
            Self::Deepgram => String::new(),
            _ => format!("{}/models", self.base_url()),
        }
    }

    pub fn default_stt_model(&self) -> &str {
        match self {
            Self::Groq => "whisper-large-v3-turbo",
            Self::OpenAI => "whisper-1",
            Self::OpenRouter => "openai/whisper-large-v3",
            Self::Deepgram => "nova-3",
        }
    }

    pub fn default_chat_model(&self) -> &str {
        match self {
            Self::Groq => "llama-3.3-70b-versatile",
            Self::OpenAI => "gpt-4o-mini",
            Self::OpenRouter => "meta-llama/llama-3.3-70b-instruct",
            Self::Deepgram => "llama-3.3-70b-versatile",
        }
    }

    fn auth_header(&self, api_key: &str) -> (String, String) {
        match self {
            Self::Deepgram => ("Authorization".to_string(), format!("Token {}", api_key)),
            _ => ("Authorization".to_string(), format!("Bearer {}", api_key)),
        }
    }

    pub fn supports_stt(&self) -> bool {
        !matches!(self, Self::OpenRouter)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub model_type: String,
}

pub async fn fetch_models(api_key: &str, provider: &Provider) -> Result<Vec<ModelInfo>, String> {
    match provider {
        Provider::Deepgram => Ok(vec![
            ModelInfo { id: "nova-3".into(), name: "Nova 3".into(), model_type: "stt".into() },
            ModelInfo { id: "nova-2".into(), name: "Nova 2".into(), model_type: "stt".into() },
        ]),
        _ => fetch_openai_compatible_models(api_key, provider).await,
    }
}

async fn fetch_openai_compatible_models(api_key: &str, provider: &Provider) -> Result<Vec<ModelInfo>, String> {
    let client = reqwest::Client::new();
    let url = provider.models_url();
    if url.is_empty() {
        return Ok(vec![]);
    }

    let (header_name, header_val) = provider.auth_header(api_key);

    let response = client
        .get(&url)
        .header(&header_name, &header_val)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch models: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("Models API returned {}", response.status()));
    }

    let body: serde_json::Value = response.json().await
        .map_err(|e| format!("Failed to parse models: {}", e))?;

    let data = body["data"].as_array()
        .ok_or("No models data in response")?;

    let mut models = Vec::new();
    for m in data {
        let id = m["id"].as_str().unwrap_or_default().to_string();
        if id.is_empty() { continue; }

        let id_lower = id.to_lowercase();

        let is_stt = id_lower.contains("whisper")
            || (id_lower.contains("distil") && id_lower.contains("whisper"));
        let is_skip = id_lower.contains("tts")
            || id_lower.contains("dall")
            || id_lower.contains("embed")
            || id_lower.contains("moderation")
            || id_lower.contains("image");

        if is_skip { continue; }

        let model_type = if is_stt { "stt" } else { "chat" };

        let name = m["name"].as_str()
            .unwrap_or(&id)
            .to_string();

        models.push(ModelInfo {
            id: id.clone(),
            name,
            model_type: model_type.to_string(),
        });
    }

    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

pub async fn transcribe_audio(
    wav_bytes: Vec<u8>,
    api_key: &str,
    language: Option<&str>,
    provider: &Provider,
    model: Option<&str>,
) -> Result<String, String> {
    match provider {
        Provider::Deepgram => transcribe_deepgram(wav_bytes, api_key, language, model).await,
        _ => transcribe_whisper(wav_bytes, api_key, language, provider, model).await,
    }
}

async fn transcribe_whisper(
    wav_bytes: Vec<u8>,
    api_key: &str,
    language: Option<&str>,
    provider: &Provider,
    model: Option<&str>,
) -> Result<String, String> {
    let client = reqwest::Client::new();
    let stt_model = model.unwrap_or(provider.default_stt_model());

    let file_part = multipart::Part::bytes(wav_bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| format!("Multipart error: {}", e))?;

    let mut form = multipart::Form::new()
        .part("file", file_part)
        .text("model", stt_model.to_string())
        .text("response_format", "json");

    if let Some(lang) = language {
        form = form.text("language", lang.to_string());
    }

    let (header_name, header_val) = provider.auth_header(api_key);

    let mut req = client
        .post(&provider.transcription_url())
        .header(&header_name, &header_val)
        .multipart(form)
        .timeout(std::time::Duration::from_secs(30));

    if matches!(provider, Provider::OpenRouter) {
        req = req.header("HTTP-Referer", "https://openflow.dev");
        req = req.header("X-Title", "OpenFlow");
    }

    let response = req.send().await
        .map_err(|e| format!("Request failed: {}", e))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return match status.as_u16() {
            401 | 403 => Err("Invalid API key. Check your API key in settings.".to_string()),
            429 => Err("Rate limit hit. Wait a moment and try again.".to_string()),
            _ => Err(format!("API error ({}): {}", status, body)),
        };
    }

    let result: WhisperResponse = response.json().await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    Ok(result.text)
}

async fn transcribe_deepgram(
    wav_bytes: Vec<u8>,
    api_key: &str,
    language: Option<&str>,
    model: Option<&str>,
) -> Result<String, String> {
    let client = reqwest::Client::new();
    let dg_model = model.unwrap_or("nova-3");

    let mut url = format!("https://api.deepgram.com/v1/listen?model={}&smart_format=true", dg_model);
    if let Some(lang) = language {
        url.push_str(&format!("&language={}", lang));
    }

    let response = client
        .post(&url)
        .header("Authorization", format!("Token {}", api_key))
        .header("Content-Type", "audio/wav")
        .body(wav_bytes)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("Deepgram request failed: {}", e))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return match status.as_u16() {
            401 | 403 => Err("Invalid Deepgram API key.".to_string()),
            429 => Err("Deepgram rate limit hit.".to_string()),
            _ => Err(format!("Deepgram error ({}): {}", status, body)),
        };
    }

    let result: serde_json::Value = response.json().await
        .map_err(|e| format!("Parse error: {}", e))?;

    result["results"]["channels"][0]["alternatives"][0]["transcript"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or("No transcript in Deepgram response".to_string())
}

const FORMAT_SYSTEM_PROMPT: &str = "\
Format this dictation into clean text. Rules:
1. Add punctuation, capitalization, and paragraph breaks.
2. Interpret voice commands naturally:
   - \"new paragraph\" or \"next paragraph\" = start a new paragraph
   - \"new line\" or \"next line\" = line break
   - \"period\" / \"full stop\" / \"comma\" / \"question mark\" / \"exclamation mark\" = insert that punctuation
   - \"scratch that\" or \"delete that\" = remove the previous sentence
   - \"undo\" or \"go back\" = remove the last few words
   - \"all caps\" + word = CAPITALIZE that word
3. Remove filler words (um, uh, like, you know) unless they seem intentional.
4. Do not add content that was not spoken.
5. Output ONLY the formatted text, nothing else.";

pub async fn format_text(
    raw_text: &str,
    api_key: &str,
    context: Option<&str>,
    provider: &Provider,
    chat_model: Option<&str>,
) -> Result<String, String> {
    let client = reqwest::Client::new();
    let model = chat_model.unwrap_or(provider.default_chat_model());

    let user_content = match context {
        Some(ctx) => format!("Context: {}\n\nDictation: {}", ctx, raw_text),
        None => raw_text.to_string(),
    };

    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": FORMAT_SYSTEM_PROMPT},
            {"role": "user", "content": user_content}
        ],
        "temperature": 0.3,
        "max_tokens": 2048
    });

    let (header_name, header_val) = provider.auth_header(api_key);

    let mut req = client
        .post(&provider.chat_url())
        .header(&header_name, &header_val)
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(15));

    if matches!(provider, Provider::OpenRouter) {
        req = req.header("HTTP-Referer", "https://openflow.dev");
        req = req.header("X-Title", "OpenFlow");
    }

    let response = req.send().await
        .map_err(|e| format!("Format request failed: {}", e))?;

    if !response.status().is_success() {
        return Err("LLM formatting failed".to_string());
    }

    let result: serde_json::Value = response.json().await
        .map_err(|e| format!("Parse error: {}", e))?;

    result["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or("No content in response".to_string())
}
