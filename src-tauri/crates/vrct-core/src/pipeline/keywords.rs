//! The word filter: `flashtext.KeywordProcessor` as `Model.checkKeywords` uses it.
//!
//! flashtext is not a plain substring search. It lower-cases both sides, and it only matches where a word
//! ends, and "a word" is made of ASCII letters, digits and `_` alone: every other character, a Japanese one
//! included, is a boundary. [`KeywordFilter::extract`] follows its `extract_keywords` step by step (the
//! quirks too), and `tests/pipeline.rs` compares it with the real library on random keyword lists.

use std::collections::HashMap;

#[derive(Default)]
struct Node {
    next: HashMap<char, Node>,
    /// Set where a keyword ends: the keyword as it was added (flashtext's "clean name").
    keyword: Option<String>,
}

#[derive(Default)]
pub struct KeywordFilter {
    root: Node,
}

/// `string.digits + string.ascii_letters + '_'`.
fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

impl KeywordFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// `add_keyword`: an empty keyword is ignored, adding the same one again replaces it.
    pub fn add(&mut self, keyword: &str) {
        if keyword.is_empty() {
            return;
        }
        let mut node = &mut self.root;
        for c in keyword.to_lowercase().chars() {
            node = node.next.entry(c).or_default();
        }
        node.keyword = Some(keyword.to_string());
    }

    pub fn is_empty(&self) -> bool {
        self.root.next.is_empty()
    }

    /// `checkKeywords`: whether the message holds any keyword.
    pub fn matches(&self, sentence: &str) -> bool {
        !self.extract(sentence).is_empty()
    }

    /// `extract_keywords`: the keywords found, in order, as they were added.
    pub fn extract(&self, sentence: &str) -> Vec<String> {
        let mut found = Vec::new();
        if sentence.is_empty() {
            return found;
        }
        let chars: Vec<char> = sentence.to_lowercase().chars().collect();
        let len = chars.len();
        let mut current = &self.root;
        let mut idx = 0;
        while idx < len {
            let c = chars[idx];
            if !is_word_char(c) {
                // A character that can end a word.
                if current.keyword.is_some() || current.next.contains_key(&c) {
                    let mut longest: Option<&String> = None;
                    let mut longer = false;
                    let mut end = idx;
                    if let Some(keyword) = &current.keyword {
                        longest = Some(keyword);
                    }
                    if let Some(first) = current.next.get(&c) {
                        let mut inner = first;
                        let mut idy = idx + 1;
                        let mut broke = false;
                        while idy < len {
                            let d = chars[idy];
                            if !is_word_char(d) {
                                if let Some(keyword) = &inner.keyword {
                                    longest = Some(keyword);
                                    end = idy;
                                    longer = true;
                                }
                            }
                            match inner.next.get(&d) {
                                Some(node) => inner = node,
                                None => {
                                    broke = true;
                                    break;
                                }
                            }
                            idy += 1;
                        }
                        if !broke {
                            // The sentence ended inside the keyword.
                            if let Some(keyword) = &inner.keyword {
                                longest = Some(keyword);
                                end = idy;
                                longer = true;
                            }
                        }
                        if longer {
                            idx = end;
                        }
                    }
                    current = &self.root;
                    if let Some(keyword) = longest {
                        found.push(keyword.clone());
                    }
                } else {
                    current = &self.root;
                }
            } else if let Some(node) = current.next.get(&c) {
                current = node;
            } else {
                // Not a keyword: skip to the end of this word.
                current = &self.root;
                let mut idy = idx + 1;
                while idy < len && is_word_char(chars[idy]) {
                    idy += 1;
                }
                idx = idy;
            }
            // The sentence ends here with a keyword complete.
            if idx + 1 >= len {
                if let Some(keyword) = &current.keyword {
                    found.push(keyword.clone());
                }
            }
            idx += 1;
        }
        found
    }
}
