
use eframe::{
    egui::{
        self, Align, Align2, Color32, ColorImage, ComboBox, FontId, Frame, Layout, Rect, RichText,
        Rounding, ScrollArea, Sense, Shape, Stroke, TextEdit, TextureHandle, TextureOptions, Vec2,
    },
    App,
};
use libloading::{Library, Symbol};
use std::{
    ffi::{c_char, c_int, CStr},
    fs,
    path::{Path, PathBuf},
};

const APP_TITLE: &str = "ByteForge 2000";
const PLUGIN_DIR: &str = "plugins";
const HEX_ROW_LEN: usize = 16;
const MAX_HEX_ROWS: usize = 4096; // защита от зависания на огромных файлах

#[repr(C)]
struct CPluginInfo {
    name: *const c_char,
    description: *const c_char,
    plugin_type: c_int,
    version: *const c_char,
}

#[repr(C)]
struct CPluginRequest {
    data: *const u8,
    length: usize,
    offset_hint: usize,
}

#[repr(C)]
struct CPluginResult {
    out_data: *mut u8,
    out_capacity: usize,
    out_written: usize,
    log_buffer: *mut c_char,
    log_capacity: usize,
}

fn c_str_or(ptr: *const c_char, fallback: &str) -> String {
    if ptr.is_null() {
        return fallback.to_string();
    }
    unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() }
}

fn plugin_type_name(t: c_int) -> &'static str {
    match t {
        0 => "analyzer",
        1 => "transformer",
        2 => "audio processor",
        3 => "visualization plugin",
        4 => "repair plugin",
        _ => "unknown",
    }
}

#[derive(Clone)]
struct LoadedPlugin {
    file_name: String,
    path: String,
    name: String,
    description: String,
    kind: String,
    version: String,
    status: String,
}


fn probe_plugin(path: &Path) -> LoadedPlugin {
    let file_name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let path_str = path.to_string_lossy().to_string();

    unsafe {
        let lib = match Library::new(path) {
            Ok(lib) => lib,
            Err(err) => {
                return LoadedPlugin {
                    file_name,
                    path: path_str,
                    name: "?".into(),
                    description: format!("Failed to load: {err}"),
                    kind: "unknown".into(),
                    version: "-".into(),
                    status: "FAILED".into(),
                };
            }
        };

        let get_info: Result<Symbol<unsafe extern "C" fn() -> *const CPluginInfo>, _> =
            lib.get(b"plugin_get_info");

        match get_info {
            Ok(func) => {
                let info_ptr = func();
                if info_ptr.is_null() {
                    LoadedPlugin {
                        file_name,
                        path: path_str,
                        name: "?".into(),
                        description: "plugin_get_info returned NULL".into(),
                        kind: "unknown".into(),
                        version: "-".into(),
                        status: "LOAD ERROR".into(),
                    }
                } else {
                    let info = &*info_ptr;
                    LoadedPlugin {
                        file_name,
                        path: path_str,
                        name: c_str_or(info.name, "Unnamed plugin"),
                        description: c_str_or(info.description, "No description"),
                        kind: plugin_type_name(info.plugin_type).to_string(),
                        version: c_str_or(info.version, "-"),
                        status: "READY".into(),
                    }
                }
            }
            Err(_) => LoadedPlugin {
                file_name,
                path: path_str,
                name: "?".into(),
                description: "Missing symbol plugin_get_info".into(),
                kind: "unknown".into(),
                version: "-".into(),
                status: "INVALID ABI".into(),
            },
        }
    }
}


fn run_plugin(path: &str, bytes: &[u8], offset_hint: usize) -> Result<String, String> {
    unsafe {
        let lib = Library::new(path).map_err(|e| format!("load failed: {e}"))?;
        let func: Symbol<unsafe extern "C" fn(*const CPluginRequest, *mut CPluginResult) -> c_int> =
            lib.get(b"plugin_process").map_err(|e| format!("missing plugin_process: {e}"))?;

        let request = CPluginRequest {
            data: bytes.as_ptr(),
            length: bytes.len(),
            offset_hint,
        };

        let mut log_buf = vec![0_u8; 4096];
        let mut result = CPluginResult {
            out_data: std::ptr::null_mut(),
            out_capacity: 0,
            out_written: 0,
            log_buffer: log_buf.as_mut_ptr() as *mut c_char,
            log_capacity: log_buf.len(),
        };

        let ret = func(&request as *const CPluginRequest, &mut result as *mut CPluginResult);
        let text = CStr::from_ptr(log_buf.as_ptr() as *const c_char)
            .to_string_lossy()
            .into_owned();

        if ret == 0 {
            Ok(text)
        } else {
            Err(format!("plugin returned code {ret}: {text}"))
        }
    }
}


#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SegmentKind {
    Attack,
    Sustain,
    Release,
    Silence,
    Noise,
}

impl SegmentKind {
    fn label(&self) -> &'static str {
        match self {
            SegmentKind::Attack => "Attack",
            SegmentKind::Sustain => "Sustain",
            SegmentKind::Release => "Release",
            SegmentKind::Silence => "Silence",
            SegmentKind::Noise => "Noise",
        }
    }

    fn all() -> [SegmentKind; 5] {
        [
            SegmentKind::Attack,
            SegmentKind::Sustain,
            SegmentKind::Release,
            SegmentKind::Silence,
            SegmentKind::Noise,
        ]
    }
}

#[derive(Clone)]
struct AudioTrack {
    label: String,
    kind: SegmentKind,
    start_ms: f64,
    end_ms: f64,
    start_byte: usize,
    end_byte: usize,
    peak_amplitude: f32,
}

struct WavFmt {
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
    audio_format: u16,
}


