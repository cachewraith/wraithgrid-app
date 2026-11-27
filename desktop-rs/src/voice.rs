//! Voice typing: the microphone, recorded on this computer, turned into text by a local Whisper
//! model (whisper.cpp), so speech never leaves the machine. The webview can't do it: WebKitGTK has
//! no speech recognition and WebView2's needs a cloud key. The model (multilingual "small", q5_1)
//! is downloaded once into `~/.wraithgrid/models`, checked against a pinned SHA-256, and shared by
//! every user-data dir. While recording, `live` re-reads what was said so far every moment so the
//! box shows the words as they're spoken. ipc/voice.rs is the channel side.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use sha2::{Digest, Sha256};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub const MODEL_FILE: &str = "ggml-small-q5_1.bin";
/// Pinned to a commit of ggerganov/whisper.cpp on Hugging Face, so the file can't change under us.
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-small-q5_1.bin";
const MODEL_SHA256: &str = "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb";
pub const MODEL_BYTES: u64 = 190_085_487;

/// Whisper's input: 16 kHz mono.
const RATE: u32 = 16_000;
/// A recording stops by itself after this long.
pub const MAX_SECS: u64 = 300;
/// Shorter than this is a click, not speech.
const MIN_SAMPLES: usize = (RATE as usize) * 3 / 10;

/// `~/.wraithgrid/models/<file>`.
pub fn model_path() -> PathBuf {
    wraithgrid_core::paths::home_dir()
        .join(".wraithgrid")
        .join("models")
        .join(MODEL_FILE)
}

pub fn model_ready() -> bool {
    std::fs::metadata(model_path()).is_ok_and(|m| m.len() == MODEL_BYTES)
}

