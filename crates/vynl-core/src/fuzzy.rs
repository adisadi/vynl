//! Fast, zero-dependency fuzzy matcher for instant local library search and
//! Command Palette filtering (mirrors `nucleo` semantics offline).

/// Scored fuzzy match result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    /// Match relevance score (higher is better).
    pub score: i32,
    /// Matched character indices inside the candidate string.
    pub indices: Vec<usize>,
}

/// Evaluates whether `query` fuzzy-matches `candidate` (ASCII/Unicode case-insensitive).
///
/// Rewards consecutive character runs, word-boundary starts (` `, `-`, `_`, `/`),
/// and prefix hits while penalizing long gaps.
#[must_use]
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<FuzzyMatch> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            indices: Vec::new(),
        });
    }

    let q_chars: Vec<char> = trimmed.to_lowercase().chars().collect();
    let c_chars: Vec<char> = candidate.to_lowercase().chars().collect();
    let orig_chars: Vec<char> = candidate.chars().collect();

    let mut indices = Vec::with_capacity(q_chars.len());
    let mut score: i32 = 0;
    let mut c_idx = 0usize;
    let mut prev_match: Option<usize> = None;

    for &qc in &q_chars {
        let mut found = None;
        while c_idx < c_chars.len() {
            if c_chars[c_idx] == qc {
                found = Some(c_idx);
                c_idx += 1;
                break;
            }
            c_idx += 1;
        }

        let pos = found?;
        indices.push(pos);

        // Base match reward
        score += 10;

        // Consecutive match bonus
        if let Some(prev) = prev_match {
            if pos == prev + 1 {
                score += 15;
            } else {
                let gap = i32::try_from(pos - prev - 1).unwrap_or(32);
                score -= gap.min(12);
            }
        } else if pos == 0 {
            // Prefix bonus
            score += 20;
        }

        // Word-boundary bonus
        if pos == 0
            || orig_chars
                .get(pos.saturating_sub(1))
                .is_some_and(|ch| matches!(ch, ' ' | '-' | '_' | '/' | '('))
        {
            score += 12;
        }

        prev_match = Some(pos);
    }

    Some(FuzzyMatch { score, indices })
}

/// Filters and ranks `items` by fuzzy score descending using `key_fn`.
#[must_use]
pub fn rank_items<'a, T, F>(query: &str, items: &'a [T], key_fn: F) -> Vec<(usize, i32)>
where
    F: Fn(&'a T) -> String,
{
    let mut ranked: Vec<(usize, i32)> = items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            let key = key_fn(item);
            fuzzy_match(query, &key).map(|m| (idx, m.score))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

#[cfg(test)]
mod tests {
    use super::{fuzzy_match, rank_items};

    #[test]
    fn prefers_prefix_and_word_boundary_matches() {
        let items = [
            "The Game of Love",
            "Instant Crush",
            "Lose Yourself to Dance",
            "Give Life Back to Music",
        ];
        let ranked = rank_items("ic", &items, |s| (*s).to_owned());
        assert_ne!(ranked, []);
        assert_eq!(ranked[0].0, 1); // "Instant Crush" word boundaries I + C
    }

    #[test]
    fn returns_none_when_subsequence_missing() {
        assert_eq!(fuzzy_match("xyz", "Random Access Memories"), None);
    }
}
