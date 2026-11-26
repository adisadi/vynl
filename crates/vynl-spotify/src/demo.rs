//! Rich, realistic offline demo catalog powering `vynl --demo` and offline UI
//! testing without requiring Spotify credentials or network access.

use std::collections::BTreeMap;

use vynl_core::fuzzy::rank_items;
use vynl_core::model::{
    AlbumSummary, ArtistSummary, ConnectDevice, EpisodeSummary, ImageRef, LibraryState,
    PlaybackState, PlaylistSummary, RepeatMode, SearchResults, TrackSummary, UserProfile,
};

/// Complete offline dataset with iconic albums, tracks, playlists, artists,
/// podcast episodes, Spotify Connect devices, and instant fuzzy search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoCatalog {
    /// Demo Spotify user profile.
    pub user: UserProfile,
    /// Curated iconic albums (`6+`).
    pub albums: Vec<AlbumSummary>,
    /// Curated tracks (`15+`) with `vynl://demo/<id>` cover URLs.
    pub tracks: Vec<TrackSummary>,
    /// Curated playlists (`5`).
    pub playlists: Vec<PlaylistSummary>,
    /// Track ID membership per playlist ID.
    pub playlist_track_ids: BTreeMap<String, Vec<String>>,
    /// Featured artists (`4+`).
    pub artists: Vec<ArtistSummary>,
    /// Podcast episodes (`3+`).
    pub episodes: Vec<EpisodeSummary>,
    /// Simulated Spotify Connect devices (`3`).
    pub devices: Vec<ConnectDevice>,
    /// Recently played track history.
    pub recently_played: Vec<TrackSummary>,
}

impl Default for DemoCatalog {
    fn default() -> Self {
        Self::builtin()
    }
}

impl DemoCatalog {
    /// Constructs the built-in offline demo catalog.
    #[must_use]
    pub fn builtin() -> Self {
        let user = UserProfile {
            id: String::from("vynl_demo_listener"),
            display_name: String::from("Vinyl Explorer"),
            product: String::from("premium"),
            avatar: Some(demo_image("avatar-listener")),
        };

        let albums = build_demo_albums();
        let tracks = build_demo_tracks();
        let playlists = build_demo_playlists();
        let playlist_track_ids = build_demo_playlist_tracks();
        let artists = build_demo_artists();
        let episodes = build_demo_episodes();
        let devices = build_demo_devices();
        let recently_played = tracks.iter().take(6).cloned().collect();

        Self {
            user,
            albums,
            tracks,
            playlists,
            playlist_track_ids,
            artists,
            episodes,
            devices,
            recently_played,
        }
    }

    /// Converts the demo catalog into a [`LibraryState`] ready for `vynl-core`.
    #[must_use]
    pub fn to_library_state(&self) -> LibraryState {
        LibraryState {
            liked_tracks: self.tracks.iter().filter(|t| t.is_liked).cloned().collect(),
            playlists: self.playlists.clone(),
            albums: self.albums.clone(),
            artists: self.artists.clone(),
            episodes: self.episodes.clone(),
        }
    }

    /// Returns a realistic initial [`PlaybackState`] for demo mode.
    #[must_use]
    pub fn initial_playback_state(&self) -> PlaybackState {
        PlaybackState {
            current_track: self.tracks.first().cloned(),
            is_playing: true,
            position_ms: 84_000,
            shuffle: false,
            repeat: RepeatMode::Context,
            volume_percent: 78,
            muted: false,
            speed_percent: 100,
            active_device: self.devices.first().cloned(),
        }
    }

    /// Resolves the ordered tracks belonging to `playlist_id`.
    #[must_use]
    pub fn tracks_for_playlist(&self, playlist_id: &str) -> Vec<TrackSummary> {
        let Some(ids) = self.playlist_track_ids.get(playlist_id) else {
            return Vec::new();
        };
        ids.iter()
            .filter_map(|id| self.tracks.iter().find(|t| t.id == *id).cloned())
            .collect()
    }

    /// Resolves all demo tracks belonging to `album_title`.
    #[must_use]
    pub fn tracks_for_album(&self, album_title: &str) -> Vec<TrackSummary> {
        self.tracks
            .iter()
            .filter(|t| t.album.eq_ignore_ascii_case(album_title))
            .cloned()
            .collect()
    }