fn find_wave_chunks(bytes: &[u8]) -> Option<(WavFmt, usize, usize)> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }

    let mut pos = 12;
    let mut fmt: Option<WavFmt> = None;
    let mut data_range: Option<(usize, usize)> = None;

    while pos + 8 <= bytes.len() {
        let chunk_id = &bytes[pos..pos + 4];
        let chunk_size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap_or([0; 4])) as usize;
        let body_start = pos + 8;
        let body_end = (body_start + chunk_size).min(bytes.len());

        if chunk_id == b"fmt " && body_end - body_start >= 16 {
            let f = &bytes[body_start..body_end];
            fmt = Some(WavFmt {
                audio_format: u16::from_le_bytes([f[0], f[1]]),
                channels: u16::from_le_bytes([f[2], f[3]]),
                sample_rate: u32::from_le_bytes([f[4], f[5], f[6], f[7]]),
                bits_per_sample: u16::from_le_bytes([f[14], f[15]]),
            });
        } else if chunk_id == b"data" {
            data_range = Some((body_start, body_end));
        }

        pos = body_start + chunk_size + (chunk_size % 2); // WAVE-чанки выровнены по 2 байта
        if chunk_size == 0 {
            break;
        }
    }

    match (fmt, data_range) {
        (Some(f), Some((s, e))) => Some((f, s, e)),
        _ => None,
    }
}


fn decode_pcm_mono(bytes: &[u8], fmt: &WavFmt) -> Option<Vec<f32>> {
    if fmt.audio_format != 1 {
        return None; // поддерживаем только целочисленный PCM
    }
    let channels = fmt.channels.max(1) as usize;

    match fmt.bits_per_sample {
        8 => {
            let frame = channels;
            let samples: Vec<f32> = bytes
                .chunks_exact(frame)
                .map(|f| {
                    let sum: i32 = f.iter().map(|&b| b as i32 - 128).sum();
                    (sum as f32 / frame as f32) / 128.0
                })
                .collect();
            Some(samples)
        }
        16 => {
            let frame = 2 * channels;
            let samples: Vec<f32> = bytes
                .chunks_exact(frame)
                .map(|f| {
                    let mut sum = 0_i32;
                    for c in 0..channels {
                        let lo = f[c * 2];
                        let hi = f[c * 2 + 1];
                        sum += i16::from_le_bytes([lo, hi]) as i32;
                    }
                    (sum as f32 / channels as f32) / i16::MAX as f32
                })
                .collect();
            Some(samples)
        }
        _ => None,
    }
}


fn segment_samples(samples: &[f32], sample_rate: u32, byte_offset: usize, bytes_per_sample_frame: usize) -> Vec<AudioTrack> {
    const FRAME: usize = 1024;
    const SILENCE_THRESHOLD: f32 = 0.02;
    const NOISE_ZCR_THRESHOLD: f32 = 0.35;

    if samples.is_empty() {
        return Vec::new();
    }

    #[derive(PartialEq, Clone, Copy)]
    enum Class {
        Sound,
        Silence,
    }

    let mut frames: Vec<(Class, f32, f32)> = Vec::new(); // (класс, rms, zcr)
    for chunk in samples.chunks(FRAME) {
        let rms = (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len().max(1) as f32).sqrt();
        let mut crossings = 0usize;
        for w in chunk.windows(2) {
            if (w[0] >= 0.0) != (w[1] >= 0.0) {
                crossings += 1;
            }
        }
        let zcr = crossings as f32 / chunk.len().max(1) as f32;
        let class = if rms > SILENCE_THRESHOLD { Class::Sound } else { Class::Silence };
        frames.push((class, rms, zcr));
    }

  
    let mut runs: Vec<(Class, usize, usize)> = Vec::new(); // (класс, start_frame, end_frame_excl)
    let mut run_start = 0usize;
    for i in 1..frames.len() {
        if frames[i].0 != frames[run_start].0 {
            runs.push((frames[run_start].0, run_start, i));
            run_start = i;
        }
    }
    runs.push((frames[run_start].0, run_start, frames.len()));

    let samples_to_ms = |sample_idx: usize| -> f64 {
        if sample_rate == 0 {
            0.0
        } else {
            sample_idx as f64 * 1000.0 / sample_rate as f64
        }
    };

    let mut tracks = Vec::new();
    let mut track_no = 1;

    for (class, start_f, end_f) in runs {
        let start_sample = start_f * FRAME;
        let end_sample = (end_f * FRAME).min(samples.len());
        let peak = samples[start_sample..end_sample]
            .iter()
            .fold(0.0_f32, |acc, s| acc.max(s.abs()));

        let start_ms = samples_to_ms(start_sample);
        let end_ms = samples_to_ms(end_sample);
        let start_byte = byte_offset + start_sample * bytes_per_sample_frame;
        let end_byte = byte_offset + end_sample * bytes_per_sample_frame;

        if class == Class::Silence {
            tracks.push(AudioTrack {
                label: format!("Track {track_no}"),
                kind: SegmentKind::Silence,
                start_ms,
                end_ms,
                start_byte,
                end_byte,
                peak_amplitude: peak,
            });
            track_no += 1;
            continue;
        }

        let run_len = end_f - start_f;
        let avg_zcr: f32 = frames[start_f..end_f].iter().map(|f| f.2).sum::<f32>() / run_len.max(1) as f32;

        if avg_zcr > NOISE_ZCR_THRESHOLD {
            tracks.push(AudioTrack {
                label: format!("Track {track_no}"),
                kind: SegmentKind::Noise,
                start_ms,
                end_ms,
                start_byte,
                end_byte,
                peak_amplitude: peak,
            });
            track_no += 1;
            continue;
        }

       
        if run_len <= 2 {
            tracks.push(AudioTrack {
                label: format!("Track {track_no}"),
                kind: SegmentKind::Sustain,
                start_ms,
                end_ms,
                start_byte,
                end_byte,
                peak_amplitude: peak,
            });
            track_no += 1;
        } else {
            let attack_frames = (run_len as f32 * 0.1).ceil().max(1.0) as usize;
            let release_frames = attack_frames;
            let sustain_frames = run_len.saturating_sub(attack_frames + release_frames).max(1);

            let bounds = [
                (SegmentKind::Attack, start_f, start_f + attack_frames),
                (SegmentKind::Sustain, start_f + attack_frames, start_f + attack_frames + sustain_frames),
                (SegmentKind::Release, start_f + attack_frames + sustain_frames, end_f),
            ];

            for (kind, sf, ef) in bounds {
                let sf = sf.min(frames.len());
                let ef = ef.min(frames.len()).max(sf);
                if ef == sf {
                    continue;
                }
                let s_sample = sf * FRAME;
                let e_sample = (ef * FRAME).min(samples.len());
                let sub_peak = samples[s_sample..e_sample].iter().fold(0.0_f32, |a, s| a.max(s.abs()));
                tracks.push(AudioTrack {
                    label: format!("Track {track_no}"),
                    kind,
                    start_ms: samples_to_ms(s_sample),
                    end_ms: samples_to_ms(e_sample),
                    start_byte: byte_offset + s_sample * bytes_per_sample_frame,
                    end_byte: byte_offset + e_sample * bytes_per_sample_frame,
                    peak_amplitude: sub_peak,
                });
                track_no += 1;
            }
        }
    }

    tracks
}


