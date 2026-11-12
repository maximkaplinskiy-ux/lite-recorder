mod audio;
mod audio_selection;
mod config;
mod encoder;
mod recording;
mod replay;
mod system;

use std::{
    env, fs,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use windows::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::*};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Default)]
struct Options {
    duration: Option<Duration>,
    output: Option<PathBuf>,
}

fn duration(value: &str) -> Result<Duration> {
    let (number, factor) = match value.as_bytes().last() {
        Some(b's') => (&value[..value.len() - 1], 1),
        Some(b'm') => (&value[..value.len() - 1], 60),
        Some(b'h') => (&value[..value.len() - 1], 3600),
        _ => (value, 1),
    };
    let seconds = number
        .parse::<u64>()?
        .checked_mul(factor)
        .ok_or("Слишком большой таймер")?;
    if seconds == 0 || seconds > 7 * 24 * 3600 {
        return Err("Таймер: от 1 секунды до 7 дней".into());
    }
    Ok(Duration::from_secs(seconds))
}

fn options(args: &[std::ffi::OsString]) -> Result<Options> {
    let mut out = Options::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("--duration") if out.duration.is_none() => {
                out.duration = Some(duration(
                    it.next()
                        .ok_or("После --duration нужно время")?
                        .to_str()
                        .ok_or("Некорректное время")?,
                )?)
            }
            Some("--output") if out.output.is_none() => {
                out.output = Some(PathBuf::from(it.next().ok_or("После --output нужен путь")?))
            }
            _ => {
                return Err(format!(
                    "Неизвестный или повторный параметр: {}",
                    arg.to_string_lossy()
                )
                .into());
            }
        }
    }
    Ok(out)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Ошибка: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    system::create_readme()?;
    let args: Vec<_> = env::args_os().skip(1).collect();
    let cmd = args.first().and_then(|v| v.to_str()).unwrap_or("help");
    match cmd {
        "help" | "--help" | "-h" if args.len() <= 1 => println!("{}", system::readme_text()?),
        "start" => start(options(&args[1..])?, false)?,
        "replay-start" if args.len() == 2 => {
            let value = args[1].to_str().ok_or("Некорректное время буфера")?;
            let window = if value.chars().all(|c| c.is_ascii_digit()) {
                duration(&format!("{value}m"))?
            } else {
                duration(value)?
            };
            if window > Duration::from_secs(24 * 3600) {
                return Err("Буфер: не больше 24 часов".into());
            }
            start(
                Options {
                    duration: Some(window),
                    output: None,
                },
                true,
            )?;
        }
        "replay-save" if args.len() == 1 => replay::save()?,
        "replay-stop" if args.len() == 1 => {
            if system::open_event("replay-save", SYNCHRONIZATION_SYNCHRONIZE.0)?.is_some() {
                stop()?;
            } else {
                println!("Буфер откатов не включён.");
            }
        }
        "__replay" => replay::run(options(&args[1..])?)?,
        "__record" => recording::run(options(&args[1..])?)?,
        "stop" if args.len() == 1 => stop()?,
        "set-dir" if args.len() <= 2 => config::set_dir(args.get(1))?,
        "status" if args.len() == 1 => {
            if let Some(h) = system::open_event("ready", SYNCHRONIZATION_SYNCHRONIZE.0)? {
                if unsafe { WaitForSingleObject(h.0, 0) } == WAIT_OBJECT_0 {
                    println!(
                        "Идёт запись: {}",
                        fs::read_to_string(system::data_dir()?.join("current.txt"))
                            .unwrap_or_default()
                    );
                } else {
                    println!("Запись запускается или завершается.");
                }
            } else {
                println!("Запись не идёт.");
            }
        }
        "devices" if args.len() == 1 => audio::list_devices()?,
        _ => {
            return Err(
                "Команды: start, stop, replay-start, replay-save, replay-stop, set-dir, status, devices, help. Справка: lite-recorder help"
                    .into(),
            );
        }
    }
    Ok(())
}

