use crate::{
    Options, Result, audio,
    encoder::{self, Router},
    system,
};
use std::{
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::{Duration, Instant},
};
use windows::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::*};
use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::*,
};

#[derive(Clone)]
pub(crate) struct Flags {
    pub encoder: Arc<Mutex<Router>>,
    pub epoch: Arc<AtomicI64>,
}
pub(crate) struct Capture {
    flags: Flags,
}
impl GraphicsCaptureApiHandler for Capture {
    type Flags = Flags;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    fn new(ctx: Context<Flags>) -> Result<Self> {
        Ok(Self { flags: ctx.flags })
    }
    fn on_frame_arrived(&mut self, frame: &mut Frame, _: InternalCaptureControl) -> Result<()> {
        let mut encoder = self
            .flags
            .encoder
            .lock()
            .map_err(|_| "Ошибка блокировки кодировщика")?;
        encoder.send_frame(frame)?;
        let _ = self.flags.epoch.compare_exchange(
            0,
            frame.timestamp()?.Duration,
            Ordering::Release,
            Ordering::Relaxed,
        );
        Ok(())
    }
}

pub fn run(opts: Options) -> Result<()> {
    wasapi::initialize_mta().ok()?;
    let _lock = system::recording_lock()?;
    let stop = system::event("stop")?;
    let ready = system::event("ready")?;
    let output = opts.output.ok_or("Нет выходного файла")?;
    // Reserve the file atomically; never overwrite an existing user recording.
    let reserve = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)?;
    drop(reserve);
    let monitor = Monitor::primary()?;
    let (width, height) = (monitor.width()?, monitor.height()?);
    println!("Основной экран: {width}×{height}; H.264 30 fps, 6 Мбит/с; AAC 48 kHz stereo.");
    fs::write(system::data_dir()?.join("mode.txt"), "recording")?;
    let encoder = Arc::new(Mutex::new(Router::new(
        encoder::create(width, height, &output)?,
        output.clone(),
    )));
    let epoch = Arc::new(AtomicI64::new(0));
    let audio = match audio::start(encoder.clone(), epoch.clone()) {
        Ok(a) => a,
        Err(e) => {
            // No frames were submitted; dropping the encoder releases its channels.
            encoder.lock().unwrap().finish_all()?;
            return Err(e);
        }
    };
    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithCursor,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Custom(Duration::from_nanos(1_000_000_000 / 30)),
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        Flags {
            encoder: encoder.clone(),
            epoch: epoch.clone(),
        },
    );
    let capture = match Capture::start_free_threaded(settings) {
        Ok(c) => c,
        Err(e) => {
            audio.end.store(1, Ordering::Release);
            let _ = audio.thread.join();
            encoder.lock().unwrap().finish_all()?;
            return Err(e.into());
        }
    };
    let startup = Instant::now();
    let mut started = None;
    let mut failure: Option<String> = None;
    loop {
        if epoch.load(Ordering::Acquire) != 0 && started.is_none() {
            fs::write(
                system::data_dir()?.join("current.txt"),
                output.to_string_lossy().as_bytes(),
            )?;
            unsafe {
                SetEvent(ready.0)?;
            }
            started = Some(Instant::now());
        }
        if unsafe { WaitForSingleObject(stop.0, 20) } == WAIT_OBJECT_0 {
            break;
        }
        if opts
            .duration
            .is_some_and(|d| started.is_some_and(|s| s.elapsed() >= d))
        {
            break;
        }
        if capture.is_finished() {
            failure = Some("Захват экрана неожиданно завершился".into());
            break;
        }
        if audio.thread.is_finished() {
            failure = Some("Аудиозахват неожиданно завершился".into());
            break;
        }
        if started.is_none() && startup.elapsed() > Duration::from_secs(15) {
            failure = Some("Не получен первый кадр за 15 секунд".into());
            break;
        }
    }
    unsafe {
        ResetEvent(ready.0)?;
    }
    let end = system::qpc_hns()?;
    let capture_result = capture.stop();
    // Allow the final WASAPI packet to arrive before flushing up to the stop time.
    std::thread::sleep(Duration::from_millis(150));
    audio.end.store(end, Ordering::Release);
    let audio_result = audio
        .thread
        .join()
        .map_err(|_| "Поток аудио аварийно завершён")?;
    let finish_result = encoder
        .lock()
        .map_err(|_| "Ошибка кодировщика")?
        .finish_all();
    capture_result?;
    audio_result?;
    finish_result?;
    if let Some(failure) = failure {
        return Err(failure.into());
    }
    println!("FINISHED");
    Ok(())
}