/// Downloads the model to a `.part` file, checks its hash, then renames it into place.
/// `progress(done, total)` is called as it goes; `cancel` stops it.
pub fn download_model(
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    let path = model_path();
    let dir = path.parent().expect("model dir");
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
    let part = path.with_extension("bin.part");
    let res = ureq::get(MODEL_URL)
        .call()
        .map_err(|e| format!("Couldn't download the speech model: {e}"))?;
    let total = res
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(MODEL_BYTES);
    let mut body = res.into_body();
    let mut reader = body.as_reader();
    let mut file =
        std::fs::File::create(&part).map_err(|e| format!("Couldn't save the speech model: {e}"))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    let mut last = Instant::now();
    let result = (|| {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("Download cancelled.".to_string());
            }
            let n = reader
                .read(&mut buf)
                .map_err(|e| format!("The download stopped: {e}"))?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
            file.write_all(&buf[..n])
                .map_err(|e| format!("Couldn't save the speech model: {e}"))?;
            done += n as u64;
            if last.elapsed() >= Duration::from_millis(200) {
                last = Instant::now();
                progress(done, total);
            }
        }
        file.sync_all()
            .map_err(|e| format!("Couldn't save the speech model: {e}"))?;
        let got = hex(&hash.finalize());
        if got != MODEL_SHA256 {
            return Err("The downloaded speech model is damaged. Try again.".to_string());
        }
        Ok(())
    })();
    drop(file);
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    progress(done, total);
    std::fs::rename(&part, &path).map_err(|e| format!("Couldn't save the speech model: {e}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A recording in progress: the capture thread owns the stream (it isn't `Send` everywhere).
pub struct Recording {
    stop: mpsc::Sender<()>,
    samples: Arc<Mutex<Vec<f32>>>,
    rate: u32,
    thread: Option<JoinHandle<()>>,
}

/// Opens the default microphone and records mono samples until `finish_since`. `level(0..1)` is called
/// about ten times a second, so the button can show it hears you; the recording ends by itself
/// after `MAX_SECS` (`ended` is called then).
pub fn record(
    level: impl Fn(f32) + Send + 'static,
    ended: impl FnOnce() + Send + 'static,
) -> Result<Recording, String> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let samples = Arc::new(Mutex::new(Vec::<f32>::new()));
    let peak = Arc::new(AtomicU32::new(0));
    let (s2, p2) = (samples.clone(), peak.clone());
    let thread = std::thread::Builder::new()
        .name("voice-capture".into())
        .spawn(move || {
            let stream = match open_input(s2, p2) {
                Ok((stream, rate)) => {
                    let _ = ready_tx.send(Ok(rate));
                    stream
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let started = Instant::now();
            // Until `finish_since` (or the Recording is dropped): the level, and the time limit.
            while let Err(mpsc::RecvTimeoutError::Timeout) =
                stop_rx.recv_timeout(Duration::from_millis(100))
            {
                level(f32::from_bits(peak.swap(0, Ordering::Relaxed)).min(1.0));
                if started.elapsed() >= Duration::from_secs(MAX_SECS) {
                    drop(stream);
                    ended();
                    return;
                }
            }
            drop(stream);
        })
        .map_err(|e| e.to_string())?;
    match ready_rx.recv() {
        Ok(Ok(rate)) => Ok(Recording {
            stop: stop_tx,
            samples,
            rate,
            thread: Some(thread),
        }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => Err("The microphone stopped unexpectedly.".into()),
    }
}

impl Recording {
    /// Stops the microphone and gives the audio from raw sample `from` on (what `live` hasn't
    /// written down yet) as 16 kHz mono.
    pub fn finish_since(mut self, from: usize) -> Vec<f32> {
        let _ = self.stop.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let raw = std::mem::take(&mut *self.samples.lock().unwrap());
        resample(raw.get(from..).unwrap_or_default(), self.rate, RATE)
    }

    /// A handle on the samples as they come in, for `live`.
    pub fn tap(&self) -> Tap {
        Tap {
            samples: self.samples.clone(),
            rate: self.rate,
        }
    }
}

/// The recording's samples so far, read while it goes on.
pub struct Tap {
    samples: Arc<Mutex<Vec<f32>>>,
    rate: u32,
}

impl Tap {
    /// The audio from raw sample `from` on, as 16 kHz mono, and the raw length it reached.
    fn since(&self, from: usize) -> (Vec<f32>, usize) {
        let raw = self.samples.lock().unwrap();
        let end = raw.len();
        let part = raw.get(from..).unwrap_or_default().to_vec();
        drop(raw);
        (resample(&part, self.rate, RATE), end)
    }

    /// 16 kHz sample `i` (of audio taken at raw `from`) back to a raw sample index.
    fn raw_at(&self, from: usize, i: usize) -> usize {
        from + (i as u64 * self.rate as u64 / RATE as u64) as usize
    }
}

/// How often the words are re-read while you talk (plus however long a pass takes).
const LIVE_EVERY: Duration = Duration::from_millis(500);
/// Not written down for good yet past this much speech: the head is, so each pass stays short.
const COMMIT_AT: usize = RATE as usize * 20;
/// Where that cut goes: the quietest moment between these (from the start of the uncommitted audio).
const CUT_FROM: usize = RATE as usize * 12;
const CUT_TO: usize = RATE as usize * 19;

/// Heard this much before the language is taken as known (shorter guesses are unreliable).
const LANG_AFTER: usize = RATE as usize * 3;

/// What `Live` wrote down for good: the text, the raw sample it ends at (the rest is still to be
/// read) and the language spoken, once known.
#[derive(Default)]
pub struct Written {
    pub text: String,
    pub until: usize,
    pub lang: Option<&'static str>,
}

/// The live transcript while recording: a thread that keeps re-reading the audio heard so far
/// and calls `partial` with the text. Long speech is written down for good in pieces, cut at a
/// pause, so a pass never reads more than ~20 s. The language is detected once (it costs a pass
/// of its own), then kept. `finish` gives back what was written down.
pub struct Live {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Written>>,
}

pub fn live(engine: Arc<Engine>, tap: Tap, partial: impl Fn(&str) + Send + 'static) -> Live {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name("voice-live".into())
        .spawn(move || {
            let mut done = String::new();
            let mut from = 0usize;
            let mut seen = 0usize;
            let mut lang: Option<&'static str> = None;
            let mut shown = String::new();
            'passes: loop {
                let waited = Instant::now();
                while waited.elapsed() < LIVE_EVERY {
                    if flag.load(Ordering::Relaxed) {
                        break 'passes;
                    }
                    std::thread::sleep(Duration::from_millis(40));
                }
                let (mut audio, end) = tap.since(from);
                if end == seen {
                    continue; // nothing new (the time limit stopped the mic)
                }
                seen = end;
                if audio.len() > COMMIT_AT {
                    let cut = quiet_cut(&audio, CUT_FROM, CUT_TO);
                    match engine.pass(&audio[..cut], false, lang, Some(&flag)) {
                        Ok((t, l)) => {
                            lang = lang.or(l);
                            done = joined(&done, &t);
                            from = tap.raw_at(from, cut);
                            audio.drain(..cut);
                        }
                        Err(_) => break,
                    }
                }
                match engine.pass(&audio, true, lang, Some(&flag)) {
                    Ok((t, l)) => {
                        if lang.is_none() && from + audio.len() >= LANG_AFTER && !t.is_empty() {
                            lang = l;
                        }
                        let text = joined(&done, &t);
                        if text != shown {
                            partial(&text);
                            shown = text;
                        }
                    }
                    Err(_) if flag.load(Ordering::Relaxed) => break,
                    Err(e) => eprintln!("wraithgrid: voice: live pass: {e}"),
                }
            }
            Written {
                text: done,
                until: from,
                lang,
            }
        })
        .ok();
    Live { stop, thread }
}

impl Live {
    /// Stops re-reading (a pass under way is cut short) and gives what was written down for good.
    pub fn finish(mut self) -> Written {
        self.stop.store(true, Ordering::Relaxed);
        self.thread
            .take()
            .and_then(|t| t.join().ok())
            .unwrap_or_default()
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Two pieces of transcript as one, a space between.
pub fn joined(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => format!("{a} {b}"),
    }
}

/// The middle of the quietest 100 ms between `lo` and `hi` (clamped to the audio): a pause
/// between words, so cutting there doesn't split one.
pub fn quiet_cut(audio: &[f32], lo: usize, hi: usize) -> usize {
    const FRAME: usize = RATE as usize / 10;
    let hi = hi.min(audio.len().saturating_sub(FRAME));
    if lo >= hi {
        return audio.len().min(lo.max(1));
    }
    let mut best = (f32::MAX, lo);
    let mut at = lo;
    while at <= hi {
        let energy: f32 = audio[at..at + FRAME].iter().map(|s| s * s).sum();
        if energy < best.0 {
            best = (energy, at);
        }
        at += FRAME / 2;
    }
    best.1 + FRAME / 2
}

/// whisper.cpp asks this between steps; `data` is the `AtomicBool` passed in `Engine::pass`.
unsafe extern "C" fn abort_if_set(data: *mut std::ffi::c_void) -> bool {
    // SAFETY: `pass` sets `data` to a `&AtomicBool` that outlives the `full` call.
    unsafe { (*(data as *const AtomicBool)).load(Ordering::Relaxed) }
}

impl Drop for Recording {
    fn drop(&mut self) {
        let _ = self.stop.send(());
    }
}

fn open_input(
    samples: Arc<Mutex<Vec<f32>>>,
    peak: Arc<AtomicU32>,
) -> Result<(cpal::Stream, u32), String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("No microphone found. Plug one in or check your sound settings.")?;
    let config = device
        .default_input_config()
        .map_err(|e| format!("Couldn't use the microphone: {e}"))?;
    let rate = config.sample_rate();
    let channels = config.channels().max(1) as usize;
    let cap = rate as usize * (MAX_SECS as usize + 1);
    let stream = match config.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, config.into(), channels, cap, samples, peak),
        SampleFormat::I16 => build::<i16>(&device, config.into(), channels, cap, samples, peak),
        SampleFormat::U16 => build::<u16>(&device, config.into(), channels, cap, samples, peak),
        SampleFormat::I32 => build::<i32>(&device, config.into(), channels, cap, samples, peak),
        SampleFormat::I8 => build::<i8>(&device, config.into(), channels, cap, samples, peak),
        SampleFormat::U8 => build::<u8>(&device, config.into(), channels, cap, samples, peak),
        SampleFormat::F64 => build::<f64>(&device, config.into(), channels, cap, samples, peak),
        f => {
            return Err(format!(
                "The microphone's sample format ({f:?}) isn't supported."
            ))
        }
    }?;
    stream
        .play()
        .map_err(|e| format!("Couldn't start the microphone: {e}"))?;
    Ok((stream, rate))
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    cap: usize,
    samples: Arc<Mutex<Vec<f32>>>,
    peak: Arc<AtomicU32>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let mut out = samples.lock().unwrap();
                let mut loudest = f32::from_bits(peak.load(Ordering::Relaxed));
                for frame in data.chunks(channels) {
                    if out.len() >= cap {
                        break;
                    }
                    let sum: f32 = frame.iter().map(|&s| f32::from_sample(s)).sum();
                    let v = sum / frame.len() as f32;
                    loudest = loudest.max(v.abs());
                    out.push(v);
                }
                peak.store(loudest.to_bits(), Ordering::Relaxed);
            },
            |e| eprintln!("wraithgrid: microphone: {e}"),
            None,
        )
        .map_err(|e| format!("Couldn't open the microphone: {e}"))
}

