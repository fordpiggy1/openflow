use reqwest::multipart;
use serde::Deserialize;

#[derive(Deserialize)]
struct WhisperResponse {
    text: String,
}

#[derive(Clone, Debug)]
pub enum Provider {
    Groq,
    OpenAI,
    Deepgram,
}

impl Provider {
    pub fn from_str(s: &str) -> Self {
        match s {
            "openai" => Self::OpenAI,
            "deepgram" => Self::Deepgram,
            _ => Self::Groq,
        }
    }

    fn transcription_url(&self) -> &str {
        match self {
            Self::Groq => "https://api.groq.com/openai/v1/audio/transcriptions",
            Self::OpenAI => "https://api.openai.com/v1/audio/transcriptions",
            Self::Deepgram => "https://api.deepgram.com/v1/listen",
        }
    }

    fn whisper_model(&self) -> &str {
        match self {
            Self::Groq => "whisper-large-v3-turbo",
            Self::OpenAI => "whisper-1",
            Self::Deepgram => "nova-3",
        }
    }

    fn chat_url(&self) -> &str {
        match self {
            Self::Groq => "https://api.groq.com/openai/v1/chat/completions",
            Self::OpenAI => "https://api.openai.com/v1/chat/completions",
            Self::Deepgram => "https://api.groq.com/openai/v1/chat/completions",
        }
    }

    fn chat_model(&self) -> &str {
        match self {
            Self::Groq => "llama-3.3-70b-versatile",
            Self::OpenAI => "gpt-4o-mini",
            Self::Deepgram => "llama-3.3-70b-versatile",
        }
    }
}

pub async fn transcribe_audio(
    wav_bytes: Vec<u8>,
    api_key: &str,
    language: Option<&str>,
    provider: &Provider,
) -> Result<String, String> {
    match provider {
        Provider::Deepgram => transcribe_deepgram(wav_bytes, api_key, language).await,
        _ => transcribe_whisper(wav_bytes, api_key, language, provider).await,
    }
}

async fn transcribe_whisper(
    wav_bytes: Vec<u8>,
    api_key: &str,
    language: Option<&str>,
    provider: &Provider,
) -> Result<String, String> {
    let client = reqwest::Client::new();

    let file_part = multipart::Part::bytes(wav_bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| format!("Multipart error: {}", e))?;

    let mut form = multipart::Form::new()
        .part("file", file_part)
        .text("model", provider.whisper_model().to_string())
        .text("response_format", "json");

    if let Some(lang) = language {
        form = form.text("language", lang.to_string());
    }

    let response = client
        .post(provider.transcription_url())
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
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

    let result: WhisperResponse = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    Ok(result.text)
}

async fn transcribe_deepgram(
    wav_bytes: Vec<u8>,
    api_key: &str,
    language: Option<&str>,
) -> Result<String, String> {
    let client = reqwest::Client::new();

    let mut url = "https://api.deepgram.com/v1/listen?model=nova-3&smart_format=true".to_string();
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

    let result: serde_json::Value = response
        .json()
        .await
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
    format_api_key: Option<&str>,
) -> Result<String, String> {
    let client = reqwest::Client::new();

    let key = format_api_key.unwrap_or(api_key);

    let user_content = match context {
        Some(ctx) => format!("Context: {}\n\nDictation: {}", ctx, raw_text),
        None => raw_text.to_string(),
    };

    let body = serde_json::json!({
        "model": provider.chat_model(),
        "messages": [
            {"role": "system", "content": FORMAT_SYSTEM_PROMPT},
            {"role": "user", "content": user_content}
        ],
        "temperature": 0.3,
        "max_tokens": 2048
    });

    let response = client
        .post(provider.chat_url())
        .header("Authorization", format!("Bearer {}", key))
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("Format request failed: {}", e))?;

    if !response.status().is_success() {
        return Err("LLM formatting failed".to_string());
    }

    let result: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Parse error: {}", e))?;

    result["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or("No content in response".to_string())
}