fn analyze_audio(bytes: &[u8]) -> (Vec<AudioTrack>, String) {
    if let Some((fmt, data_start, data_end)) = find_wave_chunks(bytes) {
        let data = &bytes[data_start..data_end];
        if let Some(samples) = decode_pcm_mono(data, &fmt) {
            let bytes_per_frame = fmt.channels.max(1) as usize * (fmt.bits_per_sample / 8).max(1) as usize;
            let tracks = segment_samples(&samples, fmt.sample_rate, data_start, bytes_per_frame);
            let info = format!(
                "WAVE PCM detected: {} ch, {} Hz, {} bit — {} lane(s) generated",
                fmt.channels,
                fmt.sample_rate,
                fmt.bits_per_sample,
                tracks.len()
            );
            return (tracks, info);
        }
        return (
            Vec::new(),
            format!(
                "WAVE container found but audio_format={} / {}-bit is not supported yet",
                fmt.audio_format, fmt.bits_per_sample
            ),
        );
    }

    if bytes.len() < 64 {
        return (Vec::new(), "File too small for audio heuristics".to_string());
    }

    // Эвристический фоллбэк: трактуем сырые байты как 8-битный PCM
    // и запускаем ту же сегментацию, помечая результат как heuristic.
    let pseudo_samples: Vec<f32> = bytes.iter().map(|&b| (b as f32 - 128.0) / 128.0).collect();
    let tracks = segment_samples(&pseudo_samples, 44100, 0, 1);
    let info = format!(
        "No RIFF/WAVE header — heuristic byte-energy scan produced {} candidate lane(s)",
        tracks.len()
    );
    (tracks, info)
}


const SIGNATURES: &[(&[u8], &str)] = &[
    (b"\x89PNG\r\n\x1a\n", "PNG image"),
    (&[0xFF, 0xD8, 0xFF], "JPEG image"),
    (b"GIF87a", "GIF image"),
    (b"GIF89a", "GIF image"),
    (b"PK\x03\x04", "ZIP / Office / JAR archive"),
    (b"%PDF", "PDF document"),
    (b"\x7FELF", "ELF executable"),
    (b"MZ", "Windows PE executable"),
    (b"RIFF", "RIFF container (WAV/AVI/...)"),
    (b"ID3", "MP3 with ID3 tag"),
    (b"OggS", "Ogg container"),
    (b"\x1F\x8B", "GZIP archive"),
    (b"7z\xBC\xAF\x27\x1C", "7-Zip archive"),
    (b"Rar!", "RAR archive"),
    (b"SQLite format 3", "SQLite database"),
];

fn shannon_entropy(bytes: &[u8]) -> f32 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut histogram = [0u32; 256];
    for &b in bytes {
        histogram[b as usize] += 1;
    }
    let len = bytes.len() as f32;
    histogram
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f32 / len;
            -p * p.log2()
        })
        .sum()
}

fn detect_signatures(bytes: &[u8]) -> Vec<String> {
    let mut hits = Vec::new();
    let scan_limit = bytes.len().min(65536);
    let window = &bytes[..scan_limit];

    for (sig, name) in SIGNATURES {
        if window.starts_with(sig) {
            hits.push(format!("offset 0x0: {name}"));
            continue;
        }
        if let Some(pos) = window
            .windows(sig.len())
            .position(|w| w == *sig)
        {
            if pos != 0 {
                hits.push(format!("offset {pos:#06X}: {name} (embedded)"));
            }
        }
    }

    if hits.is_empty() {
        hits.push("No known signatures found".to_string());
    }
    hits
}

fn ai_verdict(entropy: f32, printable_ratio: f32) -> &'static str {
    if entropy > 7.5 {
        "Likely compressed or encrypted data"
    } else if printable_ratio > 0.85 {
        "Likely plain text / source data"
    } else if entropy < 3.0 {
        "Likely sparse / padded / repetitive structure"
    } else {
        "Likely structured binary format"
    }
}



#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Hex,
    Ascii,
    Preview,
    Audio,
    Console,
    Analysis,
    Library,
}

struct ByteForge2000App {
    file_name: String,
    file_path: Option<PathBuf>,
    offset: String,
    new_byte: String,
    status: String,
    tab: Tab,
    edit_mode: bool,

    log_lines: Vec<String>,
    bytes: Vec<u8>,

    hex_lines: Vec<String>,
    file_size: usize,
    printable_ratio: f32,
    suspicious_count: usize,
    entropy: f32,
    signatures: Vec<String>,
    ai_summary: String,

    audio_tracks: Vec<AudioTrack>,
    audio_info: String,
    selected_track: usize,

    plugins: Vec<LoadedPlugin>,
    plugin_run_output: String,
    plugin_library_open: bool,

    
    decoded_image: Option<image::RgbaImage>,
    preview_texture: Option<TextureHandle>,

    
    hex_edit_text: String,
    ascii_edit_text: String,
}

impl Default for ByteForge2000App {
    fn default() -> Self {
        let mut app = Self {
            file_name: String::new(),
            file_path: None,
            offset: String::new(),
            new_byte: String::new(),
            status: "Ready. Open a file to begin.".to_string(),
            tab: Tab::Hex,
            edit_mode: false,
            log_lines: Vec::new(),
            bytes: Vec::new(),
            hex_lines: Vec::new(),
            file_size: 0,
            printable_ratio: 0.0,
            suspicious_count: 0,
            entropy: 0.0,
            signatures: Vec::new(),
            ai_summary: "Open a file, then press AI ANALYSIS.".to_string(),
            audio_tracks: Vec::new(),
            audio_info: "No file loaded".to_string(),
            selected_track: 0,
            plugins: Vec::new(),
            plugin_run_output: String::new(),
            plugin_library_open: false,
            decoded_image: None,
            preview_texture: None,
            hex_edit_text: String::new(),
            ascii_edit_text: String::new(),
        };
        app.log("[SYSTEM] ByteForge 2000 started");
        app.reload_plugins();
        app
    }
}

