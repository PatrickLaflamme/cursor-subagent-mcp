use crate::errors::SummarizeError;
use crate::summarize::{SummarizeResult, Summarizer};

#[derive(Default)]
pub struct ExtractiveSummarizer;

impl Summarizer for ExtractiveSummarizer {
    fn summarize(
        &self,
        context: &str,
        _instructions: Option<&str>,
        max_tokens: usize,
    ) -> Result<SummarizeResult, SummarizeError> {
        // Very simple heuristic: take the first N sentences up to rough token budget
        let max_tokens = max_tokens.min(1000);
        let approx_tokens_per_char = 0.25; // crude
        let max_chars = (max_tokens as f64 / approx_tokens_per_char) as usize;
        let mut out = String::new();
        let mut used = 0usize;
        for sentence in context.split_terminator(['.', '!', '?']) {
            let s = sentence.trim();
            if s.is_empty() {
                continue;
            }
            let add = s.len() + 1;
            if used + add > max_chars {
                break;
            }
            if !out.is_empty() {
                out.push_str(". ");
            }
            out.push_str(s);
            used += add;
        }
        if out.is_empty() {
            out = context.chars().take(max_chars).collect();
        }
        Ok(SummarizeResult {
            summary: out,
            tokens_used: max_tokens,
            backend: "textrank".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respects_token_cap_and_sentence_selection() {
        let s = ExtractiveSummarizer::default();
        let context = "Sentence one is short. Sentence two is a little bit longer! And question three? Trailing.";
        let res = s.summarize(context, None, 40).expect("summarize");
        assert_eq!(res.tokens_used, 40);
        assert!(res
            .summary
            .starts_with("Sentence one is short. Sentence two"));
    }

    #[test]
    fn falls_back_to_char_truncation_when_no_sentence_boundaries() {
        let s = ExtractiveSummarizer::default();
        let context = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let res = s.summarize(context, None, 8).expect("summarize");
        assert!(res.summary.len() <= 40);
    }
}