/// Linear resampling; plenty for speech going into Whisper.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let len = ((input.len() as f64) / ratio).floor() as usize;
    (0..len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let j = pos.floor() as usize;
            let frac = (pos - j as f64) as f32;
            let a = input[j];
            let b = *input.get(j + 1).unwrap_or(&a);
            a + (b - a) * frac
        })
        .collect()
}

/// The loaded model, kept after the first use (loading takes a second or two).
#[derive(Default)]
pub struct Engine {
    ctx: Mutex<Option<Arc<WhisperContext>>>,
}

/// whisper.cpp is built for this x86-64 baseline (.cargo/config.toml: GGML_NATIVE=OFF); on an
/// older CPU its code would crash the app with an illegal instruction, so it's never loaded there.
pub fn cpu_supported() -> Result<(), String> {
    #[cfg(target_arch = "x86_64")]
    {
        let ok = is_x86_feature_detected!("sse4.2")
            && is_x86_feature_detected!("avx")
            && is_x86_feature_detected!("avx2")
            && is_x86_feature_detected!("fma")
            && is_x86_feature_detected!("f16c")
            && is_x86_feature_detected!("bmi2");
        if !ok {
            return Err(
                "Voice typing needs a processor with AVX2 (most from 2013 on); this one doesn't have it."
                    .into(),
            );
        }
    }
    Ok(())
}