    /// Performs instant offline fuzzy search across tracks, albums, artists,
    /// playlists, and podcast episodes using [`vynl_core::fuzzy::rank_items`].
    #[must_use]
    pub fn search(&self, query: &str) -> SearchResults {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return SearchResults {
                query: String::new(),
                tracks: self.tracks.clone(),
                albums: self.albums.clone(),
                artists: self.artists.clone(),
                playlists: self.playlists.clone(),
                episodes: self.episodes.clone(),
            };
        }

        let tracks = rank_items(trimmed, &self.tracks, |t| {
            format!("{} {} {}", t.title, t.artist_line(), t.album)
        })
        .into_iter()
        .map(|(idx, _)| self.tracks[idx].clone())
        .collect();

        let albums = rank_items(trimmed, &self.albums, |a| {
            format!("{} {} {}", a.title, a.artists.join(" "), a.release_year)
        })
        .into_iter()
        .map(|(idx, _)| self.albums[idx].clone())
        .collect();

        let artists = rank_items(trimmed, &self.artists, |a| {
            format!("{} {}", a.name, a.genres.join(" "))
        })
        .into_iter()
        .map(|(idx, _)| self.artists[idx].clone())
        .collect();

        let playlists = rank_items(trimmed, &self.playlists, |p| {
            format!("{} {} {}", p.name, p.description, p.owner)
        })
        .into_iter()
        .map(|(idx, _)| self.playlists[idx].clone())
        .collect();

        let episodes = rank_items(trimmed, &self.episodes, |e| {
            format!("{} {}", e.title, e.show_name)
        })
        .into_iter()
        .map(|(idx, _)| self.episodes[idx].clone())
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
}

fn demo_image(id: &str) -> ImageRef {
    ImageRef {
        url: format!("vynl://demo/{id}"),
        width: Some(640),
        height: Some(640),
    }
}

fn build_demo_albums() -> Vec<AlbumSummary> {
    vec![
        AlbumSummary {
            id: String::from("alb_ram"),
            title: String::from("Random Access Memories"),
            artists: vec![String::from("Daft Punk")],
            release_year: String::from("2013"),
            album_type: String::from("album"),
            total_tracks: 13,
            cover: Some(demo_image("ram")),
        },
        AlbumSummary {
            id: String::from("alb_dsotm"),
            title: String::from("The Dark Side of the Moon"),
            artists: vec![String::from("Pink Floyd")],
            release_year: String::from("1973"),
            album_type: String::from("album"),
            total_tracks: 10,
            cover: Some(demo_image("dsotm")),
        },
        AlbumSummary {
            id: String::from("alb_kob"),
            title: String::from("Kind of Blue"),
            artists: vec![String::from("Miles Davis")],
            release_year: String::from("1959"),
            album_type: String::from("album"),
            total_tracks: 5,
            cover: Some(demo_image("kind-of-blue")),
        },
        AlbumSummary {
            id: String::from("alb_in_rainbows"),
            title: String::from("In Rainbows"),
            artists: vec![String::from("Radiohead")],
            release_year: String::from("2007"),
            album_type: String::from("album"),
            total_tracks: 10,
            cover: Some(demo_image("in-rainbows")),
        },
        AlbumSummary {
            id: String::from("alb_rumours"),
            title: String::from("Rumours"),
            artists: vec![String::from("Fleetwood Mac")],
            release_year: String::from("1977"),
            album_type: String::from("album"),
            total_tracks: 11,
            cover: Some(demo_image("rumours")),
        },
        AlbumSummary {
            id: String::from("alb_currents"),
            title: String::from("Currents"),
            artists: vec![String::from("Tame Impala")],
            release_year: String::from("2015"),
            album_type: String::from("album"),
            total_tracks: 13,
            cover: Some(demo_image("currents")),
        },
        AlbumSummary {
            id: String::from("alb_blonde"),
            title: String::from("Blonde"),
            artists: vec![String::from("Frank Ocean")],
            release_year: String::from("2016"),
            album_type: String::from("album"),
            total_tracks: 17,
            cover: Some(demo_image("blonde")),
        },
    ]
}

