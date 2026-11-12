use crate::audio_selection::{Candidate, choose_microphone, playback_is_router};
use crate::encoder::Router;
use crate::{Result, system};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicI64, Ordering},
    mpsc,
};
use std::{thread, time::Duration};
use wasapi::*;

fn microphone_selection(enumerator: &DeviceEnumerator) -> Result<(Vec<Candidate>, Option<usize>)> {
    let devices = enumerator.get_device_collection(Direction::Capture)?;
    let candidates = (0..devices.get_nbr_devices()?)
        .map(|i| {
            let device = devices.get_device_at_index(i)?;
            Ok(Candidate {
                id: device.get_id()?,
                name: device.get_friendlyname()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let default = enumerator
        .get_default_device(Direction::Capture)
        .ok()
        .and_then(|d| d.get_id().ok());
    let communications = enumerator
        .get_default_device_for_role(Direction::Capture, Role::Communications)
        .ok()
        .and_then(|d| d.get_id().ok());
    let selected = choose_microphone(&candidates, default.as_deref(), communications.as_deref());
    Ok((candidates, selected))
}

const RATE: i64 = 48_000;
const RING_FRAMES: usize = 96_000;
const LATENCY_HNS: i64 = 1_500_000; // 150 ms for device scheduling and capture packets.

struct Com;
impl Com {
    fn new() -> Result<Self> {
        initialize_mta().ok()?;
        Ok(Self)
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        deinitialize();
    }
}

pub fn list_devices() -> Result<()> {
    let _com = Com::new()?;
    let enumerator = DeviceEnumerator::new()?;
    let (microphones, selected) = microphone_selection(&enumerator)?;
    for (direction, label) in [
        (Direction::Render, "Выходы звука"),
        (Direction::Capture, "Микрофоны и другие входы"),
    ] {
        println!("{label}:");
        let devices = enumerator.get_device_collection(direction)?;
        for i in 0..devices.get_nbr_devices()? {
            let device = devices.get_device_at_index(i)?;
            let name = device.get_friendlyname()?;
            let note = if direction == Direction::Render {
                if playback_is_router(&name) {
                    " [пропущен: виртуальная маршрутизация]"
                } else {
                    " [записывается]"
                }
            } else if selected
                .is_some_and(|s| microphones[s].id == device.get_id().unwrap_or_default())
            {
                " [выбранный микрофон]"
            } else {
                " [пропущен]"
            };
            println!("  {name}{note}");
        }
    }
    Ok(())
}

struct Input {
    client: AudioClient,
    capture: AudioCaptureClient,
    bytes: Vec<u8>,
    name: String,
    frames: u64,
    peak: f32,
}
fn inputs() -> Result<Vec<Input>> {
    let enumerator = DeviceEnumerator::new()?;
    let (microphones, selected) = microphone_selection(&enumerator)?;
    let selected_id = selected.map(|s| microphones[s].id.as_str());
    if let Some(index) = selected {
        println!("Выбран микрофон: {}", microphones[index].name);
    } else {
        println!("Подходящий микрофон не найден; записывается только системный звук.");
    }
    let mut inputs = Vec::new();
    for direction in [Direction::Render, Direction::Capture] {
        let devices = enumerator.get_device_collection(direction)?;
        for index in 0..devices.get_nbr_devices()? {
            let device = devices.get_device_at_index(index)?;
            let name = device.get_friendlyname()?;
            if direction == Direction::Capture && Some(device.get_id()?.as_str()) != selected_id {
                println!("Пропущен вход: {name}");
                continue;
            }
            if direction == Direction::Render && playback_is_router(&name) {
                println!("Пропущен виртуальный выход: {name}");
                continue;
            }
            let mut client = device.get_iaudioclient()?;
            let format = WaveFormat::new(32, 32, SampleType::Float, RATE as usize, 2, None);
            client
                .initialize_client(
                    &format,
                    Direction::Capture,
                    StreamMode::PollingShared {
                        autoconvert: true,
                        buffer_duration_hns: 1_000_000,
                    },
                    None,
                )
                .map_err(|e| format!("Не удалось открыть аудиоустройство «{name}»: {e}"))?;
            let capture = client.get_audiocaptureclient()?;
            let bytes = vec![0; client.get_buffer_size()? as usize * 8];
            client.start_stream()?;
            println!("Аудио {:?}: {name}", direction);
            inputs.push(Input {
                client,
                capture,
                bytes,
                name,
                frames: 0,
                peak: 0.0,
            });
        }
    }
    if inputs.is_empty() {
        return Err("Не найдено ни одного активного аудиоустройства".into());
    }
    Ok(inputs)
}

// Ring slots correspond to absolute sample positions, shared by all devices.
// QPC packet timestamps align silence, independent device clocks and video.
struct Mixer {
    ring: Vec<[f32; 2]>,
    cursor: i64,
}
impl Mixer {
    fn new() -> Self {
        Self {
            ring: vec![[0.0; 2]; RING_FRAMES],
            cursor: 0,
        }
    }
    fn add(&mut self, first: i64, bytes: &[u8]) -> Result<()> {
        for (offset, frame) in bytes.as_chunks::<8>().0.iter().enumerate() {
            let pos = first + offset as i64;
            if pos < self.cursor {
                continue;
            }
            if pos - self.cursor >= RING_FRAMES as i64 {
                return Err("Аудиопоток отстал более чем на две секунды".into());
            }
            let slot = &mut self.ring[pos as usize % RING_FRAMES];
            for ch in 0..2 {
                let value = f32::from_le_bytes(frame[ch * 4..ch * 4 + 4].try_into().unwrap());
                if value.is_finite() {
                    slot[ch] += value;
                }
            }
        }
        Ok(())
    }
    fn take(&mut self, frames: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(frames * 4);
        for _ in 0..frames {
            let slot = &mut self.ring[self.cursor as usize % RING_FRAMES];
            for value in slot.iter() {
                bytes.extend_from_slice(
                    &((value.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes(),
                );
            }
            *slot = [0.0; 2];
            self.cursor += 1;
        }
        bytes
    }
}

pub struct AudioThread {
    pub thread: thread::JoinHandle<Result<()>>,
    pub end: Arc<AtomicI64>,
}

pub fn start(encoder: Arc<Mutex<Router>>, epoch: Arc<AtomicI64>) -> Result<AudioThread> {
    let end = Arc::new(AtomicI64::new(0));
    let end_worker = end.clone();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let thread = thread::spawn(move || {
        let _com = Com::new()?;
        let mut inputs = match inputs() {
            Ok(inputs) => {
                let _ = ready_tx.send(Ok(()));
                inputs
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e.to_string()));
                return Err(e);
            }
        };
        let mut mixer = Mixer::new();
        let result = (|| -> Result<()> {
            loop {
                let origin = epoch.load(Ordering::Acquire);
                for input in &mut inputs {
                    // Drain queued packets each tick. Buffer size bounds each packet.
                    while input.capture.get_next_packet_size()?.unwrap_or(0) > 0 {
                        let (frames, info) = input
                            .capture
                            .read_from_device(&mut input.bytes)
                            .map_err(|e| format!("Потерян источник «{}»: {e}", input.name))?;
                        input.frames += frames as u64;
                        for sample in input.bytes[..frames as usize * 8].as_chunks::<4>().0 {
                            input.peak = input.peak.max(f32::from_le_bytes(*sample).abs());
                        }
                        if origin == 0 {
                            continue;
                        }
                        let timestamp = if info.flags.timestamp_error {
                            system::qpc_hns()? - frames as i64 * 10_000_000 / RATE
                        } else {
                            info.timestamp as i64
                        };
                        let first =
                            ((timestamp - origin) as i128 * RATE as i128 / 10_000_000) as i64;
                        mixer.add(first, &input.bytes[..frames as usize * 8])?;
                    }
                }
                let ending = end_worker.load(Ordering::Acquire);
                if origin != 0 {
                    let now = system::qpc_hns()?;
                    let until = if ending > 0 {
                        ending
                    } else {
                        now - LATENCY_HNS
                    };
                    let target =
                        ((until - origin).max(0) as i128 * RATE as i128 / 10_000_000) as i64;
                    while target > mixer.cursor {
                        let frames = (target - mixer.cursor).min(480) as usize;
                        let start_sample = mixer.cursor;
                        let bytes = mixer.take(frames);
                        encoder
                            .lock()
                            .map_err(|_| "Ошибка блокировки кодировщика")?
                            .send_audio(&bytes, start_sample)?;
                    }
                }
                if ending > 0 {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        })();
        for input in inputs {
            let _ = input.client.stop_stream();
            println!(
                "Источник «{}»: {} кадров аудио, пик {:.5}",
                input.name, input.frames, input.peak
            );
        }
        result
    });
    match ready_rx.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(())) => Ok(AudioThread { thread, end }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e.into())
        }
        Err(e) => {
            end.store(1, Ordering::Release);
            Err(format!("Таймаут запуска аудио: {e}").into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stereo(value: f32, frames: usize) -> Vec<u8> {
        value.to_le_bytes().repeat(frames * 2)
    }
    #[test]
    fn mix_align_silence_and_clip() {
        let mut mix = Mixer::new();
        mix.add(-1, &stereo(0.25, 2)).unwrap();
        mix.add(2, &stereo(0.75, 2)).unwrap();
        mix.add(2, &stereo(0.75, 1)).unwrap();
        let bytes = mix.take(5);
        let samples: Vec<_> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        assert_eq!(
            samples,
            vec![8191, 8191, 0, 0, 32767, 32767, 24575, 24575, 0, 0]
        );
        assert!(mix.ring.iter().all(|v| *v == [0.0; 2]));
    }
    #[test]
    fn ring_wrap_and_bounds() {
        let mut mix = Mixer::new();
        mix.take(RING_FRAMES - 1);
        mix.add(RING_FRAMES as i64 - 1, &stereo(1.0, 2)).unwrap();
        assert_eq!(mix.take(2), i16::MAX.to_le_bytes().repeat(4));
        assert!(
            mix.add(mix.cursor + RING_FRAMES as i64, &stereo(1.0, 1))
                .is_err()
        );
    }
}
