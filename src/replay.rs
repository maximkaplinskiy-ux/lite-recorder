use crate::{
    Options, Result, audio, config,
    encoder::{self, Pending, Router},
    recording::{Capture, Flags},
    system,
};
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::{
    Foundation::TimeSpan,
    Media::{
        Editing::{MediaClip, MediaComposition, MediaTrimmingPreference},
        Transcoding::TranscodeFailureReason,
    },
    Storage::StorageFile,
    Win32::{Foundation::WAIT_OBJECT_0, System::Threading::*},
    core::HSTRING,
};
use windows_capture::{capture::GraphicsCaptureApiHandler, monitor::Monitor, settings::*};

const CHUNK_HNS: i64 = 50_000_000;

struct Closed {
    path: PathBuf,
    start: i64,
    end: i64,
}
struct Cache {
    path: PathBuf,
    root: PathBuf,
}
impl Cache {
    fn cleanup(&self) -> Result<()> {
        if !self.path.exists() {
            return Ok(());
        }
        // Only remove a verified session directory created under our cache root.
        let root = fs::canonicalize(&self.root)?;
        let path = fs::canonicalize(&self.path)?;
        if path.parent() != Some(root.as_path())
            || !path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("session-"))
        {
            return Err("Не удалось проверить путь временных файлов буфера".into());
        }
        fs::remove_dir_all(path)?;
        Ok(())
    }
}
impl Drop for Cache {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn signal(name: &str) -> Result<()> {
    let event =
        system::open_event(name, EVENT_MODIFY_STATE.0)?.ok_or("Буфер откатов не включён")?;
    unsafe {
        SetEvent(event.0)?;
    }
    Ok(())
}

pub fn save() -> Result<()> {
    let _request_lock = system::named_lock("replay-save-command")?;
    let done = system::open_event(
        "replay-saved",
        (SYNCHRONIZATION_SYNCHRONIZE | EVENT_MODIFY_STATE).0,
    )?
    .ok_or("Буфер откатов не включён")?;
    unsafe {
        ResetEvent(done.0)?;
    }
    signal("replay-save")?;
    println!("Сохраняю откат; буфер продолжает записываться...");
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        if unsafe { WaitForSingleObject(done.0, 100) } == WAIT_OBJECT_0 {
            break;
        }
        if system::open_event("stop", SYNCHRONIZATION_SYNCHRONIZE.0)?.is_none() {
            return Err("Буфер завершился до сохранения. Проверьте replay.log".into());
        }
        if Instant::now() >= deadline {
            return Err(
                "Сохранение продолжается дольше 10 минут. Проверьте replay-result.txt и replay.log"
                    .into(),
            );
        }
    }
    let result = fs::read_to_string(system::data_dir()?.join("replay-result.txt"))?;
    if let Some(path) = result.strip_prefix("OK\n") {
        println!("Откат сохранён: {path}");
        Ok(())
    } else {
        Err(result.trim_start_matches("ERR\n").to_string().into())
    }
}

fn publish(result: Result<PathBuf>, done: &system::Handle) -> Result<()> {
    let text = match result {
        Ok(path) => format!("OK\n{}", path.display()),
        Err(error) => {
            eprintln!("Ошибка сохранения отката: {error}");
            format!("ERR\n{error}")
        }
    };
    fs::write(system::data_dir()?.join("replay-result.txt"), text)?;
    unsafe {
        SetEvent(done.0)?;
    }
    Ok(())
}

fn storage_file(path: &Path) -> Result<StorageFile> {
    // StorageFile expects a normal absolute path rather than the extended prefix.
    let path = fs::canonicalize(path)?;
    let text = path.to_string_lossy();
    let text = if let Some(unc) = text.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{unc}")
    } else {
        text.strip_prefix("\\\\?\\").unwrap_or(&text).to_string()
    };
    Ok(StorageFile::GetFileFromPathAsync(&HSTRING::from(text))?.join()?)
}