type TrackSeed = (
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static str,
    u64,
    bool,
    bool,
    &'static str,
);

const DEMO_TRACK_SEEDS: &[TrackSeed] = &[
    (
        "trk_01",
        "Instant Crush",
        &["Daft Punk", "Julian Casablancas"],
        "Random Access Memories",
        337_560,
        false,
        true,
        "ram",
    ),
    (
        "trk_02",
        "Giorgio by Moroder",
        &["Daft Punk"],
        "Random Access Memories",
        544_626,
        false,
        true,
        "ram",
    ),
    (
        "trk_03",
        "Give Life Back to Music",
        &["Daft Punk"],
        "Random Access Memories",
        274_160,
        false,
        true,
        "ram",
    ),
    (
        "trk_04",
        "Time",
        &["Pink Floyd"],
        "The Dark Side of the Moon",
        413_947,
        false,
        true,
        "dsotm",
    ),
    (
        "trk_05",
        "Breathe (In the Air)",
        &["Pink Floyd"],
        "The Dark Side of the Moon",
        169_534,
        false,
        true,
        "dsotm",
    ),
    (
        "trk_06",
        "Us and Them",
        &["Pink Floyd"],
        "The Dark Side of the Moon",
        469_000,
        false,
        false,
        "dsotm",
    ),
    (
        "trk_07",
        "So What",
        &["Miles Davis"],
        "Kind of Blue",
        562_506,
        false,
        true,
        "kind-of-blue",
    ),
    (
        "trk_08",
        "Blue in Green",
        &["Miles Davis", "Bill Evans"],
        "Kind of Blue",
        327_533,
        false,
        true,
        "kind-of-blue",
    ),
    (
        "trk_09",
        "All Blues",
        &["Miles Davis"],
        "Kind of Blue",
        693_240,
        false,
        false,
        "kind-of-blue",
    ),
    (
        "trk_10",
        "Weird Fishes / Arpeggi",
        &["Radiohead"],
        "In Rainbows",
        318_186,
        false,
        true,
        "in-rainbows",
    ),
    (
        "trk_11",
        "Reckoner",
        &["Radiohead"],
        "In Rainbows",
        290_213,
        false,
        true,
        "in-rainbows",
    ),
    (
        "trk_12",
        "Nude",
        &["Radiohead"],
        "In Rainbows",
        255_386,
        false,
        false,
        "in-rainbows",
    ),
    (
        "trk_13",
        "Dreams",
        &["Fleetwood Mac"],
        "Rumours",
        257_800,
        false,
        true,
        "rumours",
    ),
    (
        "trk_14",
        "The Chain",
        &["Fleetwood Mac"],
        "Rumours",
        270_213,
        false,
        true,
        "rumours",
    ),
    (
        "trk_15",
        "Let It Happen",
        &["Tame Impala"],
        "Currents",
        467_586,
        false,
        true,
        "currents",
    ),
    (
        "trk_16",
        "The Less I Know the Better",
        &["Tame Impala"],
        "Currents",
        216_320,
        true,
        true,
        "currents",
    ),
    (
        "trk_17",
        "Nights",
        &["Frank Ocean"],
        "Blonde",
        307_151,
        true,
        true,
        "blonde",
    ),
    (
        "trk_18",
        "Pink + White",
        &["Frank Ocean"],
        "Blonde",
        184_516,
        false,
        true,
        "blonde",
    ),
];

fn build_demo_tracks() -> Vec<TrackSummary> {
    DEMO_TRACK_SEEDS
        .iter()
        .map(
            |&(id, title, artists, album, duration_ms, explicit, is_liked, cover_id)| {
                TrackSummary {
                    id: id.to_owned(),
                    title: title.to_owned(),
                    artists: artists.iter().map(|a| (*a).to_owned()).collect(),
                    album: album.to_owned(),
                    duration_ms,
                    explicit,
                    is_liked,
                    cover: Some(demo_image(cover_id)),
                }
            },
        )
        .collect()
}

