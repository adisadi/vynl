//! Full desktop-parity TUI frame renderer with terminal cover art,
//! **Modal-Aware Image Occlusion**, adaptive cover accents, and responsive breakpoints.

use std::collections::HashSet;

use vynl_core::fuzzy::rank_items;
use vynl_core::model::{SearchCategory, TrackSummary, format_duration_ms};
use vynl_core::state::{
    ActiveModal, AppState, LayoutBreakpoint, PanelFocus, RightSidebarTab, Route,
};
use vynl_core::theme::{Rgb, Theme};
use vynl_images::{
    CellRect, EncodedImage, ImageCacheKey, ImagePriority, ImageProtocol, ImageRequest, ImageWorker,
    L1ImageCache, OcclusionTracker, encode_for_protocol, generate_demo_cover,
};

use crate::buffer::{Buffer, CellStyle, Rect};
use crate::layout::FrameLayout;

/// Built-in Command Palette entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteCommand {
    /// Unique action identifier.
    pub id: &'static str,
    /// Human-readable English label shown in the Command Palette.
    pub label: &'static str,
    /// Shortcut hint displayed on the right.
    pub shortcut: &'static str,
}

/// Owned Command Palette entry combining built-ins and dynamically discovered user themes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicPaletteEntry {
    /// Unique action identifier (custom themes use `theme_custom:<name>`).
    pub id: String,
    /// Human-readable English label shown in the Command Palette.
    pub label: String,
    /// Shortcut hint displayed on the right.
    pub shortcut: String,
}

/// Complete list of searchable Command Palette actions.
pub const PALETTE_COMMANDS: &[PaletteCommand] = &[
    PaletteCommand {
        id: "play_pause",
        label: "Playback: Toggle Play / Pause",
        shortcut: "Space",
    },
    PaletteCommand {
        id: "next_track",
        label: "Playback: Next Track",
        shortcut: "n",
    },
    PaletteCommand {
        id: "prev_track",
        label: "Playback: Previous Track",
        shortcut: "p",
    },
    PaletteCommand {
        id: "toggle_shuffle",
        label: "Playback: Toggle Shuffle",
        shortcut: "s",
    },
    PaletteCommand {
        id: "cycle_repeat",
        label: "Playback: Cycle Repeat Mode",
        shortcut: "r",
    },
    PaletteCommand {
        id: "device_picker",
        label: "Connect: Switch Spotify Device",
        shortcut: "d",
    },
    PaletteCommand {
        id: "zen_mode",
        label: "View: Toggle Zen Fullscreen Now Playing",
        shortcut: "z",
    },
    PaletteCommand {
        id: "toggle_queue",
        label: "View: Toggle Queue Panel",
        shortcut: "q",
    },
    PaletteCommand {
        id: "go_home",
        label: "Navigate: Home Dashboard",
        shortcut: "",
    },
    PaletteCommand {
        id: "go_search",
        label: "Navigate: Instant Search",
        shortcut: "/",
    },
    PaletteCommand {
        id: "go_liked",
        label: "Navigate: Liked Songs",
        shortcut: "",
    },
    PaletteCommand {
        id: "go_podcasts",
        label: "Navigate: Podcasts & Episodes",
        shortcut: "",
    },
    PaletteCommand {
        id: "theme_vynl_dark",
        label: "Theme: Switch to vynl-dark",
        shortcut: "",
    },
    PaletteCommand {
        id: "theme_catppuccin",
        label: "Theme: Switch to catppuccin-mocha",
        shortcut: "",
    },
    PaletteCommand {
        id: "theme_tokyo_night",
        label: "Theme: Switch to tokyo-night",
        shortcut: "",
    },
    PaletteCommand {
        id: "theme_gruvbox",
        label: "Theme: Switch to gruvbox",
        shortcut: "",
    },
    PaletteCommand {
        id: "theme_nord",
        label: "Theme: Switch to nord",
        shortcut: "",
    },
    PaletteCommand {
        id: "toggle_adaptive",
        label: "Theme: Toggle Adaptive Cover Colors",
        shortcut: "",
    },
    PaletteCommand {
        id: "open_help",
        label: "Help: Show Keybindings & Mouse Controls",
        shortcut: "?",
    },
    PaletteCommand {
        id: "quit",
        label: "App: Quit vynl",
        shortcut: "Q",
    },
];

/// Returns built-in Command Palette entries ranked by fuzzy score for `query`.
#[must_use]
pub fn filter_palette_commands(query: &str) -> Vec<&'static PaletteCommand> {
    if query.trim().is_empty() {
        return PALETTE_COMMANDS.iter().collect();
    }
    rank_items(query, PALETTE_COMMANDS, |cmd| cmd.label.to_owned())
        .into_iter()
        .filter_map(|(idx, _)| PALETTE_COMMANDS.get(idx))
        .collect()
}

/// Returns all Command Palette entries (built-ins plus custom `~/.config/vynl/themes/*.toml`
/// themes) ranked by fuzzy score for `query`.
#[must_use]
pub fn filter_palette_entries(query: &str, custom_themes: &[String]) -> Vec<DynamicPaletteEntry> {
    let mut entries: Vec<DynamicPaletteEntry> = PALETTE_COMMANDS
        .iter()
        .map(|cmd| DynamicPaletteEntry {
            id: cmd.id.to_owned(),
            label: cmd.label.to_owned(),
            shortcut: cmd.shortcut.to_owned(),
        })
        .collect();

    for name in custom_themes {
        if Theme::builtin(name).is_none() {
            entries.push(DynamicPaletteEntry {
                id: format!("theme_custom:{name}"),
                label: format!("Theme: Switch to {name} (custom)"),
                shortcut: String::new(),
            });
        }
    }

    if query.trim().is_empty() {
        return entries;
    }

    rank_items(query, &entries, |cmd| cmd.label.clone())
        .into_iter()
        .filter_map(|(idx, _)| entries.get(idx).cloned())
        .collect()
}