impl Engine {
    fn context(&self) -> Result<Arc<WhisperContext>, String> {
        cpu_supported()?;
        let mut slot = self.ctx.lock().unwrap();
        if let Some(c) = slot.as_ref() {
            return Ok(c.clone());
        }
        if !model_ready() {
            return Err("The speech model isn't downloaded yet.".into());
        }
        whisper_rs::install_logging_hooks();
        let path = model_path();
        let ctx = WhisperContext::new_with_params(&path, WhisperContextParameters::default())
            .map_err(|e| format!("Couldn't load the speech model: {e}"))?;
        let ctx = Arc::new(ctx);
        *slot = Some(ctx.clone());
        Ok(ctx)
    }

    /// Loads the model now (while the user is still talking).
    pub fn warm(&self) -> Result<(), String> {
        self.context().map(|_| ())
    }

    /// 16 kHz mono → text, in the language spoken (detected). Empty for silence.
    /// `lang`: the language when already known (`Live::finish` gives it), which skips detecting
    /// it: that costs a whole extra pass of the model.
    pub fn transcribe_in(
        &self,
        audio: &[f32],
        lang: Option<&'static str>,
    ) -> Result<String, String> {
        self.pass(audio, false, lang, None).map(|(text, _)| text)
    }

    /// `transcribe`, and the language it was in. `quick`: the encoder cut to the audio's length
    /// (faster on short clips, a little less exact: fine for the words shown while talking).
    /// Stops early, with an error, once `abort` is set.
    fn pass(
        &self,
        audio: &[f32],
        quick: bool,
        lang: Option<&'static str>,
        abort: Option<&AtomicBool>,
    ) -> Result<(String, Option<&'static str>), String> {
        if audio.len() < MIN_SAMPLES || audio.iter().all(|s| s.abs() < 1e-3) {
            return Ok((String::new(), lang));
        }
        let ctx = self.context()?;
        let mut state = ctx
            .create_state()
            .map_err(|e| format!("Couldn't start the speech model: {e}"))?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(lang.unwrap_or("auto")));
        params.set_translate(false);
        params.set_n_threads(threads());
        params.set_no_context(true);
        params.set_no_timestamps(true);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        if quick {
            // 50 encoder frames a second (1500 = 30 s), and some room so the end isn't clipped.
            let frames = (audio.len() as f32 / RATE as f32 * 50.0).ceil() as i32 + 256;
            params.set_audio_ctx(frames.min(1500));
        }
        if let Some(flag) = abort {
            // SAFETY: `flag` outlives `state.full` below, the only call that reads it.
            unsafe {
                params.set_abort_callback(Some(abort_if_set));
                params.set_abort_callback_user_data(
                    flag as *const AtomicBool as *mut std::ffi::c_void,
                );
            }
        }
        // Whisper wants at least a second; pad short clips with silence.
        let mut padded;
        let audio = if audio.len() < RATE as usize + RATE as usize / 10 {
            padded = audio.to_vec();
            padded.resize(RATE as usize + RATE as usize / 10, 0.0);
            &padded[..]
        } else {
            audio
        };
        state
            .full(params, audio)
            .map_err(|e| format!("Couldn't turn the speech into text: {e}"))?;
        let mut text = String::new();
        for i in 0..state.full_n_segments() {
            if let Some(seg) = state.get_segment(i) {
                if let Ok(s) = seg.to_str_lossy() {
                    text.push_str(&s);
                }
            }
        }
        let spoken = lang.or_else(|| whisper_rs::get_lang_str(state.full_lang_id_from_state()));
        Ok((clean(&text), spoken))
    }
}