impl ByteForge2000App {
    fn log(&mut self, line: impl Into<String>) {
        self.log_lines.push(line.into());
        if self.log_lines.len() > 2000 {
            self.log_lines.drain(0..500);
        }
    }

    fn load_file(&mut self, path: PathBuf) {
        match fs::read(&path) {
            Ok(bytes) => {
                self.file_path = Some(path.clone());
                self.file_name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                self.bytes = bytes;
                self.offset.clear();
                self.new_byte.clear();
                self.selected_track = 0;
                self.status = format!("Loaded {}", self.file_name);
                self.log(format!("[SYSTEM] Loaded {} ({} bytes)", self.file_name, self.bytes.len()));
                self.refresh_all();
            }
            Err(err) => {
                self.status = format!("Open failed: {err}");
                self.log(format!("[ERROR] Could not read file: {err}"));
            }
        }
    }

    fn save_current_file(&mut self) {
        let Some(path) = self.file_path.clone() else {
            self.status = "Nothing to save yet".to_string();
            return;
        };
        match fs::write(&path, &self.bytes) {
            Ok(_) => {
                self.status = format!("Saved {}", self.file_name);
                self.log(format!("[SYSTEM] Saved {}", self.file_name));
            }
            Err(err) => {
                self.status = format!("Save failed: {err}");
                self.log(format!("[ERROR] Save failed: {err}"));
            }
        }
    }

    fn apply_byte_change(&mut self) {
        if self.file_path.is_none() {
            self.status = "Open a file first".to_string();
            return;
        }
        let offset = match self.offset.trim().parse::<usize>() {
            Ok(v) => v,
            Err(_) => {
                self.status = "Offset must be a non-negative number".to_string();
                return;
            }
        };
        let byte = match self.new_byte.trim().parse::<u8>() {
            Ok(v) => v,
            Err(_) => {
                self.status = "Byte must be 0-255".to_string();
                return;
            }
        };
        if offset >= self.bytes.len() {
            self.status = format!("Offset {offset} is out of range (file is {} bytes)", self.bytes.len());
            return;
        }

        let old = self.bytes[offset];
        self.bytes[offset] = byte;
        self.status = format!("Applied byte {byte} at offset {offset}");
        self.log(format!("[CHANGE] offset={offset} old={old} new={byte}"));
        self.refresh_all();
    }

    fn sync_edit_buffers(&mut self) {
        self.hex_edit_text = self
            .bytes
            .iter()
            .enumerate()
            .map(|(idx, b)| format!("{idx:08X}:{b:02X}"))
            .collect::<Vec<_>>()
            .join("\n");

        self.ascii_edit_text = self
            .bytes
            .iter()
            .map(|&b| if (32..=126).contains(&b) || matches!(b, 9 | 10 | 13) { b as char } else { '.' })
            .collect();
    }

    fn apply_hex_edit(&mut self) {
        let mut new_bytes = Vec::new();
        for line in self.hex_edit_text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let mut parts = trimmed.split(':');
            let _offset = parts.next();
            let hex_part = parts.next().unwrap_or(trimmed);
            let bytes_str = hex_part.split_whitespace().collect::<Vec<_>>().join("");
            if bytes_str.is_empty() {
                continue;
            }
            for chunk in bytes_str.chars().collect::<Vec<_>>().chunks(2) {
                let token: String = chunk.iter().collect();
                let byte = u8::from_str_radix(&token, 16);
                match byte {
                    Ok(value) => new_bytes.push(value),
                    Err(_) => {
                        self.status = "HEX edit contains invalid byte values".to_string();
                        self.log("[ERROR] HEX edit contains invalid byte values".to_string());
                        return;
                    }
                }
            }
        }
        self.bytes = new_bytes;
        self.status = "HEX edit applied".to_string();
        self.log("[CHANGE] HEX edit applied".to_string());
        self.refresh_all();
    }

    fn apply_ascii_edit(&mut self) {
        let text = self.ascii_edit_text.clone();
        self.bytes = text
            .chars()
            .map(|ch| match ch {
                '\n' | '\r' | '\t' => ch as u8,
                _ if ch.is_ascii() => ch as u8,
                _ => '.' as u8,
            })
            .collect();
        self.status = "ASCII edit applied".to_string();
        self.log("[CHANGE] ASCII edit applied".to_string());
        self.refresh_all();
    }

    fn refresh_all(&mut self) {
        self.file_size = self.bytes.len();

        let printable = self
            .bytes
            .iter()
            .filter(|&&b| (32..=126).contains(&b) || matches!(b, 9 | 10 | 13))
            .count();
        self.printable_ratio = if self.file_size > 0 {
            printable as f32 / self.file_size as f32
        } else {
            0.0
        };
        self.suspicious_count = self.file_size - printable;

        self.entropy = shannon_entropy(&self.bytes);
        self.signatures = detect_signatures(&self.bytes);
        self.hex_lines = Self::build_hex_lines(&self.bytes);
        self.sync_edit_buffers();

        let (tracks, info) = analyze_audio(&self.bytes);
        self.log(format!("[AUDIO] {info}"));
        self.audio_tracks = tracks;
        self.audio_info = info;
        self.selected_track = 0;

        
        self.decoded_image = image::load_from_memory(&self.bytes).ok().map(|img| img.to_rgba8());
        self.preview_texture = None;
        if self.decoded_image.is_some() {
            self.log("[SYSTEM] Preview: decoded as image".to_string());
        }

        self.refresh_ai_summary();
    }

    fn refresh_ai_summary(&mut self) {
        let verdict = ai_verdict(self.entropy, self.printable_ratio);
        self.ai_summary = format!(
            "File size: {} bytes\nPrintable ratio: {:.2}%\nSuspicious bytes: {}\nShannon entropy: {:.3} bits/byte\nAudio lanes: {}\n\nSignatures:\n{}\n\nVerdict: {}",
            self.file_size,
            self.printable_ratio * 100.0,
            self.suspicious_count,
            self.entropy,
            self.audio_tracks.len(),
            self.signatures.join("\n"),
            verdict,
        );
        self.log(format!("[AI] entropy={:.3} verdict={}", self.entropy, verdict));
    }

    fn build_hex_lines(bytes: &[u8]) -> Vec<String> {
        if bytes.is_empty() {
            return vec!["No file loaded yet.".to_string()];
        }
        let mut lines = Vec::new();
        for (i, chunk) in bytes.chunks(HEX_ROW_LEN).enumerate() {
            if i >= MAX_HEX_ROWS {
                lines.push(format!("... truncated, showing first {MAX_HEX_ROWS} rows ..."));
                break;
            }
            let offset = i * HEX_ROW_LEN;
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02X}")).collect();
            let ascii: String = chunk
                .iter()
                .map(|&b| if (32..=126).contains(&b) { b as char } else { '.' })
                .collect();
            lines.push(format!("{offset:08X}: {:<47} | {ascii}", hex.join(" ")));
        }
        lines
    }

    fn open_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            self.load_file(path);
        }
    }

    fn add_plugin_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("Native plugins", &["dll", "so", "dylib"]).pick_file() else {
            return;
        };

        let target = PathBuf::from(PLUGIN_DIR).join(path.file_name().unwrap_or_default());
        if let Err(err) = fs::copy(&path, &target) {
            self.status = format!("Failed to add plugin: {err}");
            self.log(format!("[ERROR] Failed to add plugin: {err}"));
            return;
        }

        self.status = format!("Added plugin {}", target.display());
        self.log(format!("[SYSTEM] Added plugin {}", target.display()));
        self.reload_plugins();
    }

    fn load_plugins_from_directory(&mut self) {
        self.plugins.clear();
        let base = PathBuf::from(PLUGIN_DIR);
        if !base.exists() {
            let _ = fs::create_dir_all(&base);
        }

        let mut entries: Vec<PathBuf> = fs::read_dir(&base)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        entries.sort();

        for path in entries {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            if matches!(ext.as_str(), "dll" | "so" | "dylib") {
                let plugin = probe_plugin(&path);
                self.log(format!("[PLUGIN] {} -> {} ({}) [{}]", plugin.file_name, plugin.name, plugin.kind, plugin.status));
                self.plugins.push(plugin);
            }
        }
    }

    fn reload_plugins(&mut self) {
        self.load_plugins_from_directory();

        if self.plugins.is_empty() {
            self.log("[PLUGIN] No loadable .dll/.so/.dylib plugins found in plugins/");
        }
    }

    fn run_selected_plugin(&mut self, index: usize) {
        let Some(plugin) = self.plugins.get(index).cloned() else {
            return;
        };
        if plugin.status != "READY" {
            self.plugin_run_output = format!("Cannot run '{}': status is {}", plugin.name, plugin.status);
            return;
        }
        let offset_hint = self.offset.trim().parse::<usize>().unwrap_or(0);
        match run_plugin(&plugin.path, &self.bytes, offset_hint) {
            Ok(text) => {
                self.plugin_run_output = text.clone();
                self.log(format!("[PLUGIN] {} -> {}", plugin.name, text));
            }
            Err(err) => {
                self.plugin_run_output = format!("Error: {err}");
                self.log(format!("[ERROR] plugin '{}' failed: {err}", plugin.name));
            }
        }
    }
}

