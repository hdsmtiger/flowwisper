use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait SpeechEngine: Send + Sync {
    async fn transcribe(&self, frame: &[f32]) -> Result<String>;
}

#[async_trait]
pub trait SentencePolisher: Send + Sync {
    async fn polish(&self, sentence: &str) -> Result<String>;
}

#[derive(Debug, Default)]
pub(crate) struct LightweightSentencePolisher;

impl LightweightSentencePolisher {
    fn normalize(sentence: &str) -> String {
        let trimmed = sentence.trim();
        if trimmed.is_empty() {
            return String::new();
        }

        let mut tokens: Vec<String> = trimmed
            .split_whitespace()
            .map(|token| token.trim_matches(|c: char| c.is_control()).to_string())
            .filter(|token| !token.is_empty())
            .collect();

        while tokens
            .first()
            .map(|token| Self::is_disfluency(token))
            .unwrap_or(false)
        {
            tokens.remove(0);
        }

        for token in tokens.iter_mut() {
            let lower = token.to_ascii_lowercase();
            match lower.as_str() {
                "i" => *token = "I".into(),
                "i'm" => *token = "I'm".into(),
                "i'd" => *token = "I'd".into(),
                "i've" => *token = "I've".into(),
                "i'll" => *token = "I'll".into(),
                _ => {}
            }
        }

        let mut text = tokens.join(" ");
        for mark in [",", ".", "!", "?", ";", ":"] {
            let pattern = format!(" {mark}");
            text = text.replace(&pattern, mark);
        }

        Self::capitalize_start(&mut text);

        if let Some(last) = text.chars().last() {
            if !matches!(last, '.' | '!' | '?' | '。' | '！' | '？' | '…') {
                text.push('.');
            }
        } else {
            text.push('.');
        }

        text
    }

    fn is_disfluency(token: &str) -> bool {
        matches!(
            token.to_ascii_lowercase().as_str(),
            "uh" | "um" | "erm" | "ah" | "eh" | "hmm"
        )
    }

    fn capitalize_start(text: &mut String) {
        let mut chars: Vec<char> = text.chars().collect();
        for ch in chars.iter_mut() {
            if ch.is_alphabetic() {
                if ch.is_lowercase() {
                    if let Some(upper) = ch.to_uppercase().next() {
                        *ch = upper;
                    }
                }
                break;
            }
        }
        *text = chars.into_iter().collect();
    }
}

#[async_trait]
impl SentencePolisher for LightweightSentencePolisher {
    async fn polish(&self, sentence: &str) -> Result<String> {
        Ok(Self::normalize(sentence))
    }
}