fn threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8) as i32
}

/// Whisper's text, tidied: no leading space, and no "[BLANK_AUDIO]"/"(music)" style markers it
/// writes for silence or noise.
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0i32;
    for c in text.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_halves_and_keeps_the_shape() {
        let input: Vec<f32> = (0..8).map(|i| i as f32).collect();
        assert_eq!(resample(&input, 32_000, 16_000), vec![0.0, 2.0, 4.0, 6.0]);
        assert_eq!(resample(&input, 16_000, 16_000), input);
        let up = resample(&[0.0, 1.0], 8_000, 16_000);
        assert_eq!(up, vec![0.0, 0.5, 1.0, 1.0]);
        assert_eq!(resample(&vec![0.0; 48_000], 48_000, 16_000).len(), 16_000);
    }

    #[test]
    fn clean_drops_markers_and_extra_spaces() {
        assert_eq!(clean(" Hello there."), "Hello there.");
        assert_eq!(clean(" [BLANK_AUDIO]"), "");
        assert_eq!(
            clean("(music) Fix the  login bug [inaudible] now"),
            "Fix the login bug now"
        );
    }

    #[test]
    fn joined_puts_one_space_between() {
        assert_eq!(joined("", "hello"), "hello");
        assert_eq!(joined("hello", ""), "hello");
        assert_eq!(joined("hello", "there"), "hello there");
    }

    #[test]
    fn quiet_cut_finds_the_pause() {
        // Loud, a pause at 14.0–14.2 s, loud again.
        let mut audio = vec![0.5f32; RATE as usize * 22];
        let pause = RATE as usize * 14;
        audio[pause..pause + RATE as usize / 5].fill(0.0);
        let cut = quiet_cut(&audio, CUT_FROM, CUT_TO);
        assert!(cut >= pause && cut <= pause + RATE as usize / 5, "{cut}");
        // Too short for the window: the cut can't pass the end.
        assert!(quiet_cut(&[0.1; 100], CUT_FROM, CUT_TO) <= 100);
    }

    #[test]
    fn tap_maps_back_to_raw_samples() {
        let tap = Tap {
            samples: Arc::new(Mutex::new(vec![0.0; 48_000])),
            rate: 48_000,
        };
        let (audio, end) = tap.since(24_000);
        assert_eq!((audio.len(), end), (8_000, 48_000));
        assert_eq!(tap.raw_at(24_000, 8_000), 48_000);
    }

    #[test]
    fn silence_is_empty_without_a_model() {
        let e = Engine::default();
        assert_eq!(e.transcribe_in(&[0.0; 32_000], None).unwrap(), "");
        assert_eq!(e.transcribe_in(&[0.5; 100], None).unwrap(), "");
    }

    /// Downloads the real model if missing (into `$HOME/.wraithgrid/models`) and
    /// transcribes a 16-bit mono WAV: `WG_VOICE_WAV=jfk.wav HOME=$(mktemp -d) cargo test
    /// --no-default-features voice::tests::live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_download_and_transcribe() {
        let wav = std::fs::read(std::env::var("WG_VOICE_WAV").expect("WG_VOICE_WAV")).unwrap();
        let rate = u32::from_le_bytes(wav[24..28].try_into().unwrap());
        let data = wav.windows(4).position(|w| w == b"data").unwrap() + 8;
        let pcm: Vec<f32> = wav[data..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0)
            .collect();
        if !model_ready() {
            let mut calls = 0;
            download_model(&AtomicBool::new(false), |_, _| calls += 1).unwrap();
            assert!(calls > 0);
        }
        assert!(model_ready());
        let t = Instant::now();
        let text = Engine::default()
            .transcribe_in(&resample(&pcm, rate, RATE), None)
            .unwrap();
        eprintln!("{:?} in {:?}", text, t.elapsed());
        assert!(text
            .to_lowercase()
            .contains("ask not what your country can do for you"));
    }

    /// Plays a 16-bit mono WAV into `live` in real time (as the mic would) and prints each
    /// partial with when it came: `WG_VOICE_WAV=jfk.wav cargo test --no-default-features
    /// voice::tests::live_partials -- --ignored --nocapture` (needs the model downloaded).
    #[test]
    #[ignore]
    fn live_partials() {
        let wav = std::fs::read(std::env::var("WG_VOICE_WAV").expect("WG_VOICE_WAV")).unwrap();
        let rate = u32::from_le_bytes(wav[24..28].try_into().unwrap());
        let data = wav.windows(4).position(|w| w == b"data").unwrap() + 8;
        let pcm: Vec<f32> = wav[data..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0)
            .collect();
        let samples = Arc::new(Mutex::new(Vec::new()));
        let tap = Tap {
            samples: samples.clone(),
            rate,
        };
        let engine = Arc::new(Engine::default());
        engine.warm().unwrap();
        let t = Instant::now();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let s2 = seen.clone();
        let l = live(engine.clone(), tap, move |p| {
            eprintln!("{:>6.2}s  {p}", t.elapsed().as_secs_f32());
            s2.lock().unwrap().push(p.to_string());
        });
        for chunk in pcm.chunks(rate as usize / 10) {
            samples.lock().unwrap().extend_from_slice(chunk);
            std::thread::sleep(Duration::from_millis(100));
        }
        let w = l.finish();
        let rest = engine
            .transcribe_in(
                &resample(&samples.lock().unwrap()[w.until..], rate, RATE),
                w.lang,
            )
            .unwrap();
        let fin = joined(&w.text, &rest);
        eprintln!("final at {:.2}s: {fin}", t.elapsed().as_secs_f32());
        assert!(seen.lock().unwrap().len() >= 3, "too few partials");
        assert!(fin
            .to_lowercase()
            .contains("ask not what your country can do for you"));
    }

    /// Opens the default microphone for 1.5 s (nothing is kept): `cargo test --no-default-features
    /// voice::tests::live_mic -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_mic() {
        let levels = Arc::new(Mutex::new(Vec::new()));
        let l2 = levels.clone();
        let rec = record(move |l| l2.lock().unwrap().push(l), || {}).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let audio = rec.finish_since(0);
        let n = levels.lock().unwrap().len();
        eprintln!("{} samples at 16 kHz, {n} level events", audio.len());
        assert!(audio.len() > RATE as usize && n >= 10);
    }

    #[test]
    fn this_cpu_runs_whisper() {
        assert_eq!(cpu_supported(), Ok(()));
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(hex(&[0, 15, 255]), "000fff");
    }
}