impl App for ByteForge2000App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        apply_dark_ribbon_theme(ctx);

        
        egui::TopBottomPanel::top("tab_bar")
            .exact_height(34.0)
            .frame(Frame::none().fill(Color32::from_rgb(22, 22, 24)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    ui.label(RichText::new(APP_TITLE).strong().size(13.0).color(Color32::from_gray(230)));
                    ui.add_space(18.0);

                    ribbon_tab(ui, &mut self.tab, Tab::Hex, "HEX");
                    ribbon_tab(ui, &mut self.tab, Tab::Ascii, "ASCII");
                    ribbon_tab(ui, &mut self.tab, Tab::Preview, "PREVIEW");
                    ribbon_tab(ui, &mut self.tab, Tab::Audio, "AUDIO");
                    ribbon_tab(ui, &mut self.tab, Tab::Console, "CONSOLE");
                    ribbon_tab(ui, &mut self.tab, Tab::Analysis, "ANALYSIS");
                    ribbon_tab(ui, &mut self.tab, Tab::Library, "LIBRARY");

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(14.0);

                        let display_name = if self.file_name.is_empty() {
                            "no file".to_string()
                        } else {
                            let file_name = self.file_name.clone();
                            if file_name.chars().count() > 24 {
                                let suffix: String = file_name
                                    .chars()
                                    .skip(file_name.chars().count() - 24)
                                    .collect();
                                format!("…{suffix}")
                            } else {
                                file_name
                            }
                        };

                        ui.label(RichText::new(display_name).size(11.5).color(Color32::from_gray(165)));
                    });
                });
            });

        // --- лента: группы кнопок с иконками и подписью группы ---
        egui::TopBottomPanel::top("ribbon_strip")
            .exact_height(96.0)
            .frame(
                Frame::none()
                    .fill(Color32::from_rgb(37, 37, 40))
                    .inner_margin(egui::Margin::symmetric(10.0_f32, 6.0_f32)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ribbon_group(ui, "File", 130.0, |ui| {
                        if ribbon_button(ui, IconKind::Open, "Open").clicked() {
                            self.open_file_dialog();
                        }
                        if ribbon_button(ui, IconKind::Save, "Save").clicked() {
                            self.save_current_file();
                        }
                    });

                    ribbon_group(ui, "Edit Bytes", 230.0, |ui| {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Offset").size(10.0).color(Color32::from_gray(170)));
                                ui.add(TextEdit::singleline(&mut self.offset).desired_width(64.0));
                            });
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Byte").size(10.0).color(Color32::from_gray(170)));
                                ui.add(TextEdit::singleline(&mut self.new_byte).desired_width(64.0));
                            });
                        });
                        ui.add_space(4.0);
                        if ribbon_button(ui, IconKind::Apply, "Apply").clicked() {
                            self.apply_byte_change();
                        }
                        let edit_label = if self.edit_mode { "Editing" } else { "Edit Mode" };
                        if ribbon_button(ui, IconKind::Edit, edit_label).clicked() {
                            self.edit_mode = !self.edit_mode;
                            self.status = if self.edit_mode { "Edit mode ON".into() } else { "Edit mode OFF".into() };
                        }
                    });

                    ribbon_group(ui, "Plugins", 130.0, |ui| {
                        if ribbon_button(ui, IconKind::Library, "Library").clicked() {
                            self.tab = Tab::Library;
                            self.plugin_library_open = true;
                        }
                        if ribbon_button(ui, IconKind::Reload, "Reload").clicked() {
                            self.reload_plugins();
                            self.status = "Plugins reloaded".to_string();
                        }
                    });

                    ribbon_group(ui, "Analysis", 76.0, |ui| {
                        if ribbon_button(ui, IconKind::Ai, "AI Scan").clicked() {
                            self.refresh_ai_summary();
                            self.tab = Tab::Analysis;
                            self.status = "AI analysis refreshed".to_string();
                        }
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(&self.status).size(11.5).color(Color32::from_gray(195)));
                    });
                });
            });

        if self.plugin_library_open {
            let mut plugin_library_open = self.plugin_library_open;
            egui::Window::new("Plugin Library")
                .open(&mut plugin_library_open)
                .default_size([420.0, 340.0])
                .show(ctx, |ui| {
                    self.draw_library_tab(ui);
                });
            self.plugin_library_open = plugin_library_open;
        }

        // --- содержимое активной вкладки ---
        egui::CentralPanel::default()
            .frame(Frame::default().fill(Color32::from_rgb(30, 30, 32)))
            .show(ctx, |ui| {
                let inner = Frame::default()
                    .fill(Color32::from_rgb(24, 24, 26))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(55, 55, 58)))
                    .inner_margin(egui::Margin::same(10.0_f32));

                inner.show(ui, |ui| {
                    ui.set_min_height(ui.available_height());
                    match self.tab {
                        Tab::Hex => self.draw_hex_tab(ui),
                        Tab::Ascii => self.draw_ascii_tab(ui),
                        Tab::Preview => self.draw_preview_tab(ui),
                        Tab::Audio => self.draw_audio_tab(ui),
                        Tab::Console => self.draw_console_tab(ui),
                        Tab::Analysis => self.draw_analysis_tab(ui),
                        Tab::Library => self.draw_library_tab(ui),
                    }
                });
            });
    }
}

