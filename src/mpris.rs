use std::{
    collections::HashMap,
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use zbus::{blocking::connection::Builder as ConnectionBuilder, interface};
use zvariant::{ObjectPath, OwnedValue, Str, Value};

const BUS_NAME: &str = "org.mpris.MediaPlayer2.rust_music_player";
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const TRACK_ID: &str = "/org/mpris/MediaPlayer2/Track/Current";

#[derive(Debug, Clone)]
pub enum MediaCommand {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    SeekRelative(i64),
    SeekAbsolute(Duration),
}

#[derive(Clone)]
pub struct MprisHandle {
    state: Arc<Mutex<MprisState>>,
    update_tx: Sender<MprisState>,
}

impl MprisHandle {
    pub fn update(&self, state: MprisState) {
        if let Ok(mut current) = self.state.lock() {
            *current = state.clone();
        }
        let _ = self.update_tx.send(state);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MprisState {
    pub playback_status: PlaybackStatus,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: Option<String>,
    pub duration: Option<Duration>,
    pub position: Duration,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_seek: bool,
}

impl Default for MprisState {
    fn default() -> Self {
        Self {
            playback_status: PlaybackStatus::Stopped,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            art_url: None,
            duration: None,
            position: Duration::ZERO,
            can_play: true,
            can_pause: false,
            can_seek: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

pub fn start(command_tx: Sender<MediaCommand>) -> MprisHandle {
    let state = Arc::new(Mutex::new(MprisState::default()));
    let (update_tx, update_rx) = mpsc::channel();
    let handle = MprisHandle {
        state: state.clone(),
        update_tx,
    };

    thread::spawn(move || {
        if let Err(err) = run_service(state, command_tx, update_rx) {
            eprintln!("MPRIS service unavailable: {err}");
        }
    });

    handle
}

fn run_service(
    state: Arc<Mutex<MprisState>>,
    command_tx: Sender<MediaCommand>,
    update_rx: Receiver<MprisState>,
) -> zbus::Result<()> {
    let root = RootInterface {
        command_tx: command_tx.clone(),
    };
    let player = PlayerInterface { state, command_tx };
    let connection = ConnectionBuilder::session()?
        .serve_at(OBJECT_PATH, root)?
        .serve_at(OBJECT_PATH, player)?
        .name(BUS_NAME)?
        .build()?;
    let mut last_signalled = MprisSignalState::from(&MprisState::default());

    loop {
        match update_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(mut latest) => {
                while let Ok(state) = update_rx.try_recv() {
                    latest = state;
                }

                let signal_state = MprisSignalState::from(&latest);
                if signal_state != last_signalled {
                    emit_player_properties_changed(&connection, &latest)?;
                    last_signalled = signal_state;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MprisSignalState {
    playback_status: PlaybackStatus,
    title: String,
    artist: String,
    album: String,
    art_url: Option<String>,
    duration: Option<Duration>,
    can_play: bool,
    can_pause: bool,
    can_seek: bool,
}

impl From<&MprisState> for MprisSignalState {
    fn from(state: &MprisState) -> Self {
        Self {
            playback_status: state.playback_status,
            title: state.title.clone(),
            artist: state.artist.clone(),
            album: state.album.clone(),
            art_url: state.art_url.clone(),
            duration: state.duration,
            can_play: state.can_play,
            can_pause: state.can_pause,
            can_seek: state.can_seek,
        }
    }
}

fn emit_player_properties_changed(
    connection: &zbus::blocking::Connection,
    state: &MprisState,
) -> zbus::Result<()> {
    let mut changed = HashMap::new();
    changed.insert(
        "PlaybackStatus",
        OwnedValue::from(Str::from(playback_status_str(state.playback_status))),
    );
    changed.insert("Metadata", OwnedValue::from(metadata_for(state)));
    changed.insert("CanPlay", OwnedValue::from(state.can_play));
    changed.insert("CanPause", OwnedValue::from(state.can_pause));
    changed.insert("CanSeek", OwnedValue::from(state.can_seek));

    connection.emit_signal(
        None::<&str>,
        OBJECT_PATH,
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
        &("org.mpris.MediaPlayer2.Player", changed, Vec::<&str>::new()),
    )
}

struct RootInterface {
    command_tx: Sender<MediaCommand>,
}

#[interface(name = "org.mpris.MediaPlayer2")]
impl RootInterface {
    fn raise(&self) {}

    fn quit(&self) {
        let _ = self.command_tx.send(MediaCommand::Stop);
    }

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn fullscreen(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn can_set_fullscreen(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> &str {
        "Rust Music Player"
    }

    #[zbus(property)]
    fn desktop_entry(&self) -> &str {
        "rust-music-player"
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<&str> {
        vec!["file"]
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<&str> {
        vec![
            "audio/flac",
            "audio/mpeg",
            "audio/ogg",
            "audio/wav",
            "audio/x-wav",
        ]
    }
}

struct PlayerInterface {
    state: Arc<Mutex<MprisState>>,
    command_tx: Sender<MediaCommand>,
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl PlayerInterface {
    fn next(&self) {
        let _ = self.command_tx.send(MediaCommand::Next);
    }

    fn previous(&self) {
        let _ = self.command_tx.send(MediaCommand::Previous);
    }

    fn pause(&self) {
        let _ = self.command_tx.send(MediaCommand::Pause);
    }

    fn play_pause(&self) {
        let _ = self.command_tx.send(MediaCommand::PlayPause);
    }

    fn stop(&self) {
        let _ = self.command_tx.send(MediaCommand::Stop);
    }

    fn play(&self) {
        let _ = self.command_tx.send(MediaCommand::Play);
    }

    fn seek(&self, offset: i64) {
        let _ = self.command_tx.send(MediaCommand::SeekRelative(offset));
    }

    fn set_position(&self, _track_id: ObjectPath<'_>, position: i64) {
        let position = Duration::from_micros(position.max(0) as u64);
        let _ = self.command_tx.send(MediaCommand::SeekAbsolute(position));
    }

    fn open_uri(&self, _uri: &str) {}

    #[zbus(property)]
    fn playback_status(&self) -> String {
        playback_status_str(self.snapshot().playback_status).to_string()
    }

    #[zbus(property)]
    fn loop_status(&self) -> &str {
        "None"
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn shuffle(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        metadata_for(&self.snapshot())
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn position(&self) -> i64 {
        self.snapshot().position.as_micros().min(i64::MAX as u128) as i64
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.snapshot().can_play
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.snapshot().can_pause
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.snapshot().can_seek
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

impl PlayerInterface {
    fn snapshot(&self) -> MprisState {
        self.state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_default()
    }
}

fn metadata_for(state: &MprisState) -> HashMap<String, OwnedValue> {
    let mut metadata = HashMap::new();
    let track_id = ObjectPath::try_from(TRACK_ID).expect("valid MPRIS track id");
    metadata.insert("mpris:trackid".to_string(), OwnedValue::from(track_id));

    if let Some(duration) = state.duration {
        metadata.insert(
            "mpris:length".to_string(),
            OwnedValue::from(duration.as_micros().min(i64::MAX as u128) as i64),
        );
    }

    if !state.title.is_empty() {
        metadata.insert(
            "xesam:title".to_string(),
            OwnedValue::from(Str::from(state.title.clone())),
        );
    }

    if !state.album.is_empty() {
        metadata.insert(
            "xesam:album".to_string(),
            OwnedValue::from(Str::from(state.album.clone())),
        );
    }

    if let Some(art_url) = &state.art_url {
        metadata.insert(
            "mpris:artUrl".to_string(),
            OwnedValue::from(Str::from(art_url.clone())),
        );
    }

    if !state.artist.is_empty() {
        if let Ok(value) = OwnedValue::try_from(Value::from(vec![state.artist.as_str()])) {
            metadata.insert("xesam:artist".to_string(), value);
        }
    }

    metadata
}

fn playback_status_str(status: PlaybackStatus) -> &'static str {
    match status {
        PlaybackStatus::Playing => "Playing",
        PlaybackStatus::Paused => "Paused",
        PlaybackStatus::Stopped => "Stopped",
    }
}
