use std::{fs::File, path::Path, time::Duration};

use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink, Source};

use crate::eq::{AnalyzerSnapshot, AudioAnalyzer, EqBand, EqControl, EqualizerSource};

pub struct Player {
    stream: OutputStream,
    sink: Sink,
    current_duration: Option<Duration>,
    eq_control: EqControl,
    analyzer: AudioAnalyzer,
}

impl Player {
    pub fn new() -> anyhow::Result<Self> {
        let stream = OutputStreamBuilder::open_default_stream()?;
        let sink = Sink::connect_new(stream.mixer());

        Ok(Self {
            stream,
            sink,
            current_duration: None,
            eq_control: EqControl::new(false, Vec::new()),
            analyzer: AudioAnalyzer::new(),
        })
    }

    pub fn play_file(
        &mut self,
        path: &Path,
        eq_enabled: bool,
        eq_bands: Vec<EqBand>,
    ) -> anyhow::Result<()> {
        self.sink.stop();
        self.eq_control.update(eq_enabled, eq_bands);

        self.sink = Sink::connect_new(self.stream.mixer());

        let file = File::open(path)?;
        let source = Decoder::try_from(file)?;

        self.current_duration = source.total_duration();

        let source: Box<dyn Source + Send> = Box::new(EqualizerSource::new(
            source,
            self.eq_control.clone(),
            self.analyzer.clone(),
        ));

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

    pub fn set_eq(&self, enabled: bool, bands: Vec<EqBand>) {
        self.eq_control.update(enabled, bands);
    }

    pub fn analyzer_snapshot(&self) -> AnalyzerSnapshot {
        self.analyzer.snapshot()
    }
}