fn build_demo_playlists() -> Vec<PlaylistSummary> {
    vec![
        PlaylistSummary {
            id: String::from("pl_late_night_coding"),
            name: String::from("Late Night Rust & Synth"),
            description: String::from("Warm analog synths and deep grooves for terminal sessions."),
            owner: String::from("Vinyl Explorer"),
            track_count: 6,
            snapshot_id: String::from("demo-snap-001"),
            pinned: true,
            folder: Some(String::from("Focus")),
            cover: Some(demo_image("pl-synth")),
        },
        PlaylistSummary {
            id: String::from("pl_midnight_jazz"),
            name: String::from("Midnight Vinyl Jazz"),
            description: String::from("Modal jazz essentials recorded straight to tape."),
            owner: String::from("Vinyl Explorer"),
            track_count: 4,
            snapshot_id: String::from("demo-snap-002"),
            pinned: true,
            folder: Some(String::from("Focus")),
            cover: Some(demo_image("pl-jazz")),
        },
        PlaylistSummary {
            id: String::from("pl_classic_wax"),
            name: String::from("Classic Wax Pressing"),
            description: String::from("Timeless 1970s studio masterpieces on 180g vinyl."),
            owner: String::from("Vinyl Explorer"),
            track_count: 5,
            snapshot_id: String::from("demo-snap-003"),
            pinned: false,
            folder: Some(String::from("Classics")),
            cover: Some(demo_image("pl-classic")),
        },
        PlaylistSummary {
            id: String::from("pl_dream_indie"),
            name: String::from("Neon & Arpeggi"),
            description: String::from("Layered guitars, shimmering synths, and late-evening R&B."),
            owner: String::from("Vinyl Explorer"),
            track_count: 5,
            snapshot_id: String::from("demo-snap-004"),
            pinned: false,
            folder: None,
            cover: Some(demo_image("pl-indie")),
        },
        PlaylistSummary {
            id: String::from("pl_audiophile_test"),
            name: String::from("Audiophile Reference"),
            description: String::from(
                "Dynamic range benchmarks for headphones and studio monitors.",
            ),
            owner: String::from("vynl Curated"),
            track_count: 6,
            snapshot_id: String::from("demo-snap-005"),
            pinned: false,
            folder: Some(String::from("Hi-Fi")),
            cover: Some(demo_image("pl-hifi")),
        },
    ]
}

fn build_demo_playlist_tracks() -> BTreeMap<String, Vec<String>> {
    let mut map = BTreeMap::new();
    map.insert(
        String::from("pl_late_night_coding"),
        vec![
            String::from("trk_01"),
            String::from("trk_02"),
            String::from("trk_10"),
            String::from("trk_15"),
            String::from("trk_16"),
            String::from("trk_17"),
        ],
    );
    map.insert(
        String::from("pl_midnight_jazz"),
        vec![
            String::from("trk_07"),
            String::from("trk_08"),
            String::from("trk_09"),
            String::from("trk_06"),
        ],
    );
    map.insert(
        String::from("pl_classic_wax"),
        vec![
            String::from("trk_04"),
            String::from("trk_05"),
            String::from("trk_06"),
            String::from("trk_13"),
            String::from("trk_14"),
        ],
    );
    map.insert(
        String::from("pl_dream_indie"),
        vec![
            String::from("trk_10"),
            String::from("trk_11"),
            String::from("trk_12"),
            String::from("trk_17"),
            String::from("trk_18"),
        ],
    );
    map.insert(
        String::from("pl_audiophile_test"),
        vec![
            String::from("trk_02"),
            String::from("trk_03"),
            String::from("trk_04"),
            String::from("trk_07"),
            String::from("trk_11"),
            String::from("trk_15"),
        ],
    );
    map
}

