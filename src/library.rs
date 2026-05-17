use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use lofty::{prelude::*, read_from_path};
use walkdir::WalkDir;

use crate::database::{CachedSong, LibraryCache};

#[derive(Debug, Clone)]
pub enum ArtSource {
    Embedded(PathBuf),
    File(PathBuf),
    Folder(PathBuf),
}

#[derive(Debug, Clone)]
pub struct Song {
    pub title: String,
    pub path: PathBuf,
    pub track_number: Option<u32>,
    pub art: Option<ArtSource>,
    pub modified_secs: i64,
    pub file_size: i64,
}

#[derive(Debug, Clone)]
pub struct Album {
    pub name: String,
    pub path: PathBuf,
    pub songs: Vec<Song>,
    pub art: Option<ArtSource>,
    pub metadata_loaded: bool,
}

#[derive(Debug, Clone)]
pub struct Artist {
    pub name: String,
    pub path: PathBuf,
    pub albums: Vec<Album>,
    pub art: Option<ArtSource>,
}

#[derive(Debug, Clone)]
pub struct AlbumMetadata {
    pub songs: Vec<Song>,
    pub art: Option<ArtSource>,
}

#[derive(Debug, Clone, Default)]
struct SongMetadata {
    artist: Option<String>,
    album: Option<String>,
    title: Option<String>,
    track_number: Option<u32>,
    has_embedded_art: bool,
}

pub fn scan_library(root_folder: &Path) -> Vec<Artist> {
    let cached_songs = LibraryCache::open_default()
        .and_then(|cache| cache.load_songs())
        .unwrap_or_default();
    let mut grouped: BTreeMap<(String, String), AlbumBuild> = BTreeMap::new();
    let mut artist_paths: BTreeMap<String, PathBuf> = BTreeMap::new();

    for entry in WalkDir::new(root_folder)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
    {
        let path = entry.path();

        if !is_audio_file(path) {
            continue;
        }

        let file_info = file_info(path);
        let folder_guess = folder_guess(root_folder, path);
        let cached = fresh_cached_song(&cached_songs, path, file_info);

        let artist_name = cached
            .as_ref()
            .map(|song| song.artist.clone())
            .unwrap_or_else(|| folder_guess.artist.clone());
        let album_name = cached
            .as_ref()
            .map(|song| song.album.clone())
            .unwrap_or_else(|| folder_guess.album.clone());
        let title = cached
            .as_ref()
            .map(|song| song.title.clone())
            .unwrap_or_else(|| folder_guess.title.clone());

        let art = cached.as_ref().and_then(|song| {
            song.art_path.clone().map(ArtSource::File).or_else(|| {
                song.has_embedded_art
                    .then(|| ArtSource::Embedded(path.to_path_buf()))
            })
        });

        let song = Song {
            title,
            path: path.to_path_buf(),
            track_number: cached.as_ref().and_then(|song| song.track_number),
            art,
            modified_secs: file_info.modified_secs,
            file_size: file_info.file_size,
        };

        let key = (artist_name.clone(), album_name.clone());
        artist_paths
            .entry(artist_name)
            .or_insert_with(|| folder_guess.artist_path);
        grouped
            .entry(key)
            .or_insert_with(|| AlbumBuild {
                path: folder_guess.album_path,
                songs: Vec::new(),
            })
            .songs
            .push(song);
    }

    build_artists(grouped, artist_paths)
}

pub fn scan_libraries(root_folders: &[PathBuf]) -> Vec<Artist> {
    let mut grouped: BTreeMap<(String, String), AlbumBuild> = BTreeMap::new();
    let mut artist_paths: BTreeMap<String, PathBuf> = BTreeMap::new();

    for root_folder in root_folders {
        let artists = scan_library(root_folder);
        for artist in artists {
            artist_paths
                .entry(artist.name.clone())
                .or_insert_with(|| artist.path.clone());

            for album in artist.albums {
                grouped
                    .entry((artist.name.clone(), album.name.clone()))
                    .or_insert_with(|| AlbumBuild {
                        path: album.path.clone(),
                        songs: Vec::new(),
                    })
                    .songs
                    .extend(album.songs);
            }
        }
    }

    build_artists(grouped, artist_paths)
}

pub fn rebuild_full_cache(root_folders: &[PathBuf]) -> Vec<Artist> {
    let mut artists = scan_libraries(root_folders);

    for artist in &mut artists {
        for album in &mut artist.albums {
            let metadata = load_album_metadata(album);
            album.songs = metadata.songs;
            album.art = metadata.art;
            album.metadata_loaded = true;
        }

        artist.art = artist
            .albums
            .iter()
            .find_map(|album| album.art.clone())
            .or_else(|| nearby_art(&artist.path));
    }

    artists
}