fn start(mut opts: Options, replay: bool) -> Result<()> {
    let _launch_lock = system::named_lock("launch")?;
    if system::open_event("stop", SYNCHRONIZATION_SYNCHRONIZE.0)?.is_some() {
        return Err("Запись уже запущена или запускается".into());
    }
    let output = if replay {
        system::data_dir()?.join("ReplayCache").join(format!(
            "session-{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ))
    } else {
        match opts.output.take() {
            Some(path) => std::path::absolute(path)?,
            None => {
                let dir = config::output_dir()?;
                fs::create_dir_all(&dir)?;
                dir.join(format!(
                    "recording-{}.mp4",
                    SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()
                ))
            }
        }
    };
    if !replay
        && output
            .extension()
            .is_none_or(|e| !e.eq_ignore_ascii_case("mp4"))
    {
        return Err("Выходной файл должен иметь расширение .mp4".into());
    }
    if output.exists() {
        return Err("Файл уже существует; выберите другое имя".into());
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let log_path = system::data_dir()?.join(if replay { "replay.log" } else { "last.log" });
    let log = fs::File::create(&log_path)?;
    let mut child_cmd = Command::new(env::current_exe()?);
    child_cmd
        .arg(if replay { "__replay" } else { "__record" })
        .arg("--output")
        .arg(&output);
    if let Some(d) = opts.duration {
        child_cmd.arg("--duration").arg(d.as_secs().to_string());
    }
    let mut child = child_cmd
        .creation_flags(0x08000000)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(format!(
                "Запуск завершился ({status}). {}",
                fs::read_to_string(&log_path).unwrap_or_default()
            )
            .into());
        }
        if let Some(h) = system::open_event("ready", SYNCHRONIZATION_SYNCHRONIZE.0)?
            && unsafe { WaitForSingleObject(h.0, 0) } == WAIT_OBJECT_0
        {
            break;
        }
        if std::time::Instant::now() >= deadline {
            if let Some(h) = system::open_event("stop", EVENT_MODIFY_STATE.0)? {
                unsafe {
                    SetEvent(h.0)?;
                }
            }
            return Err(format!(
                "Запуск занял больше 30 секунд, отправлена команда остановки. Журнал: {}",
                log_path.display()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if replay {
        println!(
            "Буфер откатов включён: последние {} с. Сохранить: replay-save; выключить: replay-stop.",
            opts.duration.unwrap().as_secs()
        );
    } else {
        println!("Запись началась: {}", output.display());
    }
    if let Some(d) = opts.duration
        && !replay
    {
        println!("Остановка через {} с.", d.as_secs());
    }
    Ok(())
}

fn stop() -> Result<()> {
    let Some(event) = system::open_event("stop", EVENT_MODIFY_STATE.0)? else {
        println!("Запись не идёт.");
        return Ok(());
    };
    // Release our handle after signalling so the event disappears when the worker exits.
    unsafe {
        SetEvent(event.0)?;
    }
    drop(event);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while system::open_event("stop", SYNCHRONIZATION_SYNCHRONIZE.0)?.is_some() {
        if std::time::Instant::now() >= deadline {
            return Err("Файл ещё сохраняется. Проверьте status и журнал текущего режима (last.log или replay.log); процесс не был принудительно завершён".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let is_replay =
        fs::read_to_string(system::data_dir()?.join("mode.txt")).is_ok_and(|s| s == "replay");
    let log = fs::read_to_string(system::data_dir()?.join(if is_replay {
        "replay.log"
    } else {
        "last.log"
    }))?;
    if !log.lines().any(|l| l == "FINISHED") {
        return Err(format!("Запись завершилась с ошибкой. {log}").into());
    }
    if is_replay {
        println!("Буфер откатов выключен; несохранённые фрагменты удалены.");
    } else {
        println!(
            "Сохранено: {}",
            fs::read_to_string(system::data_dir()?.join("current.txt"))?
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timer_units_and_limits() {
        assert_eq!(duration("2m").unwrap().as_secs(), 120);
        assert_eq!(duration("1h").unwrap().as_secs(), 3600);
        assert_eq!(duration("30").unwrap().as_secs(), 30);
        for s in ["0", "-1", "1.5", "8d", "9999999999999999999h"] {
            assert!(duration(s).is_err());
        }
    }
    #[test]
    fn reject_incomplete_or_unknown_options() {
        for args in [
            vec!["--duration"],
            vec!["--other", "1"],
            vec!["--duration", "1", "--duration", "2"],
        ] {
            assert!(options(&args.into_iter().map(Into::into).collect::<Vec<_>>()).is_err());
        }
    }
}
