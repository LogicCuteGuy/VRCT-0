use unicode_normalization::UnicodeNormalization;
/// Match Python NFKC followed by removal of Python str.isspace characters.
pub fn normalize_transcript(text: &str) -> String {
    text.nfkc()
        .filter(|c| !c.is_whitespace() && !('\u{1c}'..='\u{1f}').contains(c))
        .collect()
}
pub fn edit_distance(reference: &str, hypothesis: &str) -> usize {
    let a: Vec<char> = reference.chars().collect();
    let b: Vec<char> = hypothesis.chars().collect();
    // Memory is linear in the shorter string, including arbitrary Unicode.
    let (a, b) = if a.len() < b.len() { (b, a) } else { (a, b) };
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, x) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            current[j + 1] = (previous[j] + usize::from(x != y))
                .min(current[j] + 1)
                .min(previous[j + 1] + 1);
        }
        std::mem::swap(&mut current, &mut previous);
    }
    previous[b.len()]
}
pub fn character_error_rate(reference: &str, hypothesis: &str) -> f64 {
    let reference = normalize_transcript(reference);
    let hypothesis = normalize_transcript(hypothesis);
    if reference.is_empty() {
        return if hypothesis.is_empty() { 0.0 } else { 1.0 };
    }
    edit_distance(&reference, &hypothesis) as f64 / reference.chars().count() as f64
}