pub fn load_album_metadata(album: &Album) -> AlbumMetadata {
    let cache = LibraryCache::open_default().ok();
    let mut songs = Vec::with_capacity(album.songs.len());

    for song in &album.songs {
        let metadata = read_metadata(&song.path);
        let title = metadata
            .title
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| song.title.clone());
        let artist_name = metadata
            .artist
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                album
                    .path
                    .parent()
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str())
                    .unwrap_or("Unknown Artist")
                    .to_string()
            });
        let album_name = metadata
            .album
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| album.name.clone());
        let nearby_art = nearby_art(&album.path);
        let art = nearby_art.clone().or_else(|| {
            metadata
                .has_embedded_art
                .then(|| ArtSource::Embedded(song.path.clone()))
        });

        let loaded_song = Song {
            title: title.clone(),
            path: song.path.clone(),
            track_number: metadata.track_number,
            art: art.clone(),
            modified_secs: song.modified_secs,
            file_size: song.file_size,
        };

        if let Some(cache) = &cache {
            let cached_song = CachedSong {
                path: song.path.clone(),
                modified_secs: song.modified_secs,
                file_size: song.file_size,
                artist: artist_name,
                album: album_name,
                title,
                track_number: metadata.track_number,
                art_path: match &nearby_art {
                    Some(ArtSource::File(path)) => Some(path.clone()),
                    _ => None,
                },
                has_embedded_art: metadata.has_embedded_art,
            };
            let _ = cache.upsert_song(&cached_song);
        }

        songs.push(loaded_song);
    }

    songs.sort_by(compare_songs);
    let art = songs
        .iter()
        .find_map(|song| song.art.clone())
        .or_else(|| nearby_art(&album.path));

    AlbumMetadata { songs, art }
}

struct AlbumBuild {
    path: PathBuf,
    songs: Vec<Song>,
}

#[derive(Debug, Clone, Copy)]
struct FileInfo {
    modified_secs: i64,
    file_size: i64,
}

struct FolderGuess {
    artist: String,
    album: String,
    title: String,
    artist_path: PathBuf,
    album_path: PathBuf,
}

fn build_artists(
    grouped: BTreeMap<(String, String), AlbumBuild>,
    artist_paths: BTreeMap<String, PathBuf>,
) -> Vec<Artist> {
    let mut artists: BTreeMap<String, Artist> = BTreeMap::new();

    for ((artist_name, album_name), mut build) in grouped {
        build.songs.sort_by(compare_songs);
        let album_art = build
            .songs
            .iter()
            .find_map(|song| song.art.clone())
            .or_else(|| Some(ArtSource::Folder(build.path.clone())));

        let album = Album {
            name: album_name,
            path: build.path,
            songs: build.songs,
            art: album_art,
            metadata_loaded: false,
        };

        let artist_path = artist_paths
            .get(&artist_name)
            .cloned()
            .unwrap_or_else(|| PathBuf::from("/home/geesies/Music"));

        artists
            .entry(artist_name.clone())
            .or_insert_with(|| Artist {
                name: artist_name,
                path: artist_path,
                albums: Vec::new(),
                art: None,
            })
            .albums
            .push(album);
    }

    let mut artists: Vec<Artist> = artists.into_values().collect();

    for artist in &mut artists {
        artist
            .albums
            .sort_by(|a, b| alpha_key(&a.name).cmp(&alpha_key(&b.name)));
        artist.art = artist
            .albums
            .iter()
            .find_map(|album| album.art.clone())
            .or_else(|| Some(ArtSource::Folder(artist.path.clone())));
    }

    artists.sort_by(|a, b| compare_artist_names(&a.name, &b.name));
    artists
}

fn fresh_cached_song(
    cached_songs: &HashMap<PathBuf, CachedSong>,
    path: &Path,
    file_info: FileInfo,
) -> Option<CachedSong> {
    let song = cached_songs.get(path)?;

    (song.modified_secs == file_info.modified_secs && song.file_size == file_info.file_size)
        .then(|| song.clone())
}

fn folder_guess(root_folder: &Path, song_path: &Path) -> FolderGuess {
    let relative = song_path.strip_prefix(root_folder).unwrap_or(song_path);
    let components: Vec<String> = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(ToOwned::to_owned))
        .collect();

    match components.as_slice() {
        [file] => {
            let (artist, title) = split_single_filename(file);
            FolderGuess {
                artist,
                album: "Singles".to_string(),
                title,
                artist_path: root_folder.to_path_buf(),
                album_path: root_folder.to_path_buf(),
            }
        }
        [artist, file] => FolderGuess {
            artist: artist.clone(),
            album: "Singles".to_string(),
            title: file_stem(file),
            artist_path: root_folder.join(artist),
            album_path: root_folder.join(artist),
        },
        [artist, album, rest @ ..] => {
            let album_path = if rest.len() > 1 && is_quality_folder(album) {
                root_folder.join(artist)
            } else {
                root_folder.join(artist).join(album)
            };

            FolderGuess {
                artist: artist.clone(),
                album: album.clone(),
                title: rest.last().map(|file| file_stem(file)).unwrap_or_else(|| {
                    song_path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or("Unknown Track")
                        .to_string()
                }),
                artist_path: root_folder.join(artist),
                album_path,
            }
        }
        [] => FolderGuess {
            artist: "Unknown Artist".to_string(),
            album: "Singles".to_string(),
            title: "Unknown Track".to_string(),
            artist_path: root_folder.to_path_buf(),
            album_path: root_folder.to_path_buf(),
        },
    }
}

