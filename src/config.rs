use crate::{Result, system};
use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

fn config_path() -> Result<PathBuf> {
    Ok(system::data_dir()?.join("output-dir.txt"))
}

pub fn output_dir() -> Result<PathBuf> {
    match fs::read_to_string(config_path()?) {
        Ok(text) => {
            let path = PathBuf::from(text);
            if !path.is_absolute() {
                return Err("Повреждена настройка папки. Укажите set-dir заново или выполните set-dir --reset".into());
            }
            Ok(path)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(system::videos()?.join("LiteRecorder"))
        }
        Err(e) => Err(e.into()),
    }
}

pub fn set_dir(argument: Option<&OsString>) -> Result<()> {
    let Some(argument) = argument else {
        println!("Папка сохранения: {}", output_dir()?.display());
        return Ok(());
    };
    let _lock = system::named_lock("config")?;
    let config = config_path()?;
    if argument == "--reset" {
        match fs::remove_file(config) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    } else {
        if argument.is_empty() || argument.to_str().is_some_and(|s| s.starts_with("--")) {
            return Err("Использование: set-dir \"D:\\Записи\" | set-dir | set-dir --reset".into());
        }
        let directory = std::path::absolute(argument)?;
        let text = directory
            .to_str()
            .ok_or("Путь должен содержать корректный Unicode")?;
        fs::create_dir_all(&directory)?;
        // Validate actual write access before replacing the previous setting.
        let probe = directory.join(format!(
            ".lite-recorder-write-test-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)?;
        drop(file);
        fs::remove_file(probe)?;
        let temporary = config.with_extension("tmp");
        fs::write(&temporary, text)?;
        // Windows rename replaces the previous file atomically.
        fs::rename(&temporary, config)?;
    }
    println!("Папка для следующих записей: {}", output_dir()?.display());
    Ok(())
}
