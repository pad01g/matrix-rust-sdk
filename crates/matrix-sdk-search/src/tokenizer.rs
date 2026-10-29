// Copyright 2026 The Matrix.org Foundation C.I.C.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! A tokenizer for scripts that don't separate words with spaces.
//!
//! Runs of [`is_bigram_script`] characters become overlapping bigrams, plus one
//! unigram per character so that one-character queries match. Other text is
//! split like Tantivy's default tokenizer. Text is NFKC-normalised first.

use tantivy::tokenizer::{
    LowerCaser, PreTokenizedStream, PreTokenizedString, RemoveLongFilter, TextAnalyzer, Token,
    Tokenizer,
};
use unicode_normalization::UnicodeNormalization;

/// The name the tokenizer is registered under, and which is stored in the index
/// schema. Bump it whenever the tokens change, so existing indexes are rebuilt.
pub(crate) const TOKENIZER_NAME: &str = "matrix_bigram_v1";

/// The analyzer for message bodies; see the [module documentation](self).
pub(crate) fn text_analyzer() -> TextAnalyzer {
    // Same filters as Tantivy's default tokenizer.
    TextAnalyzer::builder(BigramTokenizer)
        .filter(RemoveLongFilter::limit(40))
        .filter(LowerCaser)
        .build()
}

#[derive(Clone, Debug)]
struct BigramTokenizer;

impl Tokenizer for BigramTokenizer {
    type TokenStream<'a> = PreTokenizedStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        tokenize(text).into()
    }
}

fn tokenize(text: &str) -> PreTokenizedString {
    // Token offsets point into this normalised text, not the original one.
    let text: String = text.nfkc().collect();
    let mut tokens = Vec::new();
    let mut position = 0;
    let mut chars = text.char_indices().peekable();

    while let Some(&(start, c)) = chars.peek() {
        if is_bigram_script(c) {
            let mut run = Vec::new();
            while let Some(&(offset, c)) = chars.peek() {
                if is_bigram_script(c) {
                    run.push((offset, c));
                } else if !is_ignorable_in_run(c) {
                    break;
                }
                chars.next();
            }
            push_run(&run, &mut position, &mut tokens);
        } else if c.is_alphanumeric() {
            let mut end = start;
            while let Some((offset, c)) =
                chars.next_if(|&(_, c)| c.is_alphanumeric() && !is_bigram_script(c))
            {
                end = offset + c.len_utf8();
            }
            tokens.push(token(text[start..end].to_owned(), start, end, position));
            position += 1;
        } else {
            chars.next();
        }
    }

    PreTokenizedString { text, tokens }
}

fn push_run(run: &[(usize, char)], position: &mut usize, tokens: &mut Vec<Token>) {
    for (i, &(start, c)) in run.iter().enumerate() {
        tokens.push(token(c.to_string(), start, start + c.len_utf8(), *position));
        if let Some(&(next_start, next)) = run.get(i + 1) {
            let end = next_start + next.len_utf8();
            tokens.push(token(format!("{c}{next}"), start, end, *position));
        }
        *position += 1;
    }
}

fn token(text: String, offset_from: usize, offset_to: usize, position: usize) -> Token {
    Token { offset_from, offset_to, position, text, position_length: 1 }
}