fn read_metadata(path: &Path) -> SongMetadata {
    let Ok(tagged_file) = read_from_path(path) else {
        return SongMetadata::default();
    };

    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag());

    SongMetadata {
        artist: tag.and_then(|tag| tag.artist().map(|value| value.to_string())),
        album: tag.and_then(|tag| tag.album().map(|value| value.to_string())),
        title: tag.and_then(|tag| tag.title().map(|value| value.to_string())),
        track_number: tag.and_then(|tag| tag.track()),
        has_embedded_art: tagged_file
            .tags()
            .iter()
            .any(|tag| !tag.pictures().is_empty()),
    }
}

pub fn embedded_art_bytes(path: &Path) -> Option<Vec<u8>> {
    let tagged_file = read_from_path(path).ok()?;
    tagged_file
        .tags()
        .iter()
        .flat_map(|tag| tag.pictures())
        .next()
        .map(|picture| picture.data().to_vec())
}

pub fn art_bytes(art: &ArtSource) -> Option<Vec<u8>> {
    match art {
        ArtSource::Embedded(path) => embedded_art_bytes(path),
        ArtSource::File(path) => fs::read(path).ok(),
        ArtSource::Folder(path) => nearby_art(path).and_then(|art| art_bytes(&art)),
    }
}

fn compare_artist_names(a: &str, b: &str) -> Ordering {
    let a_ascii = starts_with_ascii_letter(a);
    let b_ascii = starts_with_ascii_letter(b);

    match (a_ascii, b_ascii) {
        (false, true) => Ordering::Less,
        (true, false) => Ordering::Greater,
        _ => alpha_key(a).cmp(&alpha_key(b)),
    }
}

fn compare_songs(a: &Song, b: &Song) -> Ordering {
    match (a.track_number, b.track_number) {
        (Some(left), Some(right)) => left
            .cmp(&right)
            .then_with(|| alpha_key(&a.title).cmp(&alpha_key(&b.title))),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => alpha_key(&a.title).cmp(&alpha_key(&b.title)),
    }
}

fn starts_with_ascii_letter(value: &str) -> bool {
    value
        .chars()
        .find(|character| !character.is_whitespace())
        .is_some_and(|character| character.is_ascii_alphabetic())
}

fn alpha_key(value: &str) -> String {
    value.to_lowercase()
}

fn nearby_art(folder: &Path) -> Option<ArtSource> {
    let names = [
        "cover.jpg",
        "cover.jpeg",
        "cover.png",
        "folder.jpg",
        "folder.png",
        "artist.jpg",
        "artist.png",
    ];

    for name in names {
        let candidate = folder.join(name);
        if candidate.is_file() {
            return Some(ArtSource::File(candidate));
        }
    }

    let Ok(entries) = fs::read_dir(folder) else {
        return None;
    };

    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| is_image_file(path))
        .map(ArtSource::File)
}

fn file_info(path: &Path) -> FileInfo {
    let Ok(metadata) = fs::metadata(path) else {
        return FileInfo {
            modified_secs: 0,
            file_size: 0,
        };
    };

    let modified_secs = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);

    FileInfo {
        modified_secs,
        file_size: metadata.len() as i64,
    }
}

fn is_quality_folder(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("flac") || lower.contains("bit") || lower.contains("khz")
}

fn split_single_filename(file_name: &str) -> (String, String) {
    let stem = file_stem(file_name);

    if let Some((artist, title)) = stem.split_once(" - ") {
        (artist.trim().to_string(), title.trim().to_string())
    } else {
        ("Unknown Artist".to_string(), stem)
    }
}

fn file_stem(file_name: &str) -> String {
    Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(file_name)
        .to_string()
}

fn is_audio_file(path: &Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => matches!(
            ext.to_lowercase().as_str(),
            "mp3" | "flac" | "wav" | "ogg" | "m4a"
        ),
        None => false,
    }
}

fn is_image_file(path: &Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => matches!(ext.to_lowercase().as_str(), "jpg" | "jpeg" | "png"),
        None => false,
    }
}
