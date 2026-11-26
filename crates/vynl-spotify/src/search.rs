//! `nucleo`-powered fuzzy search and ranking over local Spotify library state.

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};
use vynl_core::model::{LibraryState, SearchResults};

/// Ranks `items` against `query` using `nucleo` fuzzy matching and returns matching
/// `(original_index, score)` pairs sorted by descending score, then ascending index
/// for deterministic tie-breaking.
///
/// An empty or whitespace-only `query` returns all indices in their original order
/// with a score of `0`.
#[must_use]
pub fn fuzzy_rank<T, F>(query: &str, items: &[T], mut key_fn: F) -> Vec<(usize, u32)>
where
    F: FnMut(&T) -> String,
{
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return (0..items.len()).map(|idx| (idx, 0)).collect();
    }

    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(trimmed, CaseMatching::Ignore, Normalization::Smart);
    let mut utf32_buf = Vec::new();
    let mut ranked = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        let key = key_fn(item);
        utf32_buf.clear();
        let haystack = Utf32Str::new(&key, &mut utf32_buf);
        if let Some(score) = pattern.score(haystack, &mut matcher) {
            ranked.push((idx, score));
        }
    }

    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

/// Performs instant local fuzzy search across a cached [`LibraryState`] (`liked_tracks`,
/// `albums`, `artists`, `playlists`, and `episodes`) powered by `nucleo`.
#[must_use]
pub fn search_library(query: &str, library: &LibraryState) -> SearchResults {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return SearchResults {
            query: String::new(),
            tracks: library.liked_tracks.clone(),
            albums: library.albums.clone(),
            artists: library.artists.clone(),
            playlists: library.playlists.clone(),
            episodes: library.episodes.clone(),
        };
    }

    let tracks = fuzzy_rank(trimmed, &library.liked_tracks, |t| {
        format!("{} {} {}", t.title, t.artist_line(), t.album)
    })
    .into_iter()
    .map(|(idx, _)| library.liked_tracks[idx].clone())
    .collect();

    let albums = fuzzy_rank(trimmed, &library.albums, |a| {
        format!("{} {} {}", a.title, a.artists.join(" "), a.release_year)
    })
    .into_iter()
    .map(|(idx, _)| library.albums[idx].clone())
    .collect();

    let artists = fuzzy_rank(trimmed, &library.artists, |a| {
        format!("{} {}", a.name, a.genres.join(" "))
    })
    .into_iter()
    .map(|(idx, _)| library.artists[idx].clone())
    .collect();

    let playlists = fuzzy_rank(trimmed, &library.playlists, |p| {
        format!("{} {} {}", p.name, p.description, p.owner)
    })
    .into_iter()
    .map(|(idx, _)| library.playlists[idx].clone())
    .collect();

    let episodes = fuzzy_rank(trimmed, &library.episodes, |e| {
        format!("{} {}", e.title, e.show_name)
    })
    .into_iter()
    .map(|(idx, _)| library.episodes[idx].clone())
    .collect();

    SearchResults {
        query: trimmed.to_owned(),
        tracks,
        albums,
        artists,
        playlists,
        episodes,
    }
}

#[cfg(test)]
mod tests {
    use crate::demo::DemoCatalog;

    use super::{fuzzy_rank, search_library};

    #[test]
    fn fuzzy_rank_prefers_exact_and_prefix_matches_deterministically() {
        let items = [
            "The Game of Love",
            "Instant Crush",
            "Lose Yourself to Dance",
            "Give Life Back to Music",
        ];

        let ranked = fuzzy_rank("instant", &items, |s| (*s).to_owned());
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0, 1);
        assert!(ranked[0].1 > 0);

        let all = fuzzy_rank("   ", &items, |s| (*s).to_owned());
        assert_eq!(all, vec![(0, 0), (1, 0), (2, 0), (3, 0)]);

        let none = fuzzy_rank("zzzznotfound", &items, |s| (*s).to_owned());
        assert_eq!(none, []);
    }

    #[test]
    fn fuzzy_rank_handles_case_insensitivity_and_tie_breaking() {
        let items = ["Alpha", "alpha", "ALPHA"];
        let ranked = fuzzy_rank("alpha", &items, |s| (*s).to_owned());
        assert_eq!(ranked.len(), 3);
        assert_eq!(ranked[0].0, 0);
        assert_eq!(ranked[1].0, 1);
        assert_eq!(ranked[2].0, 2);
    }

    #[test]
    fn search_library_searches_all_five_collections() {
        let catalog = DemoCatalog::builtin();
        let library = catalog.to_library_state();

        let empty = search_library("  ", &library);
        assert_eq!(empty.query, "");
        assert_eq!(empty.tracks.len(), library.liked_tracks.len());
        assert_eq!(empty.albums.len(), library.albums.len());
        assert_eq!(empty.artists.len(), library.artists.len());
        assert_eq!(empty.playlists.len(), library.playlists.len());
        assert_eq!(empty.episodes.len(), library.episodes.len());

        let daft = search_library("Daft", &library);
        assert_eq!(daft.query, "Daft");
        assert_ne!(daft.tracks, []);
        assert_ne!(daft.albums, []);
        assert_eq!(daft.artists.len(), 1);
        assert_eq!(daft.artists[0].name, "Daft Punk");

        let jazz = search_library("jazz", &library);
        assert_ne!(jazz.artists, []);
        assert_ne!(jazz.playlists, []);
        assert_eq!(jazz.playlists[0].id, "pl_midnight_jazz");

        let podcast = search_library("ring buffer", &library);
        assert_eq!(podcast.episodes.len(), 1);
        assert!(podcast.episodes[0].title.contains("Ring Buffers"));
    }
}
