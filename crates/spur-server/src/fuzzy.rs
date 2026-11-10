//! Fuzzy matching for the Ask list. Runs in the server, so no extension code is on the
//! keystroke path.

use spur_rpc::Span;

/// Matches `query` as a case-insensitive subsequence of `text`. Returns a score (higher
/// is better) and the matched char indices. An empty query matches with score 0.
pub(crate) fn fuzzy(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let q: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = chars
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let boundary = |i: usize| {
        i == 0
            || !chars[i - 1].is_alphanumeric()
            || (chars[i].is_uppercase() && chars[i - 1].is_lowercase())
    };
    // First try to land every char on a word start or right after the previous match;
    // fall back to the leftmost subsequence.
    let strict = |prefer: bool| -> Option<Vec<usize>> {
        let mut at = 0;
        let mut found = Vec::with_capacity(q.len());
        for &qc in &q {
            let prev: Option<usize> = found.last().copied();
            let good = (at..lower.len())
                .find(|&i| lower[i] == qc && (boundary(i) || prev.is_some_and(|p| p + 1 == i)));
            let any = || (at..lower.len()).find(|&i| lower[i] == qc);
            let i = if prefer { good.or_else(any) } else { any() }?;
            found.push(i);
            at = i + 1;
        }
        Some(found)
    };
    let found = strict(true).or_else(|| strict(false))?;
    let mut score = 0i32;
    for (n, &i) in found.iter().enumerate() {
        score += 10;
        if boundary(i) {
            score += 8;
        }
        if n > 0 && found[n - 1] + 1 == i {
            score += 12;
        }
    }
    let len = i32::try_from(chars.len()).unwrap_or(i32::MAX);
    let first = i32::try_from(found[0]).unwrap_or(i32::MAX);
    Some((score - len.min(60) / 4 - first.min(40), found))
}

/// Splits `text` into spans, tagging the chars at `matched` with the `match` token.
pub(crate) fn mark(text: &str, matched: &[usize]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for (i, c) in text.chars().enumerate() {
        let style = matched.contains(&i).then_some("match");
        match out.last_mut() {
            Some(last) if last.style.as_deref() == style => last.text.push(c),
            _ => out.push(Span {
                text: c.to_string(),
                style: style.map(str::to_owned),
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_and_ranking() {
        assert!(fuzzy("xyz", "save file").is_none());
        assert_eq!(fuzzy("", "anything"), Some((0, vec![])));
        let (word_start, at) = fuzzy("sf", "Save file").unwrap();
        assert_eq!(at, vec![0, 5]);
        let (scattered, _) = fuzzy("sf", "transform").unwrap();
        assert!(word_start > scattered);
        let (exact, _) = fuzzy("undo", "Undo").unwrap();
        let (longer, _) = fuzzy("undo", "Redo the last undone change").unwrap();
        assert!(exact > longer);
        // Falls back when the word-start guess dead-ends.
        assert!(fuzzy("ab", "a_xa_b").is_some());
        assert!(fuzzy("É", "é").is_some());
    }

    #[test]
    fn marks_matches() {
        let spans = mark("Save", &[0, 2]);
        let got: Vec<_> = spans
            .iter()
            .map(|s| (&*s.text, s.style.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("S", Some("match")),
                ("a", None),
                ("v", Some("match")),
                ("e", None)
            ]
        );
    }
}