impl ByteForge2000App {
    fn draw_hex_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("HEX DUMP").strong());
        ui.add_space(6.0);

        if self.edit_mode {
            ui.horizontal(|ui| {
                if ui.button("Apply HEX edit").clicked() {
                    self.apply_hex_edit();
                }
                if ui.button("Reset").clicked() {
                    self.sync_edit_buffers();
                }
            });
            ui.add_space(4.0);
            ui.add_sized(
                Vec2::new(ui.available_width(), ui.available_height() * 0.8),
                TextEdit::multiline(&mut self.hex_edit_text).desired_rows(28),
            );
            return;
        }

        ScrollArea::vertical().show(ui, |ui| {
            for line in &self.hex_lines {
                ui.monospace(line);
            }
        });
    }

    fn draw_ascii_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("ASCII VIEW").strong());
        ui.add_space(6.0);

        if self.edit_mode {
            ui.horizontal(|ui| {
                if ui.button("Apply ASCII edit").clicked() {
                    self.apply_ascii_edit();
                }
                if ui.button("Reset").clicked() {
                    self.sync_edit_buffers();
                }
            });
            ui.add_space(4.0);
            ui.add_sized(
                Vec2::new(ui.available_width(), ui.available_height() * 0.8),
                TextEdit::multiline(&mut self.ascii_edit_text).desired_rows(28),
            );
            return;
        }

        ScrollArea::vertical().show(ui, |ui| {
            if self.bytes.is_empty() {
                ui.label("Open a file to show ASCII data.");
            } else {
                let text: String = self
                    .bytes
                    .iter()
                    .take(1_000_000)
                    .map(|&b| if (32..=126).contains(&b) || matches!(b, 9 | 10 | 13) { b as char } else { '.' })
                    .collect();
                ui.monospace(text);
            }
        });
    }

    fn draw_preview_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("PREVIEW").strong().color(Color32::from_gray(235)));
        ui.add_space(6.0);
        if self.bytes.is_empty() {
            ui.label("Preview is empty until a file is opened.");
            return;
        }

        if let Some(rgba) = &self.decoded_image {
            let (w, h) = (rgba.width() as usize, rgba.height() as usize);
            let texture = self.preview_texture.get_or_insert_with(|| {
                let color_image = ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw());
                ui.ctx().load_texture("preview_image", color_image, TextureOptions::LINEAR)
            });

            let available = ui.available_size();
            let orig = Vec2::new(w as f32, h as f32);
            let scale = (available.x / orig.x).min((available.y - 60.0).max(50.0) / orig.y).min(1.0);
            let draw_size = orig * scale;

            ui.label(format!(
                "Image preview — {} x {} px, {} bytes",
                w, h, self.file_size
            ));
            ui.add_space(4.0);
            ui.add(egui::Image::new((texture.id(), draw_size)).rounding(Rounding::same(2.0)));
            return;
        }

        ui.label(format!("File: {}", self.file_name));
        ui.label(format!("Size: {} bytes", self.file_size));
        ui.label(format!("Printable ratio: {:.2}%", self.printable_ratio * 100.0));
        ui.label(format!("Suspicious bytes: {}", self.suspicious_count));
        ui.label(format!("Detected audio lanes: {}", self.audio_tracks.len()));
        ui.add_space(8.0);
        ui.label(RichText::new("Top signatures:").strong());
        for sig in self.signatures.iter().take(6) {
            ui.label(sig);
        }
        ui.add_space(10.0);
        ui.label(
            RichText::new("(No image decoder matched this file — showing structural stats instead.)")
                .italics()
                .size(11.0)
                .color(Color32::from_gray(150)),
        );
    }

    fn draw_audio_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("AUDIO LANE EDITOR").strong());
        ui.label(RichText::new(&self.audio_info).size(12.0).italics());
        ui.add_space(6.0);

        if self.audio_tracks.is_empty() {
            ui.label("No audio lanes for this file.");
            return;
        }

        let total_duration = self
            .audio_tracks
            .iter()
            .map(|track| track.end_ms)
            .fold(0.0_f64, f64::max)
            .max(1.0);

        let selected = self.selected_track;
        let mut clicked_index: Option<usize> = None;

        ui.columns(2, |cols| {
            ScrollArea::vertical().id_source("track_list").show(&mut cols[0], |ui| {
                for (i, track) in self.audio_tracks.iter().enumerate() {
                    let is_selected = selected == i;
                    let lane_color = match track.kind {
                        SegmentKind::Attack => Color32::from_rgb(85, 125, 255),
                        SegmentKind::Sustain => Color32::from_rgb(66, 160, 255),
                        SegmentKind::Release => Color32::from_rgb(130, 190, 255),
                        SegmentKind::Silence => Color32::from_rgb(150, 150, 150),
                        SegmentKind::Noise => Color32::from_rgb(210, 110, 110),
                    };

                    let row_height = 52.0;
                    let (row_rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_height), Sense::click());

                    if is_selected {
                        ui.painter().rect_filled(row_rect, Rounding::same(4.0), Color32::from_rgba_unmultiplied(80, 120, 255, 140));
                    } else {
                        ui.painter().rect_filled(row_rect, Rounding::same(4.0), Color32::from_rgba_unmultiplied(48, 48, 52, 180));
                    }
                    ui.painter().rect_stroke(row_rect, Rounding::same(4.0), Stroke::new(1.0_f32, Color32::from_rgb(120, 120, 120)));

                    ui.painter().text(
                        row_rect.left_top() + Vec2::new(8.0, 8.0),
                        Align2::LEFT_TOP,
                        format!("Track {} [{}]", i + 1, track.kind.label()),
                        FontId::proportional(13.0),
                        Color32::from_gray(240),
                    );

                    ui.painter().text(
                        row_rect.left_top() + Vec2::new(8.0, 24.0),
                        Align2::LEFT_TOP,
                        format!("{:.0}–{:.0} ms | 0x{:X}–0x{:X}", track.start_ms, track.end_ms, track.start_byte, track.end_byte),
                        FontId::proportional(11.0),
                        Color32::from_gray(200),
                    );

                    let lane_start = row_rect.left() + 8.0;
                    let lane_end = row_rect.right() - 8.0;
                    let lane_left = lane_start + ((track.start_ms / total_duration) as f32) * (lane_end - lane_start);
                    let lane_right = lane_start + ((track.end_ms / total_duration) as f32) * (lane_end - lane_start);
                    let lane_rect = Rect::from_min_max(
                        egui::pos2(lane_left, row_rect.center().y - 5.0),
                        egui::pos2(lane_right.max(lane_left + 12.0), row_rect.center().y + 5.0),
                    );

                    ui.painter().rect_filled(lane_rect, Rounding::same(3.0), lane_color);

                    if response.clicked() {
                        clicked_index = Some(i);
                    }
                }
            });

            let ui = &mut cols[1];
            let mut preview_label = None;
            if let Some(track) = self.audio_tracks.get_mut(selected) {
                ui.label(RichText::new("Track properties").strong());
                ui.add_space(4.0);

                ui.horizontal(|ui| {
                    ui.label("Label:");
                    ui.add(TextEdit::singleline(&mut track.label));
                });

                ui.horizontal(|ui| {
                    ui.label("Type:");
                    ComboBox::from_id_source("track_kind")
                        .selected_text(track.kind.label())
                        .show_ui(ui, |ui| {
                            for kind in SegmentKind::all() {
                                ui.selectable_value(&mut track.kind, kind, kind.label());
                            }
                        });
                });

                ui.add_space(6.0);
                ui.label(format!("Time range: {:.1} ms – {:.1} ms", track.start_ms, track.end_ms));
                ui.label(format!("Byte range: 0x{:X} – 0x{:X}", track.start_byte, track.end_byte));
                ui.label(format!("Peak amplitude: {:.3}", track.peak_amplitude));

                ui.add_space(8.0);
                if ui.button("Play track preview").clicked() {
                    preview_label = Some(track.label.clone());
                }
            }

            if let Some(label) = preview_label {
                self.log(format!("[AUDIO] Preview requested for {}", label));
                self.status = format!("Preview requested for {}", label);
            }
        });

        if let Some(i) = clicked_index {
            self.selected_track = i;
        }
    }

    fn draw_console_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("CONSOLE").strong());
        ui.add_space(6.0);
        ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
            for line in &self.log_lines {
                ui.monospace(line);
            }
        });
    }

    fn draw_analysis_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("ANALYSIS").strong());
        ui.add_space(6.0);
        ScrollArea::vertical().show(ui, |ui| {
            ui.monospace(&self.ai_summary);
        });
    }

    fn draw_library_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("PLUGIN LIBRARY").strong());
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            if ui.button("Add plugin").clicked() {
                self.add_plugin_dialog();
            }
            if ui.button("Reload plugins").clicked() {
                self.reload_plugins();
                self.status = "Plugins reloaded".to_string();
            }
        });
        ui.add_space(6.0);

        if self.plugins.is_empty() {
            ui.label("No plugins found. Drop compiled .dll/.so/.dylib files into plugins/.");
            ui.label("Use Add plugin to select a file from disk.");
            return;
        }

        let mut run_index: Option<usize> = None;

        ScrollArea::vertical().show(ui, |ui| {
            for (i, plugin) in self.plugins.iter().enumerate() {
                Frame::default()
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(150, 150, 150)))
                    .inner_margin(egui::Margin::same(6.0_f32))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&plugin.name).strong());
                                ui.label(format!("type: {} | status: {}", plugin.kind, plugin.status));
                                ui.label(&plugin.description);
                                ui.label(RichText::new(&plugin.path).size(11.0).weak());
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if plugin.status == "READY" && ui.button("RUN").clicked() {
                                    run_index = Some(i);
                                }
                            });
                        });
                    });
                ui.add_space(4.0);
            }
        });

        if let Some(i) = run_index {
            self.run_selected_plugin(i);
        }

        if !self.plugin_run_output.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Last plugin output:").strong());
            ui.monospace(&self.plugin_run_output);
        }
    }
}