/// Context passed into [`render_frame`].
pub struct RenderContext<'a> {
    /// Current application state.
    pub state: &'a AppState,
    /// Active theme (with adaptive cover accent already applied when enabled).
    pub theme: &'a Theme,
    /// Resolved terminal image protocol.
    pub protocol: ImageProtocol,
    /// Whether image rendering is enabled.
    pub images_enabled: bool,
    /// Active tracks visible in the current detail view (album/playlist/liked).
    pub view_tracks: &'a [TrackSummary],
    /// Optional FFT spectrum bars (`0.0..=1.0`) for Zen mode.
    pub spectrum_bars: &'a [f32],
    /// Custom theme names discovered under `~/.config/vynl/themes/*.toml`.
    pub custom_themes: &'a [String],
    /// L1 decoded image cache (mutated on synchronous demo fills).
    pub l1_cache: &'a mut L1ImageCache,
    /// Per-frame Modal-Aware Image Occlusion tracker.
    pub occlusion: &'a mut OcclusionTracker,
    /// Optional background worker for non-blocking remote HTTP cover fetching & decoding.
    pub image_worker: Option<&'a ImageWorker>,
    /// In-flight remote cover requests so we do not enqueue duplicates per frame.
    pub pending_images: &'a mut HashSet<ImageCacheKey>,
    /// Monotonic request ID generator for [`ImageRequest`]s.
    pub next_image_req_id: &'a mut u64,
}

/// Renders a complete frame of `vynl` into a [`Buffer`].
pub fn render_frame(cols: u16, rows: u16, ctx: &mut RenderContext<'_>) -> Buffer {
    let layout = FrameLayout::compute(cols, rows, ctx.state.show_right_sidebar);
    let mut buf = Buffer::filled(cols, rows, ctx.theme.text_primary, ctx.theme.background);

    ctx.occlusion.begin_frame();

    let modal_rect = ctx
        .state
        .modal
        .as_ref()
        .map(|m| modal_bounds(cols, rows, m));
    let modal_cell_rect = modal_rect.map(to_cell_rect);

    if let Some(top) = layout.top_bar {
        render_top_bar(&mut buf, top, ctx.state, ctx.theme);
    }

    if let Some(sidebar) = layout.sidebar {
        render_sidebar(&mut buf, sidebar, ctx.state, ctx.theme);
    }

    render_main_view(&mut buf, layout.main, ctx, modal_cell_rect);

    if let Some(right) = layout.right_sidebar {
        render_right_sidebar(&mut buf, right, ctx, modal_cell_rect);
    }

    render_player_bar(&mut buf, layout, ctx, modal_cell_rect);

    if let Some(ref toast) = ctx.state.toast {
        render_toast(&mut buf, cols, &toast.text, ctx.theme);
    }

    if let (Some(modal), Some(rect)) = (&ctx.state.modal, modal_rect) {
        render_modal(
            &mut buf,
            rect,
            modal,
            ctx.state,
            ctx.theme,
            ctx.custom_themes,
        );
    }

    buf
}

fn modal_bounds(cols: u16, rows: u16, modal: &ActiveModal) -> Rect {
    match modal {
        ActiveModal::CommandPalette { .. } => FrameLayout::centered_modal(cols, rows, 58, 60),
        ActiveModal::DevicePicker { .. } | ActiveModal::AddToPlaylist { .. } => {
            FrameLayout::centered_modal(cols, rows, 48, 48)
        }
        ActiveModal::Help => FrameLayout::centered_modal(cols, rows, 68, 72),
        ActiveModal::SetupWizard { .. } => FrameLayout::centered_modal(cols, rows, 64, 52),
    }
}

const fn to_cell_rect(r: Rect) -> CellRect {
    CellRect::new(r.x, r.y, r.width, r.height)
}

fn border_style(focused: bool, theme: &Theme) -> CellStyle {
    let fg = if focused { theme.accent } else { theme.border };
    CellStyle::fg_bg(fg, theme.surface)
}

fn render_top_bar(buf: &mut Buffer, rect: Rect, state: &AppState, theme: &Theme) {
    let fill = CellStyle::fg_bg(theme.text_primary, theme.surface);
    buf.draw_box(
        rect,
        "vynl",
        CellStyle::fg_bg(theme.border, theme.surface),
        fill,
    );
    let inner = rect.inner();
    if inner.height == 0 {
        return;
    }

    let back_hint = if state.history.is_empty() {
        "[<]"
    } else {
        "[< Back]"
    };
    buf.put_str_clipped(
        inner.x,
        inner.y,
        back_hint,
        10,
        CellStyle::fg_bg(theme.text_muted, theme.surface),
    );

    let search_x = inner.x + 11;
    let right_reserved = 26u16;
    let search_w = inner.width.saturating_sub(11 + right_reserved);
    let search_text = if state.search_query.is_empty() {
        String::from(" / Search songs, artists, albums... (Ctrl+P Palette)")
    } else {
        format!(" / {}", state.search_query)
    };
    buf.put_str_clipped(
        search_x,
        inner.y,
        &search_text,
        search_w,
        CellStyle::fg_bg(
            if state.route == Route::Search {
                theme.accent
            } else {
                theme.text_muted
            },
            theme.background,
        ),
    );

    let user_label = state.user.as_ref().map_or_else(
        || String::from("Guest [?]"),
        |u| format!("{} [?]", u.display_name),
    );
    let badge_x = inner
        .right()
        .saturating_sub(right_reserved.min(inner.width));
    buf.put_str_clipped(
        badge_x,
        inner.y,
        &user_label,
        right_reserved,
        CellStyle::fg_bg(theme.accent, theme.surface).bold(),
    );
}

fn render_sidebar(buf: &mut Buffer, rect: Rect, state: &AppState, theme: &Theme) {
    let focused = state.focus == PanelFocus::Sidebar;
    let fill = CellStyle::fg_bg(theme.text_primary, theme.surface);
    buf.draw_box(rect, "Library", border_style(focused, theme), fill);
    let inner = rect.inner();
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let mut y = inner.y;
    let nav_items = [
        ("Home", state.route == Route::Home),
        ("Search", state.route == Route::Search),
        ("Liked Songs", state.route == Route::LikedSongs),
        ("Podcasts", state.route == Route::Podcasts),
        ("Zen Mode", state.route == Route::ZenNowPlaying),
    ];

    for (idx, (label, active)) in nav_items.iter().enumerate() {
        if y >= inner.bottom() {
            return;
        }
        let selected = focused && state.sidebar_selected == idx;
        let prefix = if selected {
            "> "
        } else if *active {
            "* "
        } else {
            "  "
        };
        let style = if selected {
            CellStyle::fg_bg(theme.background, theme.accent).bold()
        } else if *active {
            CellStyle::fg_bg(theme.accent, theme.surface).bold()
        } else {
            fill
        };
        buf.put_str_clipped(inner.x, y, &format!("{prefix}{label}"), inner.width, style);
        y += 1;
    }

    if y + 2 >= inner.bottom() {
        return;
    }
    y += 1;
    let pill = format!("[Filter: {}]", state.library_filter.label());
    buf.put_str_clipped(
        inner.x,
        y,
        &pill,
        inner.width,
        CellStyle::fg_bg(theme.text_muted, theme.surface).bold(),
    );
    y += 1;

    for (i, line) in sidebar_filtered_labels(state).into_iter().enumerate() {
        if y >= inner.bottom() {
            break;
        }
        let item_idx = nav_items.len() + i;
        let selected = focused && state.sidebar_selected == item_idx;
        let style = if selected {
            CellStyle::fg_bg(theme.background, theme.accent).bold()
        } else {
            CellStyle::fg_bg(theme.text_primary, theme.surface)
        };
        buf.put_str_clipped(inner.x, y, &line, inner.width, style);
        y += 1;
    }
}

fn sidebar_filtered_labels(state: &AppState) -> Vec<String> {
    match state.library_filter {
        vynl_core::model::LibraryFilter::All | vynl_core::model::LibraryFilter::Playlists => state
            .library
            .playlists
            .iter()
            .map(|pl| {
                let pin = if pl.pinned { "* " } else { "  " };
                format!("{pin}{} ({})", pl.name, pl.track_count)
            })
            .collect(),
        vynl_core::model::LibraryFilter::Albums => state
            .library
            .albums
            .iter()
            .map(|alb| format!("  {} ({})", alb.title, alb.release_year))
            .collect(),
        vynl_core::model::LibraryFilter::Artists => state
            .library
            .artists
            .iter()
            .map(|artist| format!("  {}", artist.name))
            .collect(),
        vynl_core::model::LibraryFilter::Podcasts => state
            .library
            .episodes
            .iter()
            .map(|ep| format!("  {}", ep.title))
            .collect(),
    }
}

fn render_main_view(
    buf: &mut Buffer,
    rect: Rect,
    ctx: &mut RenderContext<'_>,
    modal_rect: Option<CellRect>,
) {
    let focused = ctx.state.focus == PanelFocus::Main;
    let fill = CellStyle::fg_bg(ctx.theme.text_primary, ctx.theme.background);

    let title = match &ctx.state.route {
        Route::Home => "Home",
        Route::Search => "Search",
        Route::LikedSongs => "Liked Songs",
        Route::Podcasts => "Podcasts & Episodes",
        Route::ZenNowPlaying => "Zen Now Playing",
        Route::Album(_) => "Album",
        Route::Playlist(_) => "Playlist",
        Route::Artist(_) => "Artist",
    };

    buf.draw_box(
        rect,
        title,
        CellStyle::fg_bg(
            if focused {
                ctx.theme.accent
            } else {
                ctx.theme.border
            },
            ctx.theme.background,
        ),
        fill,
    );

    let inner = rect.inner();
    if inner.width < 4 || inner.height < 2 {
        return;
    }

    match &ctx.state.route {
        Route::Home => render_home_view(buf, inner, ctx, modal_rect),
        Route::Search => render_search_view(buf, inner, ctx, modal_rect),
        Route::ZenNowPlaying => render_zen_view(buf, inner, ctx, modal_rect),
        Route::Podcasts => render_podcasts_view(buf, inner, ctx),
        Route::Artist(id) => render_artist_view(buf, inner, id, ctx, modal_rect),
        Route::LikedSongs | Route::Album(_) | Route::Playlist(_) => {
            render_track_collection_view(buf, inner, ctx, modal_rect);
        }
    }
}

fn render_home_view(
    buf: &mut Buffer,
    inner: Rect,
    ctx: &mut RenderContext<'_>,
    modal_rect: Option<CellRect>,
) {
    let accent = CellStyle::fg_bg(ctx.theme.accent, ctx.theme.background).bold();
    let normal = CellStyle::fg_bg(ctx.theme.text_primary, ctx.theme.background);
    let muted = CellStyle::fg_bg(ctx.theme.text_muted, ctx.theme.background);

    buf.put_str_clipped(
        inner.x,
        inner.y,
        "Featured Albums & Cover Grid",
        inner.width,
        accent,
    );

    // Cover Card Grid (up to 4 cards across)
    let card_w = 18u16;
    let card_h = 8u16;
    let max_cards = usize::from((inner.width / (card_w + 1)).clamp(1, 4));
    let grid_y = inner.y + 1;

    if inner.height >= card_h + 2 {
        for (idx, album) in ctx.state.library.albums.iter().take(max_cards).enumerate() {
            let Ok(idx_u16) = u16::try_from(idx) else {
                break;
            };
            let cx = inner.x + idx_u16 * (card_w + 1);
            let cover_rect = Rect::new(cx, grid_y, card_w.saturating_sub(2), 5);
            if let Some(ref img) = album.cover {
                render_cover_slot(
                    buf,
                    &format!("home_album_{idx}"),
                    cover_rect,
                    &img.url,
                    ctx,
                    modal_rect,
                );
            }
            buf.put_str_clipped(cx, grid_y + 5, &album.title, card_w, normal.bold());
            buf.put_str_clipped(
                cx,
                grid_y + 6,
                &format!("{} ({})", album.artists.join(", "), album.release_year),
                card_w,
                muted,
            );
        }
    }

    let table_y = (grid_y + card_h + 1).min(inner.bottom());
    if table_y + 2 < inner.bottom() {
        buf.put_str_clipped(
            inner.x,
            table_y,
            "Jump Back In — Tracks (Enter to play, L to like, a to queue)",
            inner.width,
            accent,
        );
        let list_rect = Rect::new(
            inner.x,
            table_y + 1,
            inner.width,
            inner.bottom().saturating_sub(table_y + 1),
        );
        render_track_rows(buf, list_rect, ctx.view_tracks, ctx.state, ctx.theme);
    }
}

fn render_search_view(
    buf: &mut Buffer,
    inner: Rect,
    ctx: &mut RenderContext<'_>,
    modal_rect: Option<CellRect>,
) {
    let accent = CellStyle::fg_bg(ctx.theme.accent, ctx.theme.background).bold();
    let muted = CellStyle::fg_bg(ctx.theme.text_muted, ctx.theme.background);

    let prompt = format!("Query: {}_", ctx.state.search_query);
    buf.put_str_clipped(inner.x, inner.y, &prompt, inner.width, accent);

    // Category tabs
    let mut tab_x = inner.x;
    for &cat in SearchCategory::all_tabs() {
        let label = format!("[{}]", cat.label());
        let style = if cat == ctx.state.search_category {
            CellStyle::fg_bg(ctx.theme.background, ctx.theme.accent).bold()
        } else {
            muted
        };
        let Ok(w) = u16::try_from(label.len()) else {
            break;
        };
        if tab_x + w > inner.right() {
            break;
        }
        buf.put_str_clipped(tab_x, inner.y + 1, &label, w, style);
        tab_x += w + 1;
    }

    let results_y = inner.y + 3;
    if results_y >= inner.bottom() {
        return;
    }

    // Top result cover if available
    if let Some(first) = ctx.state.search_results.tracks.first()
        && let Some(ref cover) = first.cover
        && inner.height >= 9
    {
        let cover_rect = Rect::new(inner.x, results_y, 12, 5);
        render_cover_slot(buf, "search_top", cover_rect, &cover.url, ctx, modal_rect);
        buf.put_str_clipped(
            inner.x + 14,
            results_y + 1,
            &format!("Top Result: {}", first.title),
            inner.width.saturating_sub(14),
            accent,
        );
        buf.put_str_clipped(
            inner.x + 14,
            results_y + 2,
            &format!("{} — {}", first.artist_line(), first.album),
            inner.width.saturating_sub(14),
            muted,
        );
        let list_y = results_y + 6;
        if list_y < inner.bottom() {
            let list_rect = Rect::new(inner.x, list_y, inner.width, inner.bottom() - list_y);
            render_track_rows(
                buf,
                list_rect,
                &ctx.state.search_results.tracks,
                ctx.state,
                ctx.theme,
            );
        }
        return;
    }

    let list_rect = Rect::new(inner.x, results_y, inner.width, inner.bottom() - results_y);
    render_track_rows(
        buf,
        list_rect,
        &ctx.state.search_results.tracks,
        ctx.state,
        ctx.theme,
    );
}

fn render_track_collection_view(
    buf: &mut Buffer,
    inner: Rect,
    ctx: &mut RenderContext<'_>,
    modal_rect: Option<CellRect>,
) {
    let accent = CellStyle::fg_bg(ctx.theme.accent, ctx.theme.background).bold();
    let normal = CellStyle::fg_bg(ctx.theme.text_primary, ctx.theme.background);
    let muted = CellStyle::fg_bg(ctx.theme.text_muted, ctx.theme.background);

    let (heading, subtitle, cover_url) = match &ctx.state.route {
        Route::LikedSongs => (
            String::from("Liked Songs"),
            format!("{} saved tracks", ctx.view_tracks.len()),
            ctx.view_tracks
                .first()
                .and_then(|t| t.cover.as_ref())
                .map(|c| c.url.clone()),
        ),
        Route::Playlist(id) => ctx
            .state
            .library
            .playlists
            .iter()
            .find(|p| p.id == *id)
            .map_or_else(
                || (String::from("Playlist"), String::new(), None),
                |p| {
                    (
                        p.name.clone(),
                        format!("{} • {} tracks • {}", p.owner, p.track_count, p.description),
                        p.cover.as_ref().map(|c| c.url.clone()),
                    )
                },
            ),
        Route::Album(id) => ctx
            .state
            .library
            .albums
            .iter()
            .find(|a| a.id == *id)
            .map_or_else(
                || (String::from("Album"), String::new(), None),
                |a| {
                    (
                        a.title.clone(),
                        format!(
                            "{} • {} • {} tracks",
                            a.artists.join(", "),
                            a.release_year,
                            a.total_tracks
                        ),
                        a.cover.as_ref().map(|c| c.url.clone()),
                    )
                },
            ),
        _ => (String::new(), String::new(), None),
    };

    let header_h = 6u16.min(inner.height);
    if header_h >= 5
        && let Some(ref url) = cover_url
    {
        render_cover_slot(
            buf,
            "collection_header",
            Rect::new(inner.x, inner.y, 12, 5),
            url,
            ctx,
            modal_rect,
        );
    }

    let text_x = if cover_url.is_some() && header_h >= 5 {
        inner.x + 14
    } else {
        inner.x
    };
    let text_w = inner.right().saturating_sub(text_x);
    buf.put_str_clipped(text_x, inner.y + 1, &heading, text_w, accent);
    buf.put_str_clipped(text_x, inner.y + 2, &subtitle, text_w, muted);
    buf.put_str_clipped(
        text_x,
        inner.y + 3,
        "[Enter: Play] [x: Multi-Select] [f: Filter] [P: Add to Playlist]",
        text_w,
        normal.dim(),
    );

    let mut list_y = inner.y + header_h;
    if let Some(ref filter) = ctx.state.inline_filter
        && list_y < inner.bottom()
    {
        buf.put_str_clipped(
            inner.x,
            list_y,
            &format!("Filter: {filter}_ (Esc to clear)"),
            inner.width,
            accent,
        );
        list_y += 1;
    }

    if list_y < inner.bottom() {
        let list_rect = Rect::new(inner.x, list_y, inner.width, inner.bottom() - list_y);
        render_track_rows(buf, list_rect, ctx.view_tracks, ctx.state, ctx.theme);
    }
}

fn render_artist_view(
    buf: &mut Buffer,
    inner: Rect,
    artist_id: &str,
    ctx: &mut RenderContext<'_>,
    modal_rect: Option<CellRect>,
) {
    let accent = CellStyle::fg_bg(ctx.theme.accent, ctx.theme.background).bold();
    let muted = CellStyle::fg_bg(ctx.theme.text_muted, ctx.theme.background);

    if let Some(artist) = ctx.state.library.artists.iter().find(|a| a.id == artist_id) {
        if let Some(ref avatar) = artist.avatar
            && inner.height >= 6
        {
            render_cover_slot(
                buf,
                "artist_avatar",
                Rect::new(inner.x, inner.y, 12, 5),
                &avatar.url,
                ctx,
                modal_rect,
            );
        }
        let tx = inner.x + 14;
        let tw = inner.right().saturating_sub(tx);
        buf.put_str_clipped(tx, inner.y + 1, &artist.name, tw, accent);
        buf.put_str_clipped(
            tx,
            inner.y + 2,
            &format!(
                "{} followers • {} • {}",
                artist.followers,
                if artist.following {
                    "Following"
                } else {
                    "Follow"
                },
                artist.genres.join(", ")
            ),
            tw,
            muted,
        );
    }

    let list_y = (inner.y + 6).min(inner.bottom());
    if list_y < inner.bottom() {
        render_track_rows(
            buf,
            Rect::new(inner.x, list_y, inner.width, inner.bottom() - list_y),
            ctx.view_tracks,
            ctx.state,
            ctx.theme,
        );
    }
}

fn render_podcasts_view(buf: &mut Buffer, inner: Rect, ctx: &RenderContext<'_>) {
    let normal = CellStyle::fg_bg(ctx.theme.text_primary, ctx.theme.background);
    let muted = CellStyle::fg_bg(ctx.theme.text_muted, ctx.theme.background);
    let accent = CellStyle::fg_bg(ctx.theme.accent, ctx.theme.background).bold();

    for (idx, ep) in ctx.state.library.episodes.iter().enumerate() {
        let Ok(offset) = u16::try_from(idx * 2) else {
            break;
        };
        let y = inner.y + offset;
        if y + 1 >= inner.bottom() {
            break;
        }
        let selected = ctx.state.focus == PanelFocus::Main && ctx.state.main_selected == idx;
        let style = if selected {
            CellStyle::fg_bg(ctx.theme.background, ctx.theme.accent).bold()
        } else {
            normal
        };
        let played_tag = if ep.fully_played {
            "[Played]"
        } else {
            "[Resume]"
        };
        buf.put_str_clipped(
            inner.x,
            y,
            &format!("{played_tag} {} — {}", ep.title, ep.show_name),
            inner.width,
            style,
        );
        let pct = ep
            .resume_position_ms
            .saturating_mul(100)
            .checked_div(ep.duration_ms)
            .unwrap_or(0)
            .min(100);
        buf.put_str_clipped(
            inner.x + 2,
            y + 1,
            &format!(
                "{} / {} ({pct}%)",
                format_duration_ms(ep.resume_position_ms),
                format_duration_ms(ep.duration_ms)
            ),
            inner.width.saturating_sub(2),
            if selected { accent } else { muted },
        );
    }
}

fn render_zen_view(
    buf: &mut Buffer,
    inner: Rect,
    ctx: &mut RenderContext<'_>,
    modal_rect: Option<CellRect>,
) {
    let accent = CellStyle::fg_bg(ctx.theme.accent, ctx.theme.background).bold();
    let muted = CellStyle::fg_bg(ctx.theme.text_muted, ctx.theme.background);

    let cover_w = (inner.width / 2).clamp(16, 44);
    let cover_h = (inner.height.saturating_sub(6)).clamp(6, 20);
    let cover_x = inner.x + inner.width.saturating_sub(cover_w) / 2;
    let cover_y = inner.y + 1;

    if let Some(ref track) = ctx.state.playback.current_track {
        if let Some(ref cover) = track.cover {
            render_cover_slot(
                buf,
                "zen_hero",
                Rect::new(cover_x, cover_y, cover_w, cover_h),
                &cover.url,
                ctx,
                modal_rect,
            );
        }
        let info_y = cover_y + cover_h + 1;
        if info_y < inner.bottom() {
            buf.put_str_clipped(cover_x, info_y, &track.title, cover_w, accent);
        }
        if info_y + 1 < inner.bottom() {
            buf.put_str_clipped(
                cover_x,
                info_y + 1,
                &format!("{} — {}", track.artist_line(), track.album),
                cover_w,
                muted,
            );
        }
        if !ctx.spectrum_bars.is_empty() && info_y + 3 < inner.bottom() {
            let blocks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
            let mut viz = String::with_capacity(ctx.spectrum_bars.len() * 2);
            for &level in ctx.spectrum_bars {
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let idx = (level.clamp(0.0, 1.0) * 7.0).round() as usize;
                viz.push(blocks[idx.min(7)]);
                viz.push(' ');
            }
            buf.put_str_clipped(cover_x, info_y + 3, &viz, cover_w, accent);
        }
    } else {
        buf.put_str_clipped(
            inner.x + 2,
            inner.y + 2,
            "Nothing playing — press Enter on any track.",
            inner.width.saturating_sub(4),
            muted,
        );
    }
}

fn render_track_rows(
    buf: &mut Buffer,
    rect: Rect,
    tracks: &[TrackSummary],
    state: &AppState,
    theme: &Theme,
) {
    if rect.height == 0 || rect.width < 12 {
        return;
    }
    let header_style = CellStyle::fg_bg(theme.text_muted, theme.background).bold();
    buf.put_str_clipped(
        rect.x,
        rect.y,
        " #  Title                      Artist               Time",
        rect.width,
        header_style,
    );

    let playing_id = state.playback.current_track.as_ref().map(|t| t.id.as_str());
    let focused = state.focus == PanelFocus::Main;
    let clamped_sel = if tracks.is_empty() {
        0
    } else {
        state.main_selected.min(tracks.len() - 1)
    };

    for (idx, track) in tracks.iter().enumerate() {
        let Ok(row_offset) = u16::try_from(idx + 1) else {
            break;
        };
        let y = rect.y + row_offset;
        if y >= rect.bottom() {
            break;
        }
        let is_selected = focused && clamped_sel == idx;
        let is_playing = playing_id == Some(track.id.as_str());
        let is_marked = state.multi_selected.contains(&idx);
        let style = if is_selected {
            CellStyle::fg_bg(theme.background, theme.accent).bold()
        } else if is_playing {
            CellStyle::fg_bg(theme.accent, theme.background).bold()
        } else {
            CellStyle::fg_bg(theme.text_primary, theme.background)
        };

        let mark = if is_marked {
            "[x]"
        } else if is_playing {
            " > "
        } else {
            "   "
        };
        let heart = if track.is_liked { "♥" } else { " " };
        let explicit = if track.explicit { "E" } else { " " };
        let line = format!(
            "{mark}{:>2} {:<24} {:<18} {explicit}{heart} {}",
            idx + 1,
            truncate_chars(&track.title, 24),
            truncate_chars(&track.artist_line(), 18),
            track.formatted_duration()
        );
        buf.put_str_clipped(rect.x, y, &line, rect.width, style);
    }
}

fn render_right_sidebar(
    buf: &mut Buffer,
    rect: Rect,
    ctx: &mut RenderContext<'_>,
    modal_rect: Option<CellRect>,
) {
    let focused = ctx.state.focus == PanelFocus::RightSidebar;
    let fill = CellStyle::fg_bg(ctx.theme.text_primary, ctx.theme.surface);
    let title = match ctx.state.right_tab {
        RightSidebarTab::NowPlaying => "Now Playing [q: Queue]",
        RightSidebarTab::Queue => "Queue [q: Now Playing]",
    };
    buf.draw_box(rect, title, border_style(focused, ctx.theme), fill);
    let inner = rect.inner();
    if inner.width < 4 || inner.height < 4 {
        return;
    }

    let accent = CellStyle::fg_bg(ctx.theme.accent, ctx.theme.surface).bold();
    let muted = CellStyle::fg_bg(ctx.theme.text_muted, ctx.theme.surface);

    match ctx.state.right_tab {
        RightSidebarTab::NowPlaying => {
            if let Some(ref track) = ctx.state.playback.current_track {
                let cover_h = 10u16.min(inner.height.saturating_sub(6));
                if cover_h >= 4
                    && let Some(ref cover) = track.cover
                {
                    render_cover_slot(
                        buf,
                        "now_playing_hero",
                        Rect::new(inner.x, inner.y, inner.width, cover_h),
                        &cover.url,
                        ctx,
                        modal_rect,
                    );
                }
                let ty = inner.y + cover_h + 1;
                if ty < inner.bottom() {
                    buf.put_str_clipped(inner.x, ty, &track.title, inner.width, accent);
                }
                if ty + 1 < inner.bottom() {
                    buf.put_str_clipped(inner.x, ty + 1, &track.artist_line(), inner.width, fill);
                }
                if ty + 2 < inner.bottom() {
                    buf.put_str_clipped(inner.x, ty + 2, &track.album, inner.width, muted);
                }
                if ty + 4 < inner.bottom() {
                    buf.put_str_clipped(
                        inner.x,
                        ty + 4,
                        "Next in Queue:",
                        inner.width,
                        muted.bold(),
                    );
                    if let Some(next_track) = ctx.state.queue.up_next.first()
                        && ty + 5 < inner.bottom()
                    {
                        buf.put_str_clipped(
                            inner.x,
                            ty + 5,
                            &format!("{} — {}", next_track.title, next_track.artist_line()),
                            inner.width,
                            fill,
                        );
                    }
                }
            }
        }
        RightSidebarTab::Queue => {
            let mut y = inner.y;
            buf.put_str_clipped(inner.x, y, "Up Next", inner.width, accent);
            y += 1;
            for (i, t) in ctx.state.queue.up_next.iter().enumerate() {
                if y >= inner.bottom() {
                    break;
                }
                buf.put_str_clipped(
                    inner.x,
                    y,
                    &format!("{}. {}", i + 1, t.title),
                    inner.width,
                    fill,
                );
                y += 1;
            }
            if y + 2 < inner.bottom() {
                y += 1;
                buf.put_str_clipped(inner.x, y, "Recently Played", inner.width, muted.bold());
                y += 1;
                for t in &ctx.state.queue.recently_played {
                    if y >= inner.bottom() {
                        break;
                    }
                    buf.put_str_clipped(inner.x, y, &t.title, inner.width, muted);
                    y += 1;
                }
            }
        }
    }
}

#[expect(clippy::too_many_lines)]
fn render_player_bar(
    buf: &mut Buffer,
    layout: FrameLayout,
    ctx: &mut RenderContext<'_>,
    modal_rect: Option<CellRect>,
) {
    let rect = layout.player_bar;
    let fill = CellStyle::fg_bg(ctx.theme.text_primary, ctx.theme.surface);
    buf.draw_box(
        rect,
        "",
        CellStyle::fg_bg(ctx.theme.border, ctx.theme.surface),
        fill,
    );
    let inner = rect.inner();
    if inner.height == 0 {
        return;
    }

    let accent = CellStyle::fg_bg(ctx.theme.accent, ctx.theme.surface).bold();
    let muted = CellStyle::fg_bg(ctx.theme.text_muted, ctx.theme.surface);
    let pb = &ctx.state.playback;

    // Left: Cover thumbnail + Track/Artist
    let mut left_text_x = inner.x;
    if layout.breakpoint != LayoutBreakpoint::MiniPlayer
        && inner.height >= 2
        && let Some(ref track) = pb.current_track
        && let Some(ref cover) = track.cover
    {
        render_cover_slot(
            buf,
            "player_bar_thumb",
            Rect::new(inner.x, inner.y, 4, 2),
            &cover.url,
            ctx,
            modal_rect,
        );
        left_text_x += 5;
    }

    let left_w = if layout.breakpoint == LayoutBreakpoint::MiniPlayer {
        inner.width
    } else {
        layout
            .progress_bar
            .x
            .saturating_sub(left_text_x.saturating_add(2))
            .max(8)
    };
    if let Some(ref track) = pb.current_track {
        let heart = if track.is_liked { " ♥" } else { " ♡" };
        buf.put_str_clipped(
            left_text_x,
            inner.y,
            &format!("{}{heart}", track.title),
            left_w,
            accent,
        );
        if inner.height >= 2 && layout.breakpoint != LayoutBreakpoint::MiniPlayer {
            buf.put_str_clipped(
                left_text_x,
                inner.y + 1,
                &track.artist_line(),
                left_w,
                muted,
            );
        }
    } else {
        buf.put_str_clipped(left_text_x, inner.y, "Not Playing", left_w, muted);
    }

    // Center: Transport controls & progress bar
    let shuf = if pb.shuffle { "[S*]" } else { "[S]" };
    let play_icon = if pb.is_playing { "[||]" } else { "[>]" };
    let rep = format!("[R:{}]", pb.repeat.as_api_str());
    let controls = format!("{shuf} [|<] {play_icon} [>|] {rep}");
    if layout.breakpoint != LayoutBreakpoint::MiniPlayer {
        let ctrl_w = u16::try_from(controls.len()).unwrap_or(34).min(inner.width);
        let ctrl_x = inner.x + inner.width.saturating_sub(ctrl_w) / 2;
        buf.put_str_clipped(ctrl_x, inner.y, &controls, ctrl_w, fill.bold());
    } else if inner.height >= 2 {
        buf.put_str_clipped(inner.x, inner.y + 1, &controls, inner.width, fill.bold());
    }

    if inner.height >= 2 {
        let dur_ms = pb.current_track.as_ref().map_or(0, |t| t.duration_ms);
        let pos_str = format_duration_ms(pb.position_ms);
        let dur_str = format_duration_ms(dur_ms);
        let bar_w = layout.progress_bar.width.saturating_sub(14).max(6);
        let filled = pb
            .position_ms
            .saturating_mul(u64::from(bar_w))
            .checked_div(dur_ms)
            .map_or(0, |ratio| u16::try_from(ratio).unwrap_or(bar_w).min(bar_w));
        let mut bar = String::with_capacity(usize::from(bar_w));
        for i in 0..bar_w {
            match i.cmp(&filled) {
                std::cmp::Ordering::Less => bar.push('━'),
                std::cmp::Ordering::Equal => bar.push('●'),
                std::cmp::Ordering::Greater => bar.push('─'),
            }
        }
        buf.put_str_clipped(
            layout.progress_bar.x,
            layout.progress_bar.y,
            &format!("{pos_str} {bar} {dur_str}"),
            layout.progress_bar.width,
            accent,
        );
    }

    // Right: Volume & active Connect device
    if layout.volume_bar.width >= 6 {
        let vol_label = if pb.muted {
            String::from("Vol MUTE")
        } else {
            format!("Vol {:>3}%", pb.volume_percent)
        };
        buf.put_str_clipped(
            layout.volume_bar.x,
            layout.volume_bar.y,
            &vol_label,
            layout.volume_bar.width,
            fill,
        );
        if inner.height >= 2 {
            let dev_name = pb
                .active_device
                .as_ref()
                .map_or("vynl", |d| d.name.as_str());
            buf.put_str_clipped(
                layout.volume_bar.x,
                inner.y + 1,
                dev_name,
                layout.volume_bar.width,
                muted,
            );
        }
    }
}

fn render_toast(buf: &mut Buffer, cols: u16, text: &str, theme: &Theme) {
    let w = u16::try_from(text.len() + 4)
        .unwrap_or(32)
        .min(cols.saturating_sub(2));
    let x = cols.saturating_sub(w + 2);
    buf.put_str_clipped(
        x,
        1,
        &format!(" {text} "),
        w,
        CellStyle::fg_bg(theme.background, theme.accent).bold(),
    );
}

#[expect(clippy::too_many_lines)]
fn render_modal(
    buf: &mut Buffer,
    rect: Rect,
    modal: &ActiveModal,
    state: &AppState,
    theme: &Theme,
    custom_themes: &[String],
) {
    let fill = CellStyle::fg_bg(theme.text_primary, theme.surface);
    let accent = CellStyle::fg_bg(theme.accent, theme.surface).bold();
    let muted = CellStyle::fg_bg(theme.text_muted, theme.surface);

    let title = match modal {
        ActiveModal::CommandPalette { .. } => "Command Palette (Ctrl+P)",
        ActiveModal::DevicePicker { .. } => "Spotify Connect Devices (d)",
        ActiveModal::AddToPlaylist { .. } => "Add to Playlist (P)",
        ActiveModal::Help => "vynl Help — Hybrid Keybindings & Mouse (?)",
        ActiveModal::SetupWizard { .. } => "Spotify Setup Wizard (BYO Client ID)",
    };

    buf.draw_box(
        rect,
        title,
        CellStyle::fg_bg(theme.accent, theme.surface),
        fill,
    );
    let inner = rect.inner();
    if inner.width < 4 || inner.height < 2 {
        return;
    }

    match modal {
        ActiveModal::CommandPalette { query, selected } => {
            buf.put_str_clipped(
                inner.x,
                inner.y,
                &format!("> {query}_"),
                inner.width,
                accent,
            );
            let matches = filter_palette_entries(query, custom_themes);
            for (i, cmd) in matches.iter().enumerate() {
                let Ok(off) = u16::try_from(i + 2) else {
                    break;
                };
                if inner.y + off >= inner.bottom() {
                    break;
                }
                let style = if i == *selected {
                    CellStyle::fg_bg(theme.background, theme.accent).bold()
                } else {
                    fill
                };
                let row = if cmd.shortcut.is_empty() {
                    cmd.label.clone()
                } else {
                    format!("{}  [{}]", cmd.label, cmd.shortcut)
                };
                buf.put_str_clipped(inner.x, inner.y + off, &row, inner.width, style);
            }
        }
        ActiveModal::DevicePicker { selected } => {
            buf.put_str_clipped(
                inner.x,
                inner.y,
                "Select active Spotify Connect target (Enter):",
                inner.width,
                muted,
            );
            for (i, dev) in state.devices.iter().enumerate() {
                let Ok(off) = u16::try_from(i + 2) else {
                    break;
                };
                if inner.y + off >= inner.bottom() {
                    break;
                }
                let active = if dev.is_active { "*" } else { " " };
                let style = if i == *selected {
                    CellStyle::fg_bg(theme.background, theme.accent).bold()
                } else {
                    fill
                };
                buf.put_str_clipped(
                    inner.x,
                    inner.y + off,
                    &format!(
                        " [{active}] {} ({}) — {}%",
                        dev.name, dev.device_type, dev.volume_percent
                    ),
                    inner.width,
                    style,
                );
            }
        }
        ActiveModal::AddToPlaylist {
            track_ids,
            selected,
        } => {
            buf.put_str_clipped(
                inner.x,
                inner.y,
                &format!("Add {} track(s) to playlist:", track_ids.len()),
                inner.width,
                muted,
            );
            for (i, pl) in state.library.playlists.iter().enumerate() {
                let Ok(off) = u16::try_from(i + 2) else {
                    break;
                };
                if inner.y + off >= inner.bottom() {
                    break;
                }
                let style = if i == *selected {
                    CellStyle::fg_bg(theme.background, theme.accent).bold()
                } else {
                    fill
                };
                buf.put_str_clipped(
                    inner.x,
                    inner.y + off,
                    &format!("  {}", pl.name),
                    inner.width,
                    style,
                );
            }
        }
        ActiveModal::Help => {
            let lines = [
                "Arrows / h j k l    Navigate panels & lists (g/G top/bottom)",
                "Tab / Shift+Tab     Cycle focus between Sidebar, Main & Queue",
                "Enter / Click       Play track or open selected view",
                "Space               Play / Pause",
                "n / p  (or > / <)   Next / Previous track",
                "Shift+Left / Right  Seek -5s / +5s (or click Progress Bar)",
                "+ / - / m           Volume +/-5% / Mute (or click Volume Bar)",
                "s / r               Toggle Shuffle / Cycle Repeat",
                "L / a / P / x       Like / Queue / Add to Playlist / Multi-Select",
                "/ or Ctrl+F         Instant Search (f for inline list filter)",
                "Ctrl+P or :         Command Palette (themes, navigation, controls)",
                "d / q / z           Device Picker / Queue Panel / Zen Cover Mode",
                "Esc / Q / Ctrl+C    Dismiss modal / Quit cleanly",
            ];
            for (i, line) in lines.iter().enumerate() {
                let Ok(off) = u16::try_from(i) else {
                    break;
                };
                if inner.y + off >= inner.bottom() {
                    break;
                }
                buf.put_str_clipped(inner.x, inner.y + off, line, inner.width, fill);
            }
        }
        ActiveModal::SetupWizard { client_id_input } => {
            let lines = [
                "vynl uses OAuth 2.0 PKCE with your own Spotify Developer Client ID.",
                "1. Create an app at developer.spotify.com/dashboard",
                "2. Add Redirect URI: http://127.0.0.1:8888/callback",
                "3. Paste your Client ID below and press Enter (or Esc for Demo):",
            ];
            for (i, line) in lines.iter().enumerate() {
                let Ok(off) = u16::try_from(i) else {
                    break;
                };
                if inner.y + off >= inner.bottom() {
                    break;
                }
                buf.put_str_clipped(inner.x, inner.y + off, line, inner.width, muted);
            }
            if inner.height >= 6 {
                buf.put_str_clipped(
                    inner.x,
                    inner.y + 5,
                    &format!("Client ID: {client_id_input}_"),
                    inner.width,
                    accent,
                );
            }
        }
    }
}

fn render_cover_slot(
    buf: &mut Buffer,
    slot_id: &str,
    rect: Rect,
    url: &str,
    ctx: &mut RenderContext<'_>,
    modal_rect: Option<CellRect>,
) {
    if !ctx.images_enabled || rect.width == 0 || rect.height == 0 {
        return;
    }

    let cell_rect = to_cell_rect(rect);
    ctx.occlusion.register_slot(slot_id, cell_rect, url);

    // Modal-Aware Image Occlusion (ADR-0004): suppress overlapping covers cleanly
    // while a floating modal is open; L1 cache restores instantaneously on close.
    if ctx.occlusion.is_slot_occluded(slot_id, modal_rect) {
        return;
    }

    let key = ImageCacheKey::new(url, rect.width, rect.height, ctx.protocol);
    let is_remote_url = url.starts_with("https://") || url.starts_with("http://");

    let encoded = if let Some(cached) = ctx.l1_cache.get(&key) {
        cached.encoded.clone()
    } else if is_remote_url && let Some(worker) = ctx.image_worker {
        // Non-blocking remote cover pipeline: enqueue download/decode on ImageWorker
        // and show a lightweight procedural placeholder without poisoning L1 cache.
        if ctx.pending_images.insert(key.clone()) {
            let req_id = *ctx.next_image_req_id;
            *ctx.next_image_req_id = ctx.next_image_req_id.saturating_add(1);
            let _ = worker.submit(ImageRequest {
                id: req_id,
                viewport_epoch: worker.current_viewport(),
                url: url.to_owned(),
                cols: rect.width,
                rows: rect.height,
                cell_px_w: 8,
                cell_px_h: 16,
                protocol: ctx.protocol,
                priority: if slot_id.contains("now") || slot_id.contains("zen") {
                    ImagePriority::High
                } else {
                    ImagePriority::Normal
                },
                palette_colors: 4,
            });
        }
        let placeholder = generate_demo_cover(url, 32);
        encode_for_protocol(&placeholder, ctx.protocol, rect.width, rect.height, 8, 16)
    } else {
        // Procedural cover fallback (used in --demo and headless snapshot tests)
        let img = generate_demo_cover(url, 64);
        let palette = img.extract_dominant_palette(3);
        let enc = encode_for_protocol(&img, ctx.protocol, rect.width, rect.height, 8, 16);
        ctx.l1_cache.insert(
            key,
            vynl_images::CachedFrame {
                encoded: enc.clone(),
                palette,
            },
        );
        enc
    };

    match encoded {
        EncodedImage::Halfblocks(rows) => {
            blit_halfblock_rows(buf, rect, &rows);
        }
        EncodedImage::Kitty(seq) | EncodedImage::ITerm2(seq) | EncodedImage::Sixel(seq) => {
            buf.push_graphics_overlay(rect.x, rect.y, seq);
        }
    }
}

fn blit_halfblock_rows(buf: &mut Buffer, rect: Rect, rows: &[String]) {
    let mut active_style = CellStyle::fg_bg(Rgb::new(30, 30, 30), Rgb::new(18, 18, 18));
    for (r_idx, row_str) in rows.iter().enumerate() {
        let Ok(r) = u16::try_from(r_idx) else {
            break;
        };
        if r >= rect.height {
            break;
        }
        let mut col = 0u16;
        let mut remaining = row_str.as_str();
        while !remaining.is_empty() && col < rect.width {
            if let Some(after_esc) = remaining.strip_prefix("\x1b[") {
                if let Some(m_pos) = after_esc.find('m') {
                    let params = &after_esc[..m_pos];
                    if let Some(style) = parse_halfblock_sgr(params) {
                        active_style = style;
                    }
                    remaining = &after_esc[m_pos + 1..];
                    continue;
                }
                break;
            }
            let Some(ch) = remaining.chars().next() else {
                break;
            };
            buf.set(rect.x + col, rect.y + r, ch, active_style);
            col = col.saturating_add(1);
            remaining = &remaining[ch.len_utf8()..];
        }
    }
}

fn parse_halfblock_sgr(params: &str) -> Option<CellStyle> {
    let mut parts = params.split(';');
    if parts.next()? != "38" || parts.next()? != "2" {
        return None;
    }
    let tr: u8 = parts.next()?.parse().ok()?;
    let tg: u8 = parts.next()?.parse().ok()?;
    let tb: u8 = parts.next()?.parse().ok()?;
    if parts.next()? != "48" || parts.next()? != "2" {
        return None;
    }
    let br: u8 = parts.next()?.parse().ok()?;
    let bg: u8 = parts.next()?.parse().ok()?;
    let bb: u8 = parts.next()?.parse().ok()?;
    Some(CellStyle::fg_bg(Rgb::new(tr, tg, tb), Rgb::new(br, bg, bb)))
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_owned()
    } else if max_chars > 1 {
        let mut out: String = s.chars().take(max_chars - 1).collect();
        out.push('…');
        out
    } else {
        String::from("…")
    }
}