/// Whether a character belongs to a script written without spaces between
/// words, or whose words take attached particles (Hangul).
///
/// Thai, Lao, Khmer and Myanmar are split per code point rather than with a
/// dictionary, which is coarse but keeps any word inside a sentence findable.
fn is_bigram_script(c: char) -> bool {
    matches!(c,
        // Han, including the iteration, closing and repeat marks.
        '\u{3005}'..='\u{3007}'
        | '\u{3031}'..='\u{3035}'
        | '\u{303B}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{4E00}'..='\u{9FFF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{20000}'..='\u{2FA1F}'
        | '\u{30000}'..='\u{3347F}'
        // Hiragana and Katakana, but not the Katakana middle dot.
        | '\u{3041}'..='\u{309F}'
        | '\u{30A1}'..='\u{30FA}'
        | '\u{30FC}'..='\u{30FF}'
        | '\u{31F0}'..='\u{31FF}'
        | '\u{1AFF0}'..='\u{1B16F}'
        // Bopomofo.
        | '\u{3100}'..='\u{312F}'
        | '\u{31A0}'..='\u{31BF}'
        // Hangul.
        | '\u{1100}'..='\u{11FF}'
        | '\u{3130}'..='\u{318F}'
        | '\u{A960}'..='\u{A97F}'
        | '\u{AC00}'..='\u{D7FF}'
        // Thai, without the currency sign and punctuation.
        | '\u{0E01}'..='\u{0E3A}'
        | '\u{0E40}'..='\u{0E4E}'
        | '\u{0E50}'..='\u{0E59}'
        // Lao.
        | '\u{0E80}'..='\u{0EFF}'
        // Myanmar, without punctuation.
        | '\u{1000}'..='\u{1049}'
        | '\u{1050}'..='\u{109F}'
        | '\u{A9E0}'..='\u{A9FF}'
        | '\u{AA60}'..='\u{AA7F}'
        // Khmer, without punctuation.
        | '\u{1780}'..='\u{17D3}'
        | '\u{17D7}'
        | '\u{17DC}'..='\u{17FF}'
        | '\u{19E0}'..='\u{19FF}'
    )
}

/// Characters that don't end a run: variation selectors (e.g. ideographic
/// variants of names) and zero-width characters used inside Khmer or Myanmar
/// words.
fn is_ignorable_in_run(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200D}' | '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}')
}

#[cfg(test)]
mod tests {
    use tantivy::tokenizer::TokenStream;

    use super::text_analyzer;

    fn tokens(text: &str) -> Vec<(String, usize)> {
        let mut analyzer = text_analyzer();
        let mut stream = analyzer.token_stream(text);
        let mut tokens = Vec::new();
        while stream.advance() {
            tokens.push((stream.token().text.clone(), stream.token().position));
        }
        tokens
    }

    fn texts(text: &str) -> Vec<String> {
        tokens(text).into_iter().map(|(text, _)| text).collect()
    }

    #[test]
    fn test_spaced_scripts_split_like_the_default_tokenizer() {
        assert_eq!(texts("Hello, matrix.org! Ça va?"), ["hello", "matrix", "org", "ça", "va"]);
        assert_eq!(texts("Встретимся завтра"), ["встретимся", "завтра"]);
        assert_eq!(texts(&format!("{} ok", "a".repeat(40))), ["ok"]);
    }

    #[test]
    fn test_runs_emit_unigrams_and_bigrams_per_position() {
        assert_eq!(
            tokens("駅前で"),
            [
                ("駅".to_owned(), 0),
                ("駅前".to_owned(), 0),
                ("前".to_owned(), 1),
                ("前で".to_owned(), 1),
                ("で".to_owned(), 2),
            ]
        );
    }

    #[test]
    fn test_each_character_of_a_run_takes_one_position() {
        assert_eq!(
            tokens("園no件"),
            [("園".to_owned(), 0), ("no".to_owned(), 1), ("件".to_owned(), 2)]
        );
    }

    #[test]
    fn test_text_is_nfkc_normalised() {
        assert_eq!(texts("ｽｲ"), ["ス", "スイ", "イ"]);
        assert_eq!(texts("ＭＡＴＲＩＸ"), ["matrix"]);
    }

    #[test]
    fn test_punctuation_ends_runs_but_variation_selectors_do_not() {
        assert_eq!(texts("「駅」・で"), ["駅", "で"]);
        assert_eq!(texts("ไป๏มา"), ["ไ", "ไป", "ป", "ม", "มา", "า"]);
        assert_eq!(texts("葛\u{E0100}飾"), ["葛", "葛飾", "飾"]);
    }
}
