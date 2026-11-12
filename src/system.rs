use crate::Result;
use std::{fs, path::PathBuf};
use windows::{
    Win32::{
        Foundation::*,
        System::{Com::CoTaskMemFree, Performance::*, Threading::*},
        UI::Shell::*,
    },
    core::{GUID, HSTRING},
};

pub struct Handle(pub HANDLE);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn folder(id: &GUID) -> Result<PathBuf> {
    unsafe {
        let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None)?;
        let text = raw.to_string();
        CoTaskMemFree(Some(raw.0.cast()));
        Ok(PathBuf::from(text?))
    }
}
pub fn videos() -> Result<PathBuf> {
    folder(&FOLDERID_Videos)
}
pub fn data_dir() -> Result<PathBuf> {
    let path = folder(&FOLDERID_LocalAppData)?.join("LiteRecorder");
    fs::create_dir_all(&path)?;
    Ok(path)
}
fn name(suffix: &str) -> Result<HSTRING> {
    // Stable per-user object names, scoped to the interactive Windows session.
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    data_dir()?.hash(&mut hash);
    Ok(HSTRING::from(format!(
        "Local\\LiteRecorder-{:x}-{suffix}",
        hash.finish()
    )))
}
pub fn event(suffix: &str) -> Result<Handle> {
    Ok(Handle(unsafe {
        CreateEventW(None, true, false, &name(suffix)?)?
    }))
}
pub fn open_event(suffix: &str, access: u32) -> Result<Option<Handle>> {
    match unsafe { OpenEventW(SYNCHRONIZATION_ACCESS_RIGHTS(access), false, &name(suffix)?) } {
        Ok(h) => Ok(Some(Handle(h))),
        Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) => {
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}
pub fn recording_lock() -> Result<Handle> {
    named_lock("lock")
}
pub fn named_lock(suffix: &str) -> Result<Handle> {
    unsafe {
        let handle = Handle(CreateMutexW(None, true, &name(suffix)?)?);
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Err("Другой экземпляр уже записывает".into());
        }
        Ok(handle)
    }
}
pub fn qpc_hns() -> Result<i64> {
    let (mut count, mut frequency) = (0, 0);
    unsafe {
        QueryPerformanceCounter(&mut count)?;
        QueryPerformanceFrequency(&mut frequency)?;
    }
    Ok(((count as i128 * 10_000_000) / frequency as i128) as i64)
}
pub fn readme_text() -> Result<String> {
    let exe = std::env::current_exe()?;
    Ok(include_str!("readme_template.txt")
        .replace("{{EXE}}", &exe.to_string_lossy())
        .replace("\r\n", "\n")
        .replace('\n', "\r\n"))
}

pub fn create_readme() -> Result<()> {
    let path = folder(&FOLDERID_Desktop)?.join("LiteRecorder-README.txt");
    let text = readme_text()?;
    let mut bytes = Vec::with_capacity(text.len() + 3);
    // BOM preserves Cyrillic text in older Windows Notepad versions.
    bytes.extend_from_slice(b"\xef\xbb\xbf");
    bytes.extend_from_slice(text.as_bytes());
    match fs::read(&path) {
        Ok(existing) if existing == bytes => return Ok(()),
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    use std::io::Write;
    let result = (|| -> Result<()> {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
