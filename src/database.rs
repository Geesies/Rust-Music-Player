use std::{collections::HashMap, path::PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone)]
pub struct CachedSong {
    pub path: PathBuf,
    pub modified_secs: i64,
    pub file_size: i64,
    pub artist: String,
    pub album: String,
    pub title: String,
    pub track_number: Option<u32>,
    pub art_path: Option<PathBuf>,
    pub has_embedded_art: bool,
}

pub struct LibraryCache {
    connection: Connection,
}

impl LibraryCache {
    pub fn open_default() -> anyhow::Result<Self> {
        let cache_path = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".cache")
            })
            .join("rust-music-player")
            .join("library.sqlite");

        if let Some(parent) = cache_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let connection = Connection::open(cache_path)?;
        connection.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS songs (
                path TEXT PRIMARY KEY,
                modified_secs INTEGER NOT NULL,
                file_size INTEGER NOT NULL,
                artist TEXT NOT NULL,
                album TEXT NOT NULL,
                title TEXT NOT NULL,
                track_number INTEGER,
                art_path TEXT,
                has_embedded_art INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS lyrics (
                song_path TEXT PRIMARY KEY,
                artist TEXT NOT NULL,
                title TEXT NOT NULL,
                source TEXT NOT NULL,
                lyrics TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            ",
        )?;

        Ok(Self { connection })
    }

    pub fn load_songs(&self) -> anyhow::Result<HashMap<PathBuf, CachedSong>> {
        let mut statement = self.connection.prepare(
            "
            SELECT path, modified_secs, file_size, artist, album, title, track_number, art_path, has_embedded_art
            FROM songs
            ",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(CachedSong {
                path: PathBuf::from(row.get::<_, String>(0)?),
                modified_secs: row.get(1)?,
                file_size: row.get(2)?,
                artist: row.get(3)?,
                album: row.get(4)?,
                title: row.get(5)?,
                track_number: row.get::<_, Option<u32>>(6)?,
                art_path: row.get::<_, Option<String>>(7)?.map(PathBuf::from),
                has_embedded_art: row.get::<_, i64>(8)? != 0,
            })
        })?;

        let mut songs = HashMap::new();
        for row in rows {
            let song = row?;
            songs.insert(song.path.clone(), song);
        }

        Ok(songs)
    }

    pub fn upsert_song(&self, song: &CachedSong) -> anyhow::Result<()> {
        let path = song.path.to_string_lossy();
        let art_path = song.art_path.as_ref().map(|path| path.to_string_lossy());
        let track_number = song.track_number.map(i64::from);

        self.connection.execute(
            "
            INSERT INTO songs (
                path, modified_secs, file_size, artist, album, title, track_number, art_path, has_embedded_art
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(path) DO UPDATE SET
                modified_secs = excluded.modified_secs,
                file_size = excluded.file_size,
                artist = excluded.artist,
                album = excluded.album,
                title = excluded.title,
                track_number = excluded.track_number,
                art_path = excluded.art_path,
                has_embedded_art = excluded.has_embedded_art
            ",
            params![
                path.as_ref(),
                song.modified_secs,
                song.file_size,
                song.artist,
                song.album,
                song.title,
                track_number,
                art_path.as_deref(),
                if song.has_embedded_art { 1 } else { 0 },
            ],
        )?;

        Ok(())
    }

    pub fn get_lyrics(&self, song_path: &PathBuf) -> anyhow::Result<Option<String>> {
        let song_path = song_path.to_string_lossy();

        self.connection
            .query_row(
                "SELECT lyrics FROM lyrics WHERE song_path = ?1",
                params![song_path.as_ref()],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn upsert_lyrics(
        &self,
        song_path: &PathBuf,
        artist: &str,
        title: &str,
        source: &str,
        lyrics: &str,
    ) -> anyhow::Result<()> {
        let song_path = song_path.to_string_lossy();
        let updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);

        self.connection.execute(
            "
            INSERT INTO lyrics (song_path, artist, title, source, lyrics, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(song_path) DO UPDATE SET
                artist = excluded.artist,
                title = excluded.title,
                source = excluded.source,
                lyrics = excluded.lyrics,
                updated_at = excluded.updated_at
            ",
            params![
                song_path.as_ref(),
                artist,
                title,
                source,
                lyrics,
                updated_at
            ],
        )?;

        Ok(())
    }
}