struct Snapshot(Vec<PathBuf>);
impl Drop for Snapshot {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

fn export(paths: Vec<PathBuf>, window: i64) -> Result<PathBuf> {
    let paths = Snapshot(paths);
    wasapi::initialize_mta().ok()?;
    let composition = MediaComposition::new()?;
    let mut clips = VecDeque::new();
    let mut total = 0i64;
    for path in &paths.0 {
        let clip = MediaClip::CreateFromFileAsync(&storage_file(path)?)?.join()?;
        total += clip.OriginalDuration()?.Duration;
        clips.push_back(clip);
    }
    // Keep exactly the requested tail, including a partial oldest fragment.
    let mut excess = (total - window).max(0);
    while let Some(clip) = clips.front() {
        let duration = clip.OriginalDuration()?.Duration;
        if excess < duration {
            break;
        }
        excess -= duration;
        clips.pop_front();
    }
    if let Some(clip) = clips.front() {
        clip.SetTrimTimeFromStart(TimeSpan { Duration: excess })?;
    }
    if clips.is_empty() {
        return Err("Буфер пока пуст".into());
    }
    for clip in clips {
        composition.Clips()?.Append(&clip)?;
    }
    let directory = config::output_dir()?;
    fs::create_dir_all(&directory)?;
    let output = directory.join(format!(
        "replay-{}.mp4",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)?;
    let result = (|| -> Result<()> {
        let profile = composition.CreateDefaultEncodingProfile()?;
        profile.Video()?.SetBitrate(6_000_000)?;
        profile.Audio()?.SetBitrate(160_000)?;
        let reason = composition
            .RenderToFileWithProfileAsync(
                &storage_file(&output)?,
                MediaTrimmingPreference::Precise,
                &profile,
            )?
            .join()?;
        if reason != TranscodeFailureReason::None {
            return Err(format!("Windows не смог сохранить откат: {reason:?}").into());
        }
        Ok(())
    })();
    // Close composition before deleting hard links: Windows may retain readers.
    drop(composition);
    if let Err(error) = result {
        let _ = fs::remove_file(&output);
        return Err(error);
    }
    Ok(output)
}

fn expired(end: i64, now: i64, window: i64) -> bool {
    end <= now - window
}

pub fn run(opts: Options) -> Result<()> {
    wasapi::initialize_mta().ok()?;
    let _lock = system::recording_lock()?;
    let stop = system::event("stop")?;
    let ready = system::event("ready")?;
    let save_request = system::event("replay-save")?;
    let saved = system::event("replay-saved")?;
    let window = opts.duration.ok_or("Не задан размер буфера")?.as_secs() as i64 * 10_000_000;
    let root = system::data_dir()?.join("ReplayCache");
    fs::create_dir_all(&root)?;
    let path = opts.output.ok_or("Нет пути буфера")?;
    if path.parent() != Some(root.as_path())
        || !path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("session-"))
    {
        return Err("Некорректный путь буфера".into());
    }
    fs::create_dir(&path)?;
    let cache = Cache { path, root };
    fs::write(system::data_dir()?.join("mode.txt"), "replay")?;
    let monitor = Monitor::primary()?;
    let (width, height) = (monitor.width()?, monitor.height()?);
    let first_path = cache.path.join("chunk-0.mp4");
    let router = Arc::new(Mutex::new(Router::new(
        encoder::create(width, height, &first_path)?,
        first_path,
    )));
    let epoch = Arc::new(AtomicI64::new(0));
    let audio = audio::start(router.clone(), epoch.clone())?;
    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithCursor,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Custom(Duration::from_nanos(1_000_000_000 / 30)),
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        Flags {
            encoder: router.clone(),
            epoch: epoch.clone(),
        },
    );
    let capture = match Capture::start_free_threaded(settings) {
        Ok(capture) => capture,
        Err(error) => {
            audio.end.store(1, Ordering::Release);
            let _ = audio.thread.join();
            router.lock().unwrap().finish_all()?;
            return Err(error.into());
        }
    };
    let mut completed: VecDeque<Closed> = VecDeque::new();
    let mut exporting: Option<thread::JoinHandle<Result<PathBuf>>> = None;
    let mut requested = None;
    let mut index = 1u64;
    let startup = Instant::now();
    let mut announced = false;
    let loop_result = (|| -> Result<()> {
        loop {
            if unsafe { WaitForSingleObject(stop.0, 20) } == WAIT_OBJECT_0 {
                break;
            }
            if capture.is_finished() {
                return Err("Захват экрана завершился".into());
            }
            if audio.thread.is_finished() {
                return Err("Аудиозахват завершился".into());
            }
            let origin = epoch.load(Ordering::Acquire);
            if origin == 0 {
                if startup.elapsed() > Duration::from_secs(15) {
                    return Err("Не получен первый кадр".into());
                }
                continue;
            }
            if !announced {
                fs::write(
                    system::data_dir()?.join("current.txt"),
                    format!(
                        "Буфер откатов: последние {} с; сохранить: replay-save",
                        window / 10_000_000
                    ),
                )?;
                unsafe {
                    SetEvent(ready.0)?;
                }
                announced = true;
            }
            if exporting.as_ref().is_some_and(|t| t.is_finished()) {
                let result = exporting
                    .take()
                    .unwrap()
                    .join()
                    .map_err(|_| "Ошибка потока сохранения")?;
                publish(result, &saved)?;
            }
            if unsafe { WaitForSingleObject(save_request.0, 0) } == WAIT_OBJECT_0 {
                unsafe {
                    ResetEvent(save_request.0)?;
                }
                if exporting.is_some() || requested.is_some() {
                    publish(Err("Сохранение уже выполняется".into()), &saved)?;
                } else {
                    requested = Some(system::qpc_hns()?);
                }
            }
            let (active_start, has_pending) = {
                let router = router.lock().unwrap();
                (router.active_start(), router.pending.is_some())
            };
            if !has_pending {
                let path = cache.path.join(format!("chunk-{index}.mp4"));
                index += 1;
                let encoder = encoder::create(width, height, &path)?;
                router.lock().unwrap().pending = Some(Pending {
                    encoder,
                    path,
                    at: active_start + CHUNK_HNS,
                });
            }
            if let Some(time) = requested
                && time >= active_start
                && let Some(pending) = router.lock().unwrap().pending.as_mut()
            {
                pending.at = time.max(active_start + 5_000_000);
            }
            loop {
                let segment = router.lock().unwrap().take_completed();
                let Some(segment) = segment else {
                    break;
                };
                segment.encoder.finish()?;
                let end = segment.end.unwrap();
                println!(
                    "Фрагмент {}: {:.3} с",
                    segment.path.display(),
                    (end - segment.start) as f64 / 10_000_000.0
                );
                completed.push_back(Closed {
                    path: segment.path,
                    start: segment.start,
                    end,
                });
                if requested.is_some_and(|request| end >= request) {
                    let mut paths = Vec::new();
                    for (i, chunk) in completed.iter().enumerate() {
                        let snapshot = cache.path.join(format!("snapshot-{index}-{i}.mp4"));
                        if fs::hard_link(&chunk.path, &snapshot).is_err() {
                            fs::copy(&chunk.path, &snapshot)?;
                        }
                        paths.push(snapshot);
                    }
                    exporting = Some(thread::spawn(move || export(paths, window)));
                    requested = None;
                }
            }
            let now = system::qpc_hns()?;
            while completed
                .front()
                .is_some_and(|chunk| expired(chunk.end, now, window))
            {
                let old = completed.pop_front().unwrap();
                fs::remove_file(&old.path)?;
                println!(
                    "Удалён старый фрагмент: {:.3}..{:.3}",
                    (old.start - origin) as f64 / 10_000_000.0,
                    (old.end - origin) as f64 / 10_000_000.0
                );
            }
        }
        Ok(())
    })();
    unsafe {
        let _ = ResetEvent(ready.0);
    }
    let end = system::qpc_hns()?;
    let capture_result = capture.stop();
    thread::sleep(Duration::from_millis(150));
    audio.end.store(end, Ordering::Release);
    let audio_result = audio.thread.join().map_err(|_| "Ошибка потока аудио");
    let finish_result = router.lock().unwrap().finish_all();
    if let Some(exporting) = exporting {
        publish(
            exporting.join().map_err(|_| "Ошибка потока сохранения")?,
            &saved,
        )?;
    } else if requested.is_some() {
        publish(
            Err("Буфер выключен до получения фрагмента; откат не сохранён".into()),
            &saved,
        )?;
    }
    loop_result?;
    capture_result?;
    audio_result??;
    finish_result?;
    cache.cleanup()?;
    println!("FINISHED");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retain_oldest_fragment_intersecting_the_window() {
        assert!(expired(5, 15, 10));
        assert!(!expired(6, 15, 10));
        assert!(!expired(5, 9, 10));
    }
}
