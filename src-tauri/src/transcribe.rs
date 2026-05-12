use reqwest::multipart;
use serde::Deserialize;

#[derive(Deserialize)]
struct GroqResponse {
    text: String,
}

pub async fn transcribe_audio(wav_bytes: Vec<u8>, api_key: &str, language: Option<&str>) -> Result<String, String> {
    let client = reqwest::Client::new();

    let file_part = multipart::Part::bytes(wav_bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| format!("Multipart error: {}", e))?;

    let mut form = multipart::Form::new()
        .part("file", file_part)
        .text("model", "whisper-large-v3-turbo")
        .text("response_format", "json");

    if let Some(lang) = language {
        form = form.text("language", lang.to_string());
    }

    let response = client
        .post("https://api.groq.com/openai/v1/audio/transcriptions")
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
            401 | 403 => Err("Invalid API key. Check your Groq API key in settings.".to_string()),
            429 => Err("Rate limit hit. Wait a moment and try again.".to_string()),
            _ => Err(format!("Groq API error ({}): {}", status, body)),
        };
    }

    let result: GroqResponse = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    Ok(result.text)
}

pub async fn format_text(raw_text: &str, api_key: &str, context: Option<&str>) -> Result<String, String> {
    let client = reqwest::Client::new();

    let system_prompt = "Format this transcription. Add punctuation, paragraph breaks, and appropriate tone. Do not add content. Output only the formatted text.";

    let user_content = match context {
        Some(ctx) => format!("Context: {}\n\nTranscription: {}", ctx, raw_text),
        None => raw_text.to_string(),
    };

    let body = serde_json::json!({
        "model": "llama-3.3-70b-versatile",
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_content}
        ],
        "temperature": 0.3,
        "max_tokens": 2048
    });

    let response = client
        .post("https://api.groq.com/openai/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
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