fn ribbon_tab(ui: &mut egui::Ui, current: &mut Tab, value: Tab, label: &str) {
    let active = *current == value;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(84.0, 30.0), Sense::click());

    if ui.is_rect_visible(rect) {
        if active {
            ui.painter().rect_filled(
                rect,
                Rounding { nw: 3.0, ne: 3.0, sw: 0.0, se: 0.0 },
                Color32::from_rgb(44, 44, 48),
            );
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(12.5),
                Color32::from_rgb(255, 196, 84),
            );
        } else {
            let color = if response.hovered() {
                Color32::from_gray(235)
            } else {
                Color32::from_gray(175)
            };
            ui.painter()
                .text(rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(12.0), color);
        }
    }

    if response.clicked() {
        *current = value;
    }
}


fn ribbon_button(ui: &mut egui::Ui, icon: IconKind, label: &str) -> egui::Response {
    let size = Vec2::new(58.0, 56.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    if ui.is_rect_visible(rect) {
        let bg = if response.is_pointer_button_down_on() {
            Color32::from_rgb(64, 64, 70)
        } else if response.hovered() {
            Color32::from_rgb(52, 52, 57)
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, Rounding::same(3.0), bg);
        if response.hovered() {
            ui.painter()
                .rect_stroke(rect, Rounding::same(3.0), Stroke::new(1.0_f32, Color32::from_rgb(95, 95, 100)));
        }

        let icon_rect = Rect::from_center_size(rect.center_top() + Vec2::new(0.0, 18.0), Vec2::splat(22.0));
        draw_icon(ui.painter(), icon_rect, icon, Color32::from_gray(225));

        ui.painter().text(
            rect.center_bottom() - Vec2::new(0.0, 6.0),
            Align2::CENTER_BOTTOM,
            label,
            FontId::proportional(10.5),
            Color32::from_gray(215),
        );
    }

    response
}


fn ribbon_group(ui: &mut egui::Ui, caption: &str, min_width: f32, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(Vec2::new(min_width, 84.0), Layout::top_down(Align::Center), |ui| {
        ui.horizontal(|ui| {
            add_contents(ui);
        });
        ui.add_space(2.0);
        ui.label(RichText::new(caption).size(10.5).color(Color32::from_gray(160)));
    });
    ui.add_space(6.0);
    ui.separator();
    ui.add_space(6.0);
}

#[derive(Clone, Copy)]
enum IconKind {
    Open,
    Save,
    Library,
    Reload,
    Ai,
    Apply,
    Edit,
}

fn draw_icon(painter: &egui::Painter, rect: Rect, icon: IconKind, color: Color32) {
    let stroke = Stroke::new(1.6_f32, color);
    let zero = Rounding::same(0.0);

    match icon {
        IconKind::Open => {
            let back = Rect::from_min_max(rect.min + Vec2::new(1.0, 5.0), rect.max - Vec2::new(1.0, 1.0));
            painter.rect_stroke(back, Rounding::same(1.5_f32), stroke);
            let tab = Rect::from_min_size(rect.min + Vec2::new(1.0, 2.0), Vec2::new(rect.width() * 0.5, 3.5));
            painter.rect_filled(tab, Rounding::same(1.0_f32), color);
        }
        IconKind::Save => {
            painter.rect_stroke(rect, Rounding::same(1.5_f32), stroke);
            let notch = Rect::from_min_size(rect.right_top() + Vec2::new(-7.0, 1.0), Vec2::new(6.0, 6.0));
            painter.rect_filled(notch, zero, color);
            let slot = Rect::from_center_size(rect.center() + Vec2::new(0.0, 5.0), Vec2::new(rect.width() * 0.5, 5.0));
            painter.rect_stroke(slot, zero, stroke);
        }
        IconKind::Library => {
            let bar_w = rect.width() / 3.5;
            for i in 0..3 {
                let x = rect.left() + i as f32 * (rect.width() / 3.0) + 1.0;
                let h = rect.height() * (0.45 + 0.22 * i as f32);
                let bar = Rect::from_min_max(
                    egui::pos2(x, rect.bottom() - h),
                    egui::pos2(x + bar_w, rect.bottom()),
                );
                painter.rect_filled(bar, Rounding::same(1.0_f32), color);
            }
        }
        IconKind::Reload => {
            painter.circle_stroke(rect.center(), rect.width() * 0.38_f32, stroke);
            let tip = rect.center() + Vec2::new(rect.width() * 0.38, -3.0);
            painter.add(Shape::convex_polygon(
                vec![tip, tip + Vec2::new(-6.0, -1.0), tip + Vec2::new(-1.0, 5.0)],
                color,
                Stroke::new(0.0_f32, Color32::TRANSPARENT),
            ));
        }
        IconKind::Ai => {
            painter.circle_stroke(rect.center(), rect.width() * 0.3, stroke);
            for i in 0..4 {
                let angle = std::f32::consts::FRAC_PI_2 * i as f32 + 0.5;
                let p = rect.center() + Vec2::angled(angle) * (rect.width() * 0.48);
                painter.line_segment([rect.center(), p], stroke);
                painter.circle_filled(p, 1.8, color);
            }
        }
        IconKind::Apply => {
            let p1 = rect.left_center() + Vec2::new(1.0, 2.0);
            let p2 = rect.center_bottom() - Vec2::new(2.0, 1.0);
            let p3 = rect.right_top() + Vec2::new(-1.0, 2.0);
            painter.line_segment([p1, p2], Stroke::new(2.0_f32, color));
            painter.line_segment([p2, p3], Stroke::new(2.0_f32, color));
        }
        IconKind::Edit => {
            painter.line_segment([rect.left_bottom(), rect.right_top()], Stroke::new(2.6_f32, color));
            painter.circle_filled(rect.right_top(), 1.8, color);
            painter.circle_filled(rect.left_bottom(), 1.4, color);
        }
    }
}

fn apply_dark_ribbon_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = Color32::from_rgb(30, 30, 32);
    visuals.panel_fill = Color32::from_rgb(30, 30, 32);
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(37, 37, 40);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(45, 45, 49);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(62, 62, 68);
    visuals.widgets.active.bg_fill = Color32::from_rgb(0, 110, 200);
    visuals.selection.bg_fill = Color32::from_rgb(0, 120, 215);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, Color32::from_gray(210));
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, Color32::from_gray(215));
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, Color32::from_gray(245));
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(60, 60, 64));
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(90, 90, 96));
    ctx.set_visuals(visuals);
}

fn main() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1000.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        APP_TITLE,
        options,
        Box::new(|_cc| Box::new(ByteForge2000App::default())),
    )
    .expect("run_native failed");
}