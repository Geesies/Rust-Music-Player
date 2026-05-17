use std::{
    collections::{HashMap, HashSet},
    ops::RangeInclusive,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use eframe::egui::{
    self, Align, Color32, ColorImage, CornerRadius, FontId, Frame, Id, Image, Layout, Pos2, Rect,
    RichText, Sense, Stroke, TextureHandle, TextureOptions, Ui, UiBuilder, Vec2,
};
use rand::{seq::SliceRandom, thread_rng};
use serde::{Deserialize, Serialize};

use crate::{
    database::LibraryCache,
    eq::{EqBand, EqProfile},
    library::{
        art_bytes, load_album_metadata, rebuild_full_cache, scan_libraries, Album, AlbumMetadata,
        ArtSource, Artist, Song,
    },
    player::Player,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CenterView {
    Artists,
    Albums(usize),
    Songs(usize, usize),
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LyricsView {
    Lyrics,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selection {
    Artist(usize),
    Album(usize, usize),
    Song(usize, usize, usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SongRef {
    artist: usize,
    album: usize,
    song: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct UiSettings {
    library_paths: Vec<PathBuf>,
    new_library_path: String,
    background_rgba: [u8; 4],
    panel_rgba: [u8; 4],
    header_rgba: [u8; 4],
    font_rgba: [u8; 4],
    accent_rgba: [u8; 4],
    image_rounding: f32,
    window_rounding: f32,
    inside_spacing: f32,
    outside_spacing: f32,
    lyrics_file_path: String,
    eq_enabled: bool,
    eq_bands: Vec<EqBand>,
    eq_profiles: Vec<EqProfile>,
    eq_profile_name: String,
    eq_selected_profile: usize,
}

struct ArtLoadResult {
    key: String,
    image: ColorImage,
}

struct LyricsLoadResult {
    song_path: PathBuf,
    lyrics: Option<String>,
    status: String,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            library_paths: vec![PathBuf::from("/home/geesies/Music")],
            new_library_path: String::new(),
            background_rgba: [0, 0, 0, 255],
            panel_rgba: [150, 150, 150, 255],
            header_rgba: [112, 112, 112, 255],
            font_rgba: [0, 0, 0, 255],
            accent_rgba: [101, 216, 220, 255],
            image_rounding: 34.0,
            window_rounding: 28.0,
            inside_spacing: 18.0,
            outside_spacing: 12.0,
            lyrics_file_path: String::new(),
            eq_enabled: false,
            eq_bands: default_eq_bands(),
            eq_profiles: Vec::new(),
            eq_profile_name: String::new(),
            eq_selected_profile: 0,
        }
    }
}

pub struct MusicPlayerApp {
    player: Player,
    artists: Vec<Artist>,
    library_rx: Option<Receiver<Vec<Artist>>>,
    album_metadata_rx: Option<Receiver<AlbumMetadata>>,
    pending_album_metadata: Option<(usize, usize)>,
    center_view: CenterView,
    selection: Option<Selection>,
    now_playing: Option<SongRef>,
    queue: Vec<SongRef>,
    queue_index: usize,
    is_playing: bool,
    shuffle: bool,
    volume: f32,
    status: String,
    textures: HashMap<String, TextureHandle>,
    pending_art: HashSet<String>,
    art_tx: mpsc::Sender<ArtLoadResult>,
    art_rx: Receiver<ArtLoadResult>,
    lyrics_view: LyricsView,
    lyrics_text: Option<String>,
    lyrics_status: String,
    pending_lyrics: Option<PathBuf>,
    lyrics_tx: mpsc::Sender<LyricsLoadResult>,
    lyrics_rx: Receiver<LyricsLoadResult>,
    settings: UiSettings,
    saved_settings: UiSettings,
}

impl MusicPlayerApp {
    pub fn new() -> Self {
        let settings = load_settings();
        let saved_settings = settings.clone();
        let (library_tx, library_rx) = mpsc::channel();
        let (art_tx, art_rx) = mpsc::channel();
        let (lyrics_tx, lyrics_rx) = mpsc::channel();
        let library_paths = settings.library_paths.clone();
        thread::spawn(move || {
            let artists = scan_libraries(&library_paths);
            let _ = library_tx.send(artists);
        });

        let player = Player::new().expect("Failed to initialize audio player");
        player.set_volume(0.75);

        Self {
            player,
            artists: Vec::new(),
            library_rx: Some(library_rx),
            album_metadata_rx: None,
            pending_album_metadata: None,
            center_view: CenterView::Artists,
            selection: None,
            now_playing: None,
            queue: Vec::new(),
            queue_index: 0,
            is_playing: false,
            shuffle: false,
            volume: 0.75,
            status: "Loading library".to_string(),
            textures: HashMap::new(),
            pending_art: HashSet::new(),
            art_tx,
            art_rx,
            lyrics_view: LyricsView::Lyrics,
            lyrics_text: None,
            lyrics_status: "Select or play a song".to_string(),
            pending_lyrics: None,
            lyrics_tx,
            lyrics_rx,
            saved_settings,
            settings,
        }
    }

    fn play_selection(&mut self) {
        match self.selection {
            Some(Selection::Artist(artist)) => self.play_artist(artist),
            Some(Selection::Album(artist, album)) => self.play_album(artist, album),
            Some(Selection::Song(artist, album, song)) => {
                self.ensure_album_metadata(artist, album);
                self.play_song_scope(artist, album, song);
            }
            None => self.status = "Nothing selected".to_string(),
        }
    }

    fn start_library_scan(&mut self, full_cache: bool) {
        let paths = self.settings.library_paths.clone();
        let (library_tx, library_rx) = mpsc::channel();
        thread::spawn(move || {
            let artists = if full_cache {
                rebuild_full_cache(&paths)
            } else {
                scan_libraries(&paths)
            };
            let _ = library_tx.send(artists);
        });

        self.library_rx = Some(library_rx);
        self.album_metadata_rx = None;
        self.pending_album_metadata = None;
        self.status = if full_cache {
            "Rebuilding full library cache".to_string()
        } else {
            "Loading library".to_string()
        };
    }

    fn play_artist(&mut self, artist_index: usize) {
        let mut queue = Vec::new();

        if let Some(artist) = self.artists.get(artist_index) {
            for album_index in 0..artist.albums.len() {
                for song_index in 0..artist.albums[album_index].songs.len() {
                    queue.push(SongRef {
                        artist: artist_index,
                        album: album_index,
                        song: song_index,
                    });
                }
            }
        }

        self.start_queue(queue, 0);
    }

    fn play_album(&mut self, artist_index: usize, album_index: usize) {
        let mut queue = Vec::new();

        if let Some(album) = self.album(artist_index, album_index) {
            for song_index in 0..album.songs.len() {
                queue.push(SongRef {
                    artist: artist_index,
                    album: album_index,
                    song: song_index,
                });
            }
        }

        self.start_queue(queue, 0);
    }

    fn play_song_scope(&mut self, artist_index: usize, album_index: usize, song_index: usize) {
        let mut queue = Vec::new();

        if let Some(album) = self.album(artist_index, album_index) {
            for index in 0..album.songs.len() {
                queue.push(SongRef {
                    artist: artist_index,
                    album: album_index,
                    song: index,
                });
            }
        }

        self.start_queue(queue, song_index);
    }

    fn open_album(&mut self, artist_index: usize, album_index: usize) {
        self.selection = Some(Selection::Album(artist_index, album_index));
        self.center_view = CenterView::Songs(artist_index, album_index);
        self.ensure_album_metadata(artist_index, album_index);
    }

    fn ensure_album_metadata(&mut self, artist_index: usize, album_index: usize) {
        let Some(album) = self.album(artist_index, album_index) else {
            return;
        };

        if album.metadata_loaded || self.pending_album_metadata == Some((artist_index, album_index))
        {
            return;
        }

        let album = album.clone();
        let (tx, rx) = mpsc::channel();
        self.album_metadata_rx = Some(rx);
        self.pending_album_metadata = Some((artist_index, album_index));
        thread::spawn(move || {
            let metadata = load_album_metadata(&album);
            let _ = tx.send(metadata);
        });
    }

    fn apply_album_metadata(
        &mut self,
        artist_index: usize,
        album_index: usize,
        metadata: AlbumMetadata,
    ) {
        let Some(artist) = self.artists.get_mut(artist_index) else {
            return;
        };
        let Some(album) = artist.albums.get_mut(album_index) else {
            return;
        };

        album.songs = metadata.songs;
        album.art = metadata.art;
        album.metadata_loaded = true;
        let album_name = album.name.clone();
        artist.art = artist.albums.iter().find_map(|album| album.art.clone());
        self.status = format!("Loaded metadata for {album_name}");
    }

    fn start_queue(&mut self, mut queue: Vec<SongRef>, requested_index: usize) {
        if queue.is_empty() {
            self.status = "No songs available".to_string();
            return;
        }

        let first = queue.get(requested_index).copied().unwrap_or(queue[0]);

        if self.shuffle {
            let mut rng = thread_rng();
            queue.shuffle(&mut rng);

            if let Some(position) = queue.iter().position(|song| *song == first) {
                queue.swap(0, position);
            }
        }

        let play_index = if self.shuffle {
            0
        } else {
            requested_index.min(queue.len().saturating_sub(1))
        };

        self.queue = queue;
        self.queue_index = play_index;
        self.play_queue_index(play_index);
    }

    fn play_queue_index(&mut self, index: usize) {
        let Some(song_ref) = self.queue.get(index).copied() else {
            return;
        };

        let Some((path, title)) = self
            .song(song_ref)
            .map(|song| (song.path.clone(), song.title.clone()))
        else {
            self.status = "Queued song is unavailable".to_string();
            return;
        };

        match self.player.play_file(
            &path,
            self.settings.eq_enabled,
            self.settings.eq_bands.clone(),
        ) {
            Ok(_) => {
                self.queue_index = index;
                self.now_playing = Some(song_ref);
                self.selection = Some(Selection::Song(
                    song_ref.artist,
                    song_ref.album,
                    song_ref.song,
                ));
                self.center_view = CenterView::Songs(song_ref.artist, song_ref.album);
                self.is_playing = true;
                self.status = format!("Playing {title}");
                self.ensure_lyrics_for_selection();
            }
            Err(err) => {
                self.is_playing = false;
                self.status = format!("Playback error: {err}");
            }
        }
    }

    fn play_next(&mut self) {
        if self.queue.is_empty() {
            self.play_selection();
            return;
        }

        let next = if self.queue_index + 1 < self.queue.len() {
            self.queue_index + 1
        } else {
            0
        };

        self.play_queue_index(next);
    }

    fn play_previous(&mut self) {
        if self.queue.is_empty() {
            self.play_selection();
            return;
        }

        let previous = if self.queue_index > 0 {
            self.queue_index - 1
        } else {
            self.queue.len() - 1
        };

        self.play_queue_index(previous);
    }

    fn toggle_pause(&mut self) {
        if self.player.is_paused() {
            self.player.resume();
            self.is_playing = true;
            self.status = "Playing".to_string();
        } else {
            self.player.pause();
            self.is_playing = false;
            self.status = "Paused".to_string();
        }
    }

    fn selected_song_context(&self) -> Option<(PathBuf, String, String)> {
        let song_ref = match self.now_playing {
            Some(song_ref) => song_ref,
            None => match self.selection {
                Some(Selection::Song(artist, album, song)) => SongRef {
                    artist,
                    album,
                    song,
                },
                _ => return None,
            },
        };

        let artist = self.artists.get(song_ref.artist)?;
        let song = self.song(song_ref)?;
        Some((song.path.clone(), artist.name.clone(), song.title.clone()))
    }

    fn ensure_lyrics_for_selection(&mut self) {
        let Some((song_path, artist, title)) = self.selected_song_context() else {
            self.lyrics_text = None;
            self.lyrics_status = "Select or play a song".to_string();
            return;
        };

        if self.pending_lyrics.as_ref() == Some(&song_path) {
            return;
        }

        let (tx, song_path_for_thread) = (self.lyrics_tx.clone(), song_path.clone());
        self.pending_lyrics = Some(song_path.clone());
        self.lyrics_status = "Loading lyrics".to_string();

        thread::spawn(move || {
            let result = load_cached_or_online_lyrics(&song_path_for_thread, &artist, &title);
            let _ = tx.send(result);
        });
    }

    fn load_local_lyrics_for_selection(&mut self) {
        let Some((song_path, artist, title)) = self.selected_song_context() else {
            self.lyrics_status = "Select or play a song before loading a lyrics file".to_string();
            return;
        };

        let lyrics_file = expand_home(self.settings.lyrics_file_path.trim());
        let tx = self.lyrics_tx.clone();
        self.pending_lyrics = Some(song_path.clone());
        self.lyrics_status = "Loading local lyrics".to_string();

        thread::spawn(move || {
            let result = match std::fs::read_to_string(&lyrics_file) {
                Ok(lyrics) => {
                    if let Ok(cache) = LibraryCache::open_default() {
                        let _ =
                            cache.upsert_lyrics(&song_path, &artist, &title, "local-file", &lyrics);
                    }
                    LyricsLoadResult {
                        song_path,
                        lyrics: Some(lyrics),
                        status: format!("Loaded lyrics from {}", lyrics_file.display()),
                    }
                }
                Err(err) => LyricsLoadResult {
                    song_path,
                    lyrics: None,
                    status: format!("Could not read lyrics file: {err}"),
                },
            };

            let _ = tx.send(result);
        });
    }

    fn current_song(&self) -> Option<&Song> {
        self.now_playing.and_then(|song_ref| self.song(song_ref))
    }

    fn current_album(&self) -> Option<&Album> {
        self.now_playing
            .and_then(|song_ref| self.album(song_ref.artist, song_ref.album))
    }

    fn selected_art(&self) -> Option<&ArtSource> {
        if let Some(song) = self.current_song() {
            return song
                .art
                .as_ref()
                .or_else(|| self.current_album().and_then(|album| album.art.as_ref()));
        }

        match self.selection {
            Some(Selection::Artist(artist)) => self
                .artists
                .get(artist)
                .and_then(|artist| artist.art.as_ref()),
            Some(Selection::Album(artist, album)) => self
                .album(artist, album)
                .and_then(|album| album.art.as_ref()),
            Some(Selection::Song(artist, album, song)) => self
                .song(SongRef {
                    artist,
                    album,
                    song,
                })
                .and_then(|song| song.art.as_ref())
                .or_else(|| {
                    self.album(artist, album)
                        .and_then(|album| album.art.as_ref())
                }),
            None => None,
        }
    }

    fn selected_title(&self) -> String {
        if let Some(song_ref) = self.now_playing {
            if let (Some(artist), Some(song)) =
                (self.artists.get(song_ref.artist), self.song(song_ref))
            {
                return format!("{} | {}", artist.name, song.title).replace(" | ", " | ");
            }

            if let Some(song) = self.song(song_ref) {
                return song.title.clone();
            }
        }

        match self.selection {
            Some(Selection::Artist(artist)) => self
                .artists
                .get(artist)
                .map(|artist| artist.name.clone())
                .unwrap_or_else(|| "artist | song name".to_string()),
            Some(Selection::Album(artist, album)) => {
                let artist_name = self
                    .artists
                    .get(artist)
                    .map(|artist| artist.name.as_str())
                    .unwrap_or("");
                let album = self
                    .album(artist, album)
                    .map(|album| album.name.as_str())
                    .unwrap_or("");
                format!("{artist_name} | {album}")
            }
            Some(Selection::Song(artist, album, song)) => {
                let artist_name = self
                    .artists
                    .get(artist)
                    .map(|artist| artist.name.as_str())
                    .unwrap_or("");
                let song_title = self
                    .song(SongRef {
                        artist,
                        album,
                        song,
                    })
                    .map(|song| song.title.as_str())
                    .unwrap_or("");
                format!("{artist_name} | {song_title}")
            }
            None => "artist | song name".to_string(),
        }
    }

    fn album(&self, artist_index: usize, album_index: usize) -> Option<&Album> {
        self.artists
            .get(artist_index)
            .and_then(|artist| artist.albums.get(album_index))
    }

    fn song(&self, song_ref: SongRef) -> Option<&Song> {
        self.album(song_ref.artist, song_ref.album)
            .and_then(|album| album.songs.get(song_ref.song))
    }

    fn background_color(&self) -> Color32 {
        rgba(self.settings.background_rgba)
    }

    fn panel_color(&self) -> Color32 {
        rgba(self.settings.panel_rgba)
    }

    fn header_color(&self) -> Color32 {
        rgba(self.settings.header_rgba)
    }

    fn font_color(&self) -> Color32 {
        rgba(self.settings.font_rgba)
    }

    fn accent_color(&self) -> Color32 {
        rgba(self.settings.accent_rgba)
    }

    fn window_rounding(&self) -> CornerRadius {
        CornerRadius::same(self.settings.window_rounding.clamp(0.0, 64.0) as u8)
    }

    fn image_rounding(&self, large: bool) -> CornerRadius {
        let rounding = if large {
            self.settings.image_rounding
        } else {
            self.settings.image_rounding.min(12.0)
        };
        CornerRadius::same(rounding.clamp(0.0, 64.0) as u8)
    }
}

impl eframe::App for MusicPlayerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.library_rx {
            if let Ok(artists) = rx.try_recv() {
                let artist_count = artists.len();
                let album_count: usize = artists.iter().map(|artist| artist.albums.len()).sum();
                let song_count: usize = artists
                    .iter()
                    .flat_map(|artist| &artist.albums)
                    .map(|album| album.songs.len())
                    .sum();

                self.artists = artists;
                self.library_rx = None;
                self.status = format!(
                    "Loaded {artist_count} artists, {album_count} albums, {song_count} songs"
                );
            }
        }

        if let Some(rx) = &self.album_metadata_rx {
            if let Ok(metadata) = rx.try_recv() {
                if let Some((artist, album)) = self.pending_album_metadata.take() {
                    self.apply_album_metadata(artist, album, metadata);
                }
                self.album_metadata_rx = None;
            }
        }

        while let Ok(result) = self.art_rx.try_recv() {
            let texture =
                ctx.load_texture(result.key.clone(), result.image, TextureOptions::LINEAR);
            self.pending_art.remove(&result.key);
            self.textures.insert(result.key, texture);
        }

        while let Ok(result) = self.lyrics_rx.try_recv() {
            if self.pending_lyrics.as_ref() == Some(&result.song_path) {
                self.pending_lyrics = None;
            }
            self.lyrics_text = result.lyrics;
            self.lyrics_status = result.status;
        }

        if self.settings != self.saved_settings && save_settings(&self.settings).is_ok() {
            self.saved_settings = self.settings.clone();
        }

        if self.is_playing && self.player.is_finished() {
            self.play_next();
        }

        self.player.set_volume(self.volume);
        apply_visuals(ctx, self.background_color());

        egui::CentralPanel::default()
            .frame(Frame::new().fill(self.background_color()))
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let bottom_height = 58.0;
                let outside = self.settings.outside_spacing.clamp(0.0, 80.0);
                let inside = self.settings.inside_spacing.clamp(0.0, 80.0);
                let content_rect = Rect::from_min_max(
                    rect.min + Vec2::splat(outside),
                    Pos2::new(
                        rect.max.x - outside,
                        rect.max.y - bottom_height - outside * 0.67,
                    ),
                );
                let bottom_rect = Rect::from_min_max(
                    Pos2::new(rect.min.x + outside, rect.max.y - bottom_height),
                    rect.max - Vec2::new(outside, outside * 0.67),
                );

                let left_width = 315.0;
                let right_width = 265.0;
                let left_rect = Rect::from_min_size(
                    content_rect.min,
                    Vec2::new(left_width, content_rect.height()),
                );
                let right_rect = Rect::from_min_size(
                    Pos2::new(content_rect.max.x - right_width, content_rect.min.y),
                    Vec2::new(right_width, content_rect.height()),
                );
                let center_rect = Rect::from_min_max(
                    Pos2::new(left_rect.max.x + inside, content_rect.min.y),
                    Pos2::new(right_rect.min.x - inside, content_rect.max.y),
                );

                self.panel(ui, left_rect, "Albums", "left_panel", |app, ui| {
                    app.artist_panel(ui)
                });
                self.panel(ui, center_rect, "", "center_panel", |app, ui| {
                    app.center_panel(ui)
                });
                self.side_panel(ui, right_rect);
                self.bottom_bar(ui, bottom_rect);
            });

        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

impl MusicPlayerApp {
    fn panel(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        title: &str,
        id_salt: &'static str,
        add_contents: impl FnOnce(&mut Self, &mut Ui),
    ) {
        ui.painter()
            .rect_filled(rect, self.window_rounding(), self.panel_color());

        let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 62.0));
        ui.painter()
            .rect_filled(header, self.window_rounding(), self.header_color());

        if !title.is_empty() {
            draw_marquee_text(
                ui,
                header.shrink2(Vec2::new(18.0, 0.0)),
                title,
                FontId::proportional(24.0),
                self.font_color(),
                Id::new(("panel_header", id_salt)),
            );
        }

        let body = rect.shrink2(Vec2::new(14.0, 74.0));
        ui.allocate_new_ui(UiBuilder::new().id_salt(id_salt).max_rect(body), |ui| {
            add_contents(self, ui);
        });
    }

    fn artist_panel(&mut self, ui: &mut Ui) {
        if self.library_rx.is_some() && self.artists.is_empty() {
            ui.label(
                RichText::new("Loading library...")
                    .size(18.0)
                    .color(self.font_color()),
            );
            return;
        }

        egui::ScrollArea::vertical()
            .id_salt("artist_list_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for artist_index in 0..self.artists.len() {
                    let selected = matches!(self.selection, Some(Selection::Artist(index)) if index == artist_index)
                        || matches!(self.center_view, CenterView::Albums(index) if index == artist_index);
                    let response = selectable_row(
                        ui,
                        &self.artists[artist_index].name,
                        selected,
                        self.font_color(),
                        self.accent_color(),
                    );

                    if response.clicked() {
                        self.selection = Some(Selection::Artist(artist_index));
                        self.center_view = CenterView::Albums(artist_index);
                    }

                    if response.double_clicked() {
                        self.selection = Some(Selection::Artist(artist_index));
                        self.center_view = CenterView::Albums(artist_index);
                        self.play_artist(artist_index);
                    }
                }
            });
    }

    fn center_panel(&mut self, ui: &mut Ui) {
        match self.center_view {
            CenterView::Artists => self.artist_overview(ui),
            CenterView::Albums(artist) => self.album_grid(ui, artist),
            CenterView::Songs(artist, album) => self.song_list(ui, artist, album),
            CenterView::Settings => self.settings_panel(ui),
        }
    }

    fn center_header(&mut self, ui: &mut Ui, title: &str, back_to: Option<CenterView>) {
        let rect = Rect::from_min_size(
            ui.max_rect().min - Vec2::new(14.0, 74.0),
            Vec2::new(ui.max_rect().width() + 28.0, 62.0),
        );

        if let Some(back_to) = back_to {
            let button_rect =
                Rect::from_min_size(rect.min + Vec2::new(14.0, 11.0), Vec2::splat(40.0));
            let response = ui.allocate_rect(button_rect, Sense::click());
            ui.painter()
                .rect_filled(button_rect, CornerRadius::same(20), self.panel_color());
            ui.painter().text(
                button_rect.center(),
                egui::Align2::CENTER_CENTER,
                "<",
                FontId::proportional(28.0),
                self.font_color(),
            );

            if response.clicked() {
                self.center_view = back_to;
            }
        }

        let settings_rect =
            Rect::from_min_size(rect.max - Vec2::new(54.0, 51.0), Vec2::splat(40.0));
        let settings_response = ui.allocate_rect(settings_rect, Sense::click());
        ui.painter()
            .rect_filled(settings_rect, CornerRadius::same(20), self.panel_color());
        ui.painter().text(
            settings_rect.center(),
            egui::Align2::CENTER_CENTER,
            "⚙",
            FontId::proportional(23.0),
            self.font_color(),
        );
        if settings_response.clicked() {
            self.center_view = CenterView::Settings;
        }

        let title_rect = if back_to.is_some() {
            Rect::from_min_max(
                Pos2::new(rect.min.x + 62.0, rect.min.y),
                Pos2::new(settings_rect.min.x - 10.0, rect.max.y),
            )
        } else {
            Rect::from_min_max(
                Pos2::new(rect.min.x + 18.0, rect.min.y),
                Pos2::new(settings_rect.min.x - 10.0, rect.max.y),
            )
        };
        draw_marquee_text(
            ui,
            title_rect,
            title,
            FontId::proportional(24.0),
            self.font_color(),
            Id::new(("center_header", title)),
        );
        ui.add_space(2.0);
    }

    fn artist_overview(&mut self, ui: &mut Ui) {
        self.center_header(ui, "dynamic window", None);
        let artists: Vec<(usize, String, Option<ArtSource>)> = self
            .artists
            .iter()
            .enumerate()
            .map(|(index, artist)| (index, artist.name.clone(), artist.art.clone()))
            .collect();
        self.thumbnail_grid(
            ui,
            &artists,
            |app, index| {
                app.selection = Some(Selection::Artist(index));
                app.center_view = CenterView::Albums(index);
            },
            |app, index| {
                app.selection = Some(Selection::Artist(index));
                app.center_view = CenterView::Albums(index);
                app.play_artist(index);
            },
        );
    }

    fn album_grid(&mut self, ui: &mut Ui, artist_index: usize) {
        let artist_name = self
            .artists
            .get(artist_index)
            .map(|artist| artist.name.clone())
            .unwrap_or_else(|| "Albums".to_string());
        self.center_header(ui, &artist_name, Some(CenterView::Artists));

        let albums: Vec<(usize, String, Option<ArtSource>)> = self
            .artists
            .get(artist_index)
            .map(|artist| {
                artist
                    .albums
                    .iter()
                    .enumerate()
                    .map(|(index, album)| (index, album.name.clone(), album.art.clone()))
                    .collect()
            })
            .unwrap_or_default();

        self.thumbnail_grid(
            ui,
            &albums,
            |app, album_index| {
                app.selection = Some(Selection::Album(artist_index, album_index));
            },
            |app, album_index| {
                app.open_album(artist_index, album_index);
            },
        );
    }

    fn song_list(&mut self, ui: &mut Ui, artist_index: usize, album_index: usize) {
        let title = self
            .album(artist_index, album_index)
            .map(|album| album.name.clone())
            .unwrap_or_else(|| "Songs".to_string());
        self.center_header(ui, &title, Some(CenterView::Albums(artist_index)));
        self.ensure_album_metadata(artist_index, album_index);

        let songs: Vec<(usize, String, Option<u32>)> = self
            .album(artist_index, album_index)
            .map(|album| {
                album
                    .songs
                    .iter()
                    .enumerate()
                    .map(|(index, song)| (index, song.title.clone(), song.track_number))
                    .collect()
            })
            .unwrap_or_default();

        egui::ScrollArea::vertical()
            .id_salt(("song_list_scroll", artist_index, album_index))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.pending_album_metadata == Some((artist_index, album_index)) {
                    ui.label(
                        RichText::new("Loading album metadata...")
                            .size(18.0)
                            .color(Color32::BLACK),
                    );
                }

                for (song_index, title, track) in songs {
                    let selected = matches!(
                        self.selection,
                        Some(Selection::Song(a, b, s)) if a == artist_index && b == album_index && s == song_index
                    );
                    let label = track
                        .map(|track| format!("{track:02}  {title}"))
                        .unwrap_or(title);
                    let response =
                        selectable_row(ui, &label, selected, self.font_color(), self.accent_color());

                    if response.clicked() {
                        self.selection =
                            Some(Selection::Song(artist_index, album_index, song_index));
                    }

                    if response.double_clicked() {
                        self.selection =
                            Some(Selection::Song(artist_index, album_index, song_index));
                        self.play_song_scope(artist_index, album_index, song_index);
                    }
                }
            });
    }

    fn settings_panel(&mut self, ui: &mut Ui) {
        self.center_header(ui, "Settings", Some(CenterView::Artists));

        egui::ScrollArea::vertical()
            .id_salt("settings_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(10.0, 10.0);

                ui.label(
                    RichText::new("Music libraries")
                        .size(20.0)
                        .color(self.font_color()),
                );

                let mut remove_index = None;
                for index in 0..self.settings.library_paths.len() {
                    ui.horizontal(|ui| {
                        let mut path = self.settings.library_paths[index]
                            .to_string_lossy()
                            .to_string();
                        if ui.text_edit_singleline(&mut path).changed() {
                            self.settings.library_paths[index] = PathBuf::from(path);
                        }

                        if ui.button("Remove").clicked() {
                            remove_index = Some(index);
                        }
                    });
                }

                if let Some(index) = remove_index {
                    self.settings.library_paths.remove(index);
                }

                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.settings.new_library_path);
                    if ui.button("Add").clicked() {
                        let path = self.settings.new_library_path.trim();
                        if !path.is_empty() {
                            self.settings.library_paths.push(expand_home(path));
                            self.settings.new_library_path.clear();
                        }
                    }
                });

                ui.horizontal(|ui| {
                    if ui.button("Reload libraries").clicked() {
                        self.start_library_scan(false);
                    }

                    if ui.button("Full cache rebuild").clicked() {
                        self.start_library_scan(true);
                    }
                });

                ui.separator();
                ui.label(RichText::new("Colors").size(20.0).color(self.font_color()));

                rgba_controls(ui, "Background", &mut self.settings.background_rgba);
                rgba_controls(ui, "Main windows", &mut self.settings.panel_rgba);
                rgba_controls(ui, "Headers", &mut self.settings.header_rgba);
                rgba_controls(ui, "Font", &mut self.settings.font_rgba);
                rgba_controls(ui, "Accent", &mut self.settings.accent_rgba);

                ui.separator();
                ui.label(
                    RichText::new("Shape and spacing")
                        .size(20.0)
                        .color(self.font_color()),
                );

                setting_slider(
                    ui,
                    "Image roundedness",
                    &mut self.settings.image_rounding,
                    0.0..=64.0,
                );
                setting_slider(
                    ui,
                    "Window roundedness",
                    &mut self.settings.window_rounding,
                    0.0..=64.0,
                );
                setting_slider(
                    ui,
                    "Inside spacing",
                    &mut self.settings.inside_spacing,
                    0.0..=80.0,
                );
                setting_slider(
                    ui,
                    "Outside spacing",
                    &mut self.settings.outside_spacing,
                    0.0..=80.0,
                );

                ui.separator();
                self.eq_settings(ui);

                ui.separator();
                ui.label(
                    RichText::new(&self.status)
                        .size(16.0)
                        .color(self.font_color()),
                );
            });
    }

    fn eq_settings(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new("Equalizer")
                .size(20.0)
                .color(self.font_color()),
        );
        ui.checkbox(&mut self.settings.eq_enabled, "Enable EQ");

        let mut remove_band = None;
        for index in 0..self.settings.eq_bands.len() {
            ui.horizontal(|ui| {
                ui.label(format!("Band {}", index + 1));
                ui.add(
                    egui::DragValue::new(&mut self.settings.eq_bands[index].frequency_hz)
                        .range(20.0..=20_000.0)
                        .speed(10.0)
                        .suffix(" Hz"),
                );
                ui.add(
                    egui::Slider::new(&mut self.settings.eq_bands[index].gain_db, -18.0..=18.0)
                        .text("gain dB"),
                );
                ui.add(
                    egui::DragValue::new(&mut self.settings.eq_bands[index].q)
                        .range(0.1..=10.0)
                        .speed(0.05)
                        .prefix("Q "),
                );

                if ui.button("Remove").clicked() {
                    remove_band = Some(index);
                }
            });
        }

        if let Some(index) = remove_band {
            self.settings.eq_bands.remove(index);
        }

        ui.horizontal(|ui| {
            if ui.button("Add frequency").clicked() {
                self.settings.eq_bands.push(EqBand::new(1000.0, 0.0, 1.0));
            }

            if ui.button("Reset bands").clicked() {
                self.settings.eq_bands = default_eq_bands();
            }
        });

        ui.add_space(8.0);
        ui.label(
            RichText::new("Profiles")
                .size(18.0)
                .color(self.font_color()),
        );
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.settings.eq_profile_name);
            if ui.button("Save profile").clicked() {
                let name = self.settings.eq_profile_name.trim();
                if !name.is_empty() {
                    let profile = EqProfile {
                        name: name.to_string(),
                        bands: self.settings.eq_bands.clone(),
                    };

                    if let Some(existing) = self
                        .settings
                        .eq_profiles
                        .iter_mut()
                        .find(|profile| profile.name == name)
                    {
                        *existing = profile;
                    } else {
                        self.settings.eq_profiles.push(profile);
                        self.settings.eq_selected_profile =
                            self.settings.eq_profiles.len().saturating_sub(1);
                    }
                }
            }
        });

        if !self.settings.eq_profiles.is_empty() {
            let selected = self
                .settings
                .eq_selected_profile
                .min(self.settings.eq_profiles.len().saturating_sub(1));
            self.settings.eq_selected_profile = selected;

            egui::ComboBox::from_label("Saved profiles")
                .selected_text(&self.settings.eq_profiles[selected].name)
                .show_ui(ui, |ui| {
                    for (index, profile) in self.settings.eq_profiles.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.settings.eq_selected_profile,
                            index,
                            &profile.name,
                        );
                    }
                });

            ui.horizontal(|ui| {
                if ui.button("Load profile").clicked() {
                    if let Some(profile) = self
                        .settings
                        .eq_profiles
                        .get(self.settings.eq_selected_profile)
                    {
                        self.settings.eq_bands = profile.bands.clone();
                        self.settings.eq_profile_name = profile.name.clone();
                    }
                }

                if ui.button("Delete profile").clicked()
                    && self.settings.eq_selected_profile < self.settings.eq_profiles.len()
                {
                    self.settings
                        .eq_profiles
                        .remove(self.settings.eq_selected_profile);
                    self.settings.eq_selected_profile = self
                        .settings
                        .eq_selected_profile
                        .min(self.settings.eq_profiles.len().saturating_sub(1));
                }
            });
        }

        ui.label(
            RichText::new("EQ changes apply when playback starts or the next track begins.")
                .size(14.0)
                .color(self.font_color()),
        );
    }

    fn thumbnail_grid(
        &mut self,
        ui: &mut Ui,
        items: &[(usize, String, Option<ArtSource>)],
        mut on_click: impl FnMut(&mut Self, usize),
        mut on_double_click: impl FnMut(&mut Self, usize),
    ) {
        egui::ScrollArea::vertical()
            .id_salt(("thumbnail_grid_scroll", self.center_view))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let tile_width = 145.0;
                let art_size = 118.0;
                let columns = (ui.available_width() / tile_width).floor().max(1.0) as usize;

                for row in items.chunks(columns) {
                    ui.horizontal(|ui| {
                        for (index, label, art) in row {
                            ui.vertical(|ui| {
                                let selected = match self.selection {
                                    Some(Selection::Artist(value)) => {
                                        *index == value
                                            && matches!(self.center_view, CenterView::Artists)
                                    }
                                    Some(Selection::Album(_, value)) => *index == value,
                                    _ => false,
                                };
                                let response = self.art_tile(
                                    ui,
                                    art.as_ref(),
                                    Vec2::splat(art_size),
                                    selected,
                                );

                                if response.clicked() {
                                    on_click(self, *index);
                                }

                                if response.double_clicked() {
                                    on_double_click(self, *index);
                                }

                                ui.add_sized(
                                    [tile_width - 12.0, 36.0],
                                    egui::Label::new(
                                        RichText::new(label).size(14.0).color(self.font_color()),
                                    )
                                    .wrap(),
                                );
                            });
                            ui.add_space(10.0);
                        }
                    });
                    ui.add_space(14.0);
                }
            });
    }

    fn side_panel(&mut self, ui: &mut Ui, rect: Rect) {
        let art_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 258.0));
        ui.painter()
            .rect_filled(art_rect, self.image_rounding(true), self.panel_color());

        let art_source = self.selected_art().cloned();
        let art_response = self.paint_art(ui, art_rect, art_source.as_ref(), true);
        if art_response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let lyrics_rect =
            Rect::from_min_max(Pos2::new(rect.min.x, art_rect.max.y + 18.0), rect.max);
        ui.painter()
            .rect_filled(lyrics_rect, self.window_rounding(), self.panel_color());

        let title_header =
            Rect::from_min_size(lyrics_rect.min, Vec2::new(lyrics_rect.width(), 62.0));
        ui.painter()
            .rect_filled(title_header, self.window_rounding(), self.header_color());

        let settings_rect =
            Rect::from_min_size(title_header.max - Vec2::new(47.0, 51.0), Vec2::splat(40.0));
        let settings_response = ui.allocate_rect(settings_rect, Sense::click());
        ui.painter()
            .rect_filled(settings_rect, CornerRadius::same(20), self.panel_color());
        ui.painter().text(
            settings_rect.center(),
            egui::Align2::CENTER_CENTER,
            "⚙",
            FontId::proportional(21.0),
            self.font_color(),
        );
        if settings_response.clicked() {
            self.lyrics_view = match self.lyrics_view {
                LyricsView::Lyrics => LyricsView::Settings,
                LyricsView::Settings => LyricsView::Lyrics,
            };
        }

        let selected_title = match self.lyrics_view {
            LyricsView::Lyrics => self.selected_title(),
            LyricsView::Settings => "Lyrics Settings".to_string(),
        };
        draw_marquee_text(
            ui,
            Rect::from_min_max(
                title_header.min + Vec2::new(8.0, 0.0),
                Pos2::new(settings_rect.min.x - 8.0, title_header.max.y),
            ),
            &selected_title,
            FontId::proportional(22.0),
            self.font_color(),
            Id::new(("side_title", selected_title.as_str())),
        );

        let body = Rect::from_min_max(
            Pos2::new(lyrics_rect.min.x + 12.0, title_header.max.y + 12.0),
            lyrics_rect.max - Vec2::new(12.0, 12.0),
        );
        ui.allocate_new_ui(
            UiBuilder::new().id_salt("lyrics_body").max_rect(body),
            |ui| match self.lyrics_view {
                LyricsView::Lyrics => self.lyrics_body(ui),
                LyricsView::Settings => self.lyrics_settings_body(ui),
            },
        );
    }

    fn lyrics_body(&mut self, ui: &mut Ui) {
        self.ensure_lyrics_for_selection();

        egui::ScrollArea::vertical()
            .id_salt("lyrics_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(lyrics) = &self.lyrics_text {
                    ui.label(RichText::new(lyrics).size(16.0).color(self.font_color()));
                } else {
                    ui.label(
                        RichText::new(&self.lyrics_status)
                            .size(16.0)
                            .color(self.font_color()),
                    );
                }
            });
    }

    fn lyrics_settings_body(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new("Local lyrics file")
                .size(18.0)
                .color(self.font_color()),
        );
        ui.text_edit_singleline(&mut self.settings.lyrics_file_path);

        ui.horizontal(|ui| {
            if ui.button("Load for current song").clicked() {
                self.load_local_lyrics_for_selection();
            }

            if ui.button("Back").clicked() {
                self.lyrics_view = LyricsView::Lyrics;
            }
        });

        ui.add_space(10.0);
        ui.label(
            RichText::new(&self.lyrics_status)
                .size(15.0)
                .color(self.font_color()),
        );
    }

    fn bottom_bar(&mut self, ui: &mut Ui, rect: Rect) {
        ui.painter()
            .rect_filled(rect, self.window_rounding(), self.panel_color());

        let controls_rect = Rect::from_center_size(
            rect.center(),
            Vec2::new((rect.width() - 32.0).max(0.0), 42.0),
        );

        ui.allocate_new_ui(
            UiBuilder::new()
                .id_salt("bottom_bar_controls")
                .max_rect(controls_rect),
            |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    if round_button(ui, "▶", self.font_color(), self.accent_color()).clicked() {
                        self.play_selection();
                    }

                    if round_button(ui, "Ⅱ", self.font_color(), self.accent_color()).clicked() {
                        self.toggle_pause();
                    }

                    if round_button(ui, "◀◀", self.font_color(), self.accent_color()).clicked()
                    {
                        self.play_previous();
                    }

                    if round_button(ui, "▶▶", self.font_color(), self.accent_color()).clicked()
                    {
                        self.play_next();
                    }

                    let shuffle_label = if self.shuffle { "⤨" } else { "⇄" };
                    if round_button(ui, shuffle_label, self.font_color(), self.accent_color())
                        .clicked()
                    {
                        self.shuffle = !self.shuffle;
                    }

                    ui.add_space(16.0);
                    self.scrub_bar(ui);
                    ui.add_space(10.0);
                    self.volume_bar(ui);
                });
            },
        );
    }

    fn scrub_bar(&mut self, ui: &mut Ui) {
        let desired = Vec2::new((ui.available_width() - 275.0).max(240.0), 42.0);
        let (rect, response) = ui.allocate_exact_size(desired, Sense::click_and_drag());
        let track = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 36.0));

        ui.painter()
            .rect_filled(track, CornerRadius::same(18), Color32::from_gray(110));

        let duration = self
            .player
            .duration()
            .map(|duration| duration.as_secs_f32())
            .unwrap_or(0.0);
        let position = self.player.position().as_secs_f32();
        let progress = if duration > 0.0 {
            (position / duration).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let filled = Rect::from_min_max(
            track.min,
            Pos2::new(track.min.x + track.width() * progress, track.max.y),
        );
        ui.painter().rect_filled(
            filled,
            CornerRadius::same(18),
            self.accent_color().gamma_multiply(0.85),
        );
        ui.painter().circle_filled(
            Pos2::new(track.min.x + track.width() * progress, track.center().y),
            20.0,
            self.accent_color(),
        );

        if (response.dragged() || response.clicked()) && duration > 0.0 {
            if let Some(pointer) = response.interact_pointer_pos() {
                let percent = ((pointer.x - track.min.x) / track.width()).clamp(0.0, 1.0);
                if let Err(err) = self
                    .player
                    .seek(Duration::from_secs_f32(duration * percent))
                {
                    self.status = format!("Seek error: {err}");
                }
            }
        }
    }

    fn volume_bar(&mut self, ui: &mut Ui) {
        let desired = Vec2::new(250.0, 42.0);
        let (rect, response) = ui.allocate_exact_size(desired, Sense::click_and_drag());
        let track = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 36.0));

        ui.painter()
            .rect_filled(track, CornerRadius::same(18), Color32::from_gray(110));

        let filled = Rect::from_min_max(
            track.min,
            Pos2::new(track.min.x + track.width() * self.volume, track.max.y),
        );
        ui.painter().rect_filled(
            filled,
            CornerRadius::same(18),
            self.accent_color().gamma_multiply(0.85),
        );
        ui.painter().circle_filled(
            Pos2::new(track.min.x + track.width() * self.volume, track.center().y),
            20.0,
            self.accent_color(),
        );

        if response.dragged() || response.clicked() {
            if let Some(pointer) = response.interact_pointer_pos() {
                self.volume = ((pointer.x - track.min.x) / track.width()).clamp(0.0, 1.0);
                self.player.set_volume(self.volume);
            }
        }
    }

    fn art_tile(
        &mut self,
        ui: &mut Ui,
        art: Option<&ArtSource>,
        size: Vec2,
        selected: bool,
    ) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        self.paint_art(ui, rect, art, false);

        if selected {
            ui.painter().rect_stroke(
                rect.expand(3.0),
                self.image_rounding(false),
                Stroke::new(3.0, self.accent_color()),
                egui::StrokeKind::Outside,
            );
        }

        response
    }

    fn paint_art(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        art: Option<&ArtSource>,
        large: bool,
    ) -> egui::Response {
        let response = ui.interact(
            rect,
            Id::new(("art", rect.min.x.to_bits(), rect.min.y.to_bits())),
            Sense::hover(),
        );

        if let Some((texture_id, _texture_size)) = art
            .and_then(|art| self.texture_for_art(ui.ctx(), art))
            .map(|texture| (texture.id(), texture.size_vec2()))
        {
            Image::from_texture((texture_id, rect.size()))
                .fit_to_exact_size(rect.size())
                .corner_radius(self.image_rounding(large))
                .paint_at(ui, rect);
        } else {
            paint_placeholder(ui, rect, large, self.image_rounding(large));
        }

        response
    }

    fn texture_for_art(&mut self, ctx: &egui::Context, art: &ArtSource) -> Option<&TextureHandle> {
        let key = art_key(art);

        if !self.textures.contains_key(&key) && self.pending_art.insert(key.clone()) {
            let art = art.clone();
            let tx = self.art_tx.clone();
            let ctx = ctx.clone();
            thread::spawn(move || {
                let Some(image) = load_art_image(&art) else {
                    return;
                };

                let _ = tx.send(ArtLoadResult {
                    key: art_key(&art),
                    image,
                });
                ctx.request_repaint();
            });
        }

        self.textures.get(&key)
    }
}

fn selectable_row(
    ui: &mut Ui,
    label: &str,
    selected: bool,
    font_color: Color32,
    accent_color: Color32,
) -> egui::Response {
    let height = 34.0;
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    let fill = if selected {
        accent_color
    } else if response.hovered() {
        Color32::from_gray(164)
    } else {
        Color32::TRANSPARENT
    };

    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    ui.painter().text(
        rect.left_center() + Vec2::new(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        truncate_text(label, 36),
        FontId::proportional(18.0),
        font_color,
    );
    response
}

fn round_button(
    ui: &mut Ui,
    label: &str,
    font_color: Color32,
    accent_color: Color32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(42.0), Sense::click());
    let fill = if response.hovered() {
        accent_color.gamma_multiply(1.15)
    } else {
        accent_color
    };
    ui.painter().circle_filled(rect.center(), 21.0, fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(22.0),
        font_color,
    );
    response
}

fn rgba_controls(ui: &mut Ui, label: &str, rgba: &mut [u8; 4]) {
    ui.horizontal(|ui| {
        ui.add_sized([130.0, 20.0], egui::Label::new(label));
        for (channel, name) in rgba.iter_mut().zip(["R", "G", "B", "A"]) {
            ui.label(name);
            ui.add(egui::DragValue::new(channel).range(0..=255).speed(1));
        }
    });
}

fn setting_slider(ui: &mut Ui, label: &str, value: &mut f32, range: RangeInclusive<f32>) {
    ui.horizontal(|ui| {
        ui.add_sized([160.0, 20.0], egui::Label::new(label));
        ui.add(egui::Slider::new(value, range).show_value(true));
    });
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(stripped);
        }
    }

    PathBuf::from(path)
}

fn settings_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("rust-music-player")
        .join("settings.json")
}

fn load_settings() -> UiSettings {
    let path = settings_path();
    let Ok(contents) = std::fs::read_to_string(path) else {
        return UiSettings::default();
    };

    serde_json::from_str(&contents).unwrap_or_default()
}

fn save_settings(settings: &UiSettings) -> anyhow::Result<()> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let contents = serde_json::to_string_pretty(settings)?;
    std::fs::write(path, contents)?;
    Ok(())
}

fn load_art_image(art: &ArtSource) -> Option<ColorImage> {
    let bytes = art_bytes(art)?;
    let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let pixels = image.into_raw();
    Some(ColorImage::from_rgba_unmultiplied(size, &pixels))
}

fn load_cached_or_online_lyrics(
    song_path: &PathBuf,
    artist: &str,
    title: &str,
) -> LyricsLoadResult {
    if let Ok(cache) = LibraryCache::open_default() {
        if let Ok(Some(lyrics)) = cache.get_lyrics(song_path) {
            return LyricsLoadResult {
                song_path: song_path.clone(),
                lyrics: Some(lyrics),
                status: "Loaded cached lyrics".to_string(),
            };
        }
    }

    match fetch_lrclib_lyrics(artist, title) {
        Ok(lyrics) => {
            if let Ok(cache) = LibraryCache::open_default() {
                let _ = cache.upsert_lyrics(song_path, artist, title, "lrclib", &lyrics);
            }

            LyricsLoadResult {
                song_path: song_path.clone(),
                lyrics: Some(lyrics),
                status: "Loaded lyrics from LRCLIB".to_string(),
            }
        }
        Err(err) => LyricsLoadResult {
            song_path: song_path.clone(),
            lyrics: None,
            status: format!("Lyrics unavailable: {err}"),
        },
    }
}

#[derive(Debug, Deserialize)]
struct LrclibLyrics {
    #[serde(rename = "plainLyrics")]
    plain_lyrics: Option<String>,
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
}

fn fetch_lrclib_lyrics(artist: &str, title: &str) -> anyhow::Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("rust-music-player/0.1 (local desktop player)")
        .timeout(Duration::from_secs(12))
        .build()?;
    let results = client
        .get("https://lrclib.net/api/search")
        .query(&[("artist_name", artist), ("track_name", title)])
        .send()?
        .error_for_status()?
        .json::<Vec<LrclibLyrics>>()?;

    results
        .into_iter()
        .find_map(|lyrics| lyrics.synced_lyrics.or(lyrics.plain_lyrics))
        .filter(|lyrics| !lyrics.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("no match found"))
}

fn rgba(rgba: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3])
}

fn default_eq_bands() -> Vec<EqBand> {
    [60.0, 170.0, 310.0, 600.0, 1000.0, 3000.0, 6000.0, 12_000.0]
        .into_iter()
        .map(|frequency| EqBand::new(frequency, 0.0, 1.0))
        .collect()
}

fn draw_marquee_text(ui: &Ui, rect: Rect, text: &str, font: FontId, color: Color32, _id: Id) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let text_size = galley.size();
    let painter = ui.painter().with_clip_rect(rect);

    if text_size.x <= rect.width() {
        painter.galley(
            Pos2::new(
                rect.center().x - text_size.x / 2.0,
                rect.center().y - text_size.y / 2.0,
            ),
            galley,
            color,
        );
        return;
    }

    ui.ctx().request_repaint_after(Duration::from_millis(16));

    let time = ui.input(|input| input.time);
    let overflow = text_size.x - rect.width();
    let pause = 1.0;
    let travel = overflow + 36.0;
    let period = pause * 2.0 + f64::from(travel / 42.0);
    let phase = time.rem_euclid(period);
    let offset = if phase < pause {
        0.0
    } else if phase > period - pause {
        overflow
    } else {
        let t = ((phase - pause) / (period - pause * 2.0)) as f32;
        overflow * smoothstep(t)
    };

    painter.galley(
        Pos2::new(rect.left() - offset, rect.center().y - text_size.y / 2.0),
        galley,
        color,
    );
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn paint_placeholder(ui: &mut Ui, rect: Rect, large: bool, rounding: CornerRadius) {
    let steps = if large { 28 } else { 14 };
    for step in 0..steps {
        let t = step as f32 / steps as f32;
        let gray = (178.0 - 75.0 * t) as u8;
        let y0 = egui::lerp(rect.y_range(), t);
        let y1 = egui::lerp(rect.y_range(), (step + 1) as f32 / steps as f32);
        let band = Rect::from_min_max(Pos2::new(rect.min.x, y0), Pos2::new(rect.max.x, y1));
        ui.painter()
            .rect_filled(band, rounding, Color32::from_gray(gray));
    }
}

fn art_key(art: &ArtSource) -> String {
    match art {
        ArtSource::Embedded(path) => format!("embedded:{}", path.display()),
        ArtSource::File(path) => format!("file:{}", path.display()),
        ArtSource::Folder(path) => format!("folder:{}", path.display()),
    }
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    let mut truncated = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

fn apply_visuals(ctx: &egui::Context, background: Color32) {
    let mut visuals = egui::Visuals::dark();
    visuals.widgets.noninteractive.bg_fill = background;
    visuals.panel_fill = background;
    ctx.set_visuals(visuals);
}