fn build_demo_artists() -> Vec<ArtistSummary> {
    vec![
        ArtistSummary {
            id: String::from("art_daft_punk"),
            name: String::from("Daft Punk"),
            followers: 10_420_000,
            genres: vec![String::from("french house"), String::from("electronic")],
            following: true,
            avatar: Some(demo_image("artist-daft-punk")),
        },
        ArtistSummary {
            id: String::from("art_pink_floyd"),
            name: String::from("Pink Floyd"),
            followers: 21_850_000,
            genres: vec![
                String::from("progressive rock"),
                String::from("psychedelic rock"),
            ],
            following: true,
            avatar: Some(demo_image("artist-pink-floyd")),
        },
        ArtistSummary {
            id: String::from("art_miles_davis"),
            name: String::from("Miles Davis"),
            followers: 4_310_000,
            genres: vec![String::from("modal jazz"), String::from("cool jazz")],
            following: true,
            avatar: Some(demo_image("artist-miles-davis")),
        },
        ArtistSummary {
            id: String::from("art_radiohead"),
            name: String::from("Radiohead"),
            followers: 11_940_000,
            genres: vec![String::from("art rock"), String::from("alternative rock")],
            following: true,
            avatar: Some(demo_image("artist-radiohead")),
        },
        ArtistSummary {
            id: String::from("art_tame_impala"),
            name: String::from("Tame Impala"),
            followers: 8_760_000,
            genres: vec![
                String::from("psychedelic pop"),
                String::from("neo-psychedelia"),
            ],
            following: false,
            avatar: Some(demo_image("artist-tame-impala")),
        },
    ]
}

fn build_demo_episodes() -> Vec<EpisodeSummary> {
    vec![
        EpisodeSummary {
            id: String::from("ep_rust_audio"),
            title: String::from("Lock-Free Ring Buffers in Real-Time Audio"),
            show_name: String::from("Systems in Stereo"),
            duration_ms: 2_845_000,
            resume_position_ms: 1_120_000,
            fully_played: false,
            cover: Some(demo_image("ep-systems")),
        },
        EpisodeSummary {
            id: String::from("ep_analog_tape"),
            title: String::from("Inside 1973 Abbey Road Mastering Consoles"),
            show_name: String::from("Tape & Transistors"),
            duration_ms: 3_720_000,
            resume_position_ms: 480_000,
            fully_played: false,
            cover: Some(demo_image("ep-tape")),
        },
        EpisodeSummary {
            id: String::from("ep_terminal_graphics"),
            title: String::from("Kitty Graphics, Sixel, and 60 FPS TUIs"),
            show_name: String::from("Terminal Craft"),
            duration_ms: 2_190_000,
            resume_position_ms: 2_190_000,
            fully_played: true,
            cover: Some(demo_image("ep-terminal")),
        },
    ]
}

fn build_demo_devices() -> Vec<ConnectDevice> {
    vec![
        ConnectDevice {
            id: String::from("dev_vynl_local"),
            name: String::from("vynl (Integrated)"),
            device_type: String::from("Computer"),
            is_active: true,
            volume_percent: 78,
        },
        ConnectDevice {
            id: String::from("dev_studio_monitors"),
            name: String::from("Living Room Hi-Fi"),
            device_type: String::from("Speaker"),
            is_active: false,
            volume_percent: 62,
        },
        ConnectDevice {
            id: String::from("dev_phone"),
            name: String::from("Pocket Player"),
            device_type: String::from("Smartphone"),
            is_active: false,
            volume_percent: 50,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::DemoCatalog;

    #[test]
    fn demo_catalog_has_rich_dataset_and_demo_cover_urls() {
        let catalog = DemoCatalog::builtin();
        assert!(catalog.albums.len() >= 6);
        assert!(catalog.tracks.len() >= 15);
        assert_eq!(catalog.playlists.len(), 5);
        assert!(catalog.artists.len() >= 4);
        assert!(catalog.episodes.len() >= 3);
        assert_eq!(catalog.devices.len(), 3);

        for track in &catalog.tracks {
            let url = track.cover.as_ref().map_or("", |c| c.url.as_str());
            assert!(url.starts_with("vynl://demo/"));
        }

        let pl_tracks = catalog.tracks_for_playlist("pl_late_night_coding");
        assert_eq!(pl_tracks.len(), 6);
    }

    #[test]
    fn demo_catalog_fuzzy_search_ranks_matching_entities() {
        let catalog = DemoCatalog::builtin();
        let results = catalog.search("daft");
        assert_ne!(results.tracks, []);
        assert_ne!(results.albums, []);
        assert_ne!(results.artists, []);
        assert_eq!(results.artists[0].name, "Daft Punk");

        let podcast_results = catalog.search("ring buffer");
        assert_eq!(podcast_results.episodes.len(), 1);
        assert!(podcast_results.episodes[0].title.contains("Ring Buffers"));
    }
}
