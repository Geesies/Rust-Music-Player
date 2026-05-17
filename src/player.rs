use std::{fs::File, io::BufReader, path::Path, time::Duration};

use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink, Source};

use crate::eq::{EqBand, EqualizerSource};

pub struct Player {
    stream: OutputStream,
    sink: Sink,
    current_duration: Option<Duration>,
}

impl Player {
    pub fn new() -> anyhow::Result<Self> {
        let stream = OutputStreamBuilder::open_default_stream()?;
        let sink = Sink::connect_new(stream.mixer());

        Ok(Self {
            stream,
            sink,
            current_duration: None,
        })
    }

    pub fn play_file(
        &mut self,
        path: &Path,
        eq_enabled: bool,
        eq_bands: Vec<EqBand>,
    ) -> anyhow::Result<()> {
        self.sink.stop();

        self.sink = Sink::connect_new(self.stream.mixer());

        let file = File::open(path)?;
        let source = Decoder::new(BufReader::new(file))?;

        self.current_duration = source.total_duration();

        let source: Box<dyn Source + Send> = if eq_enabled && !eq_bands.is_empty() {
            Box::new(EqualizerSource::new(source, eq_bands))
        } else {
            Box::new(source)
        };

        self.sink.append(source);
        self.sink.play();

        Ok(())
    }

    pub fn pause(&self) {
        self.sink.pause();
    }

    pub fn resume(&self) {
        self.sink.play();
    }

    pub fn is_paused(&self) -> bool {
        self.sink.is_paused()
    }

    pub fn is_finished(&self) -> bool {
        self.sink.empty()
    }

    pub fn position(&self) -> Duration {
        self.sink.get_pos()
    }

    pub fn duration(&self) -> Option<Duration> {
        self.current_duration
    }

    pub fn seek(&self, position: Duration) -> anyhow::Result<()> {
        self.sink
            .try_seek(position)
            .map_err(|err| anyhow::anyhow!("Seek failed: {err}"))?;

        Ok(())
    }

    pub fn set_volume(&self, volume: f32) {
        self.sink.set_volume(volume);
    }
}
