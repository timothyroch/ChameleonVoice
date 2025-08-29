use log::info;    
use actix_web::{get, post, web, App, HttpResponse, HttpServer, Responder};
use actix_web::http::header;
use tokio::time::{sleep, Duration};
use std::convert::Infallible;
use actix_web::web::Bytes;
use actix_cors::Cors;
use futures_util::{stream, StreamExt};
use tokio_stream::wrappers::IntervalStream;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
use reqwest::Client;
use serde_json::{Value as Json, json};
use actix_web::HttpRequest;
use std::{fs, path::PathBuf, time::{SystemTime, UNIX_EPOCH}, sync::Arc};
use std::collections::HashMap;


mod config;
mod state;
mod multi_threads;

use crate::config::Config;
use crate::state::AppState;

/// Simple health check route
#[get("/health")]
async fn health() -> impl Responder {
    "OK"
}

/// dBFS for i16 PCM slice; returns (is_voice, db)
fn vad_rms_db(pcm: &[i16]) -> (bool, f32) {
    if pcm.is_empty() { return (false, -90.0); }
    let sum_sq: f64 = pcm.iter().map(|&s| (s as f64) * (s as f64)).sum();
    let rms = (sum_sq / pcm.len() as f64).sqrt();
    let db = if rms <= 1.0 { -90.0 } else { 20.0 * (rms / 32768.0).log10() as f32 };
    let is_voice = db > -60.0; // simple threshold
    (is_voice, db)
}

fn s16le_to_f32(pcm: &[i16]) -> Vec<f32> {
    pcm.iter().map(|&s| (s as f32) / 32768.0).collect()
}

fn device_arg(format: &str, device: &str) -> String {
    if format == "dshow" && !device.starts_with("audio=") {
        format!("audio={}", device)
    } else if format == "avfoundation" && !device.starts_with(':') {
        format!(":{}", device)
    } else {
        device.to_string()
    }
}

async fn translate_text(http: &Client, base: &str, text: &str, to: &str) -> String {
    if text.trim().is_empty() { return String::new(); }
    let url = format!("{}/translate", base.trim_end_matches('/'));
    let body = json!({ "q": text, "source":"auto", "target": to });

    match http.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<Json>().await {
                Ok(j) => j.get("translatedText").and_then(|v| v.as_str()).unwrap_or(text).to_string(),
                Err(_) => text.to_string(),
            }
        }
        _ => text.to_string(), // fail-open so SSE keeps flowing
    }
}

fn normalize_and_chunk(text: &str, max_len: usize) -> Vec<String> {
    use unicode_normalization::UnicodeNormalization;
    let clean = text
        .nfc()
        .collect::<String>()
        .chars()
        .filter(|&c| !c.is_control() || c == '\n')
        .collect::<String>()
        .trim()
        .to_string();

    let mut out = Vec::new();
    for piece in clean.split(|c: char| "。．.?!？！\n".contains(c)) {
        let p = piece.trim();
        if p.is_empty() { continue; }
        if p.len() <= max_len {
            out.push(p.to_string());
        } else {
            // safe fixed-size slices on char boundaries
            let mut start_idx = 0;
            while start_idx < p.len() {
                let mut end = (start_idx + max_len).min(p.len());
                while end < p.len() && !p.is_char_boundary(end) { end += 1; }
                out.push(p[start_idx..end].to_string());
                start_idx = end;
            }
        }
    }
    if out.is_empty() && !clean.is_empty() { out.push(clean); }
    out
}


#[post("/start")]
async fn start(
    app: web::Data<AppState>,
     cfg: web::Data<Config>,
     q: web::Query<HashMap<String, String>>,
    ) -> impl Responder {
    let mut worker = app.worker.lock().await;
    if worker.is_some() { return HttpResponse::Ok().body("already running"); }

    // reset stop flag
    let _ = app.stop_tx.send(false);
    let tx = app.tx.clone();
    let speaker_buf_handle = app.speaker_buf.clone();

    // decide mode
    let mode = q.get("mode").map(|s| s.as_str()).unwrap_or("");
    let enable_multi= match mode {
        "multi" => true,
        "single" => false,
        _ => std::env::var("ENABLE_MULTI").ok().as_deref()== Some("1"), // fallback to env
    };
    let lang_force: Option<String> = q.get("lang")
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty() && s.to_ascii_lowercase() != "auto");

    // capture the config values we need into the task
    let cfg_ffmpeg = (
    tx.clone(),
    app.stop_tx.subscribe(),
    cfg.ffmpeg.clone(),
    cfg.ffmpeg_format.clone(),
    cfg.ffmpeg_device.clone(),
    cfg.sample_rate,
    cfg.chunk_ms,
    cfg.whisper_model.clone(),
    cfg.whisper_threads,
    speaker_buf_handle,
    enable_multi,
    lang_force.clone(),
    );
    
    // Mock producer
    let handle = tokio::spawn(async move {
        let (tx, mut stop_rx, ffmpeg, format, device, sr, chunk_ms, whisper_model, whisper_threads, speaker_buf_handle, enable_multi, lang_force) = cfg_ffmpeg;

        // ---- ffmpeg command: microphone -> mono s16le @ sr Hz -> stdout ----
        let dev = device_arg(&format, &device);
        let mut cmd = Command::new(ffmpeg);
        cmd.args([
            "-hide_banner","-loglevel","error","-nostdin",
            "-fflags","nobuffer","-flags","low_delay","-analyzeduration","0","-probesize","32",
            "-f",&format,"-rtbufsize","100M","-thread_queue_size","1024",
            "-i",&dev,
            "-ac","1","-ar",&sr.to_string(),
            "-f","s16le","pipe:1",
        ]);

        let mut child = match cmd.stdout(std::process::Stdio::piped()).spawn() {
            Ok(c) => c,
            Err(e) => { let _ = tx.send(format!(r#"{{"error":"spawn ffmpeg","msg":"{}"}}"#, e)); return; }
        };

        let mut stdout = match child.stdout.take() {
            Some(s) => s,
            None => { let _ = tx.send(r#"{"error":"no ffmpeg stdout"}"#.to_string()); return; }
        };

        let bytes_per_sample = 2usize; // i16
        let samples_per_chunk = (sr as u64 * chunk_ms as u64 / 1000) as usize;
        let speaker_window_sec: usize = 6; 
        let sr_usize = sr as usize;
        let speaker_buf_cap = sr_usize * speaker_window_sec;
        let bytes_per_chunk = samples_per_chunk * bytes_per_sample;

        // ---- whisper init (load once per /start) ----
        let ctx = match WhisperContext::new_with_params(
        &whisper_model,
        WhisperContextParameters::default()
         ) {
            Ok(c) => Arc::new(c),
            Err(e) => { let _ = tx.send(format!(r#"{{"error":"load whisper","msg":"{}"}}"#, e)); return; }
        };

         #[allow(unused_mut)]
        let mut maybe_job_tx: Option<tokio::sync::mpsc::Sender<multi_threads::AsrJob>> = None;
        #[allow(unused_mut)]
        let mut wstate_single = None;

        if enable_multi {
            // choose worker layout
            let total = num_cpus::get_physical().max(1);
            let workers = (total / 2).max(2);
            let threads_per_state = (total / workers).max(1) as i32;
            let jt = multi_threads::spawn_parallel_asr(ctx.clone(), threads_per_state, workers, tx.clone(), lang_force.clone(),);
            maybe_job_tx = Some(jt);
            let _ = tx.send(format!(r#"{{"info":"mode","value":"multi","workers":{},"threads_per_state":{}}}"#, workers, threads_per_state));
        } else {
            // original single-state decoder
            match ctx.create_state() {
                Ok(s) => { wstate_single = Some(s); let _ = tx.send(r#"{"info":"mode","value":"single"}"#.to_string()); }
                Err(e) => { let _ = tx.send(format!(r#"{{"error":"whisper state","msg":"{}"}}"#, e)); return; }
            }
        }

        let mut buf = vec![0u8; bytes_per_chunk];
        let mut seq_counter: u64 = 0;
        let mut t: f32 = 0.0;
        loop {
            if *stop_rx.borrow() { break; }

             // read one chunk or report error/EOF
            match stdout.read_exact(&mut buf).await {
            Ok(_) => {
                // convert to i16
                let mut pcm = Vec::with_capacity(samples_per_chunk);
                for i in (0..buf.len()).step_by(2) {
                    pcm.push(i16::from_le_bytes([buf[i], buf[i + 1]]));
                }

                {
                // Append to shared ring buffer and truncate to cap
                let mut sb = speaker_buf_handle.lock().await;
                for &s in &pcm {
                    sb.push_back(s);
                }
                while sb.len() > speaker_buf_cap {
                    sb.pop_front();
                }
            }

                // simple VAD + dBFS
                let (is_voice, db) = vad_rms_db(&pcm);

                // --- run whisper on this chunk if voice is active ---
                if is_voice {
                    let t1 = t + (samples_per_chunk as f32 / sr as f32);
                    let pcm_f32 = s16le_to_f32(&pcm);

                    if let Some(job_tx) = &maybe_job_tx {
                        use multi_threads::AsrJob;
                        if job_tx.try_send(AsrJob { seq: seq_counter, t0: t, t1, pcm_f32 }).is_err() {
                            let _ = tx.send(r#"{"warn":"asr_queue_full"}"#.to_string());

                        }
                        seq_counter = seq_counter.wrapping_add(1);
                    } else if let Some(wstate) = wstate_single.as_mut() {
                         
                    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
                    params.set_n_threads(whisper_threads as i32);
                    params.set_translate(false);
                    if let Some(ref l) = lang_force {
                        params.set_language(Some(l));
                    } else {
                        params.set_language(Some("auto"));
                    }
                    params.set_no_timestamps(true);
                    params.set_print_special(false);
                    params.set_print_progress(false);
                    params.set_print_realtime(false);
                    params.set_no_context(true);
                    params.set_single_segment(true);
                    params.set_temperature(0.0);
                    params.set_max_len(120);

                    if let Err(e) = wstate.full(params, &pcm_f32) {
                        let _ = tx.send(format!(r#"{{"error":"whisper full","msg":"{}"}}"#, e));
                    } else {
                        let mut text_out = String::new();
                        let n = wstate.full_n_segments().unwrap_or(0);
                        for i in 0..n {
                            if let Ok(seg) = wstate.full_get_segment_text(i) {
                                text_out.push_str(&seg);
                            }
                        }
                        let text_out = text_out.trim().to_string();
                        if !text_out.is_empty() {
                            let _ = tx.send(format!(
                                r#"{{"t0":{:.2},"t1":{:.2},"text":"{}","lang":"und"}}"#,
                                t, t1, text_out.replace('"', "\\\"")
                            ));
                        }
                    }
                  }
                }


            // emit one frame
            let _ = tx.send(format!(
                r#"{{"t0":{:.2},"db":{:.1},"voice":{},"sr":{},"chunk_ms":{}}}"#,
                t, db, if is_voice { "true" } else { "false" }, sr, chunk_ms
            ));
            t += (samples_per_chunk as f32) / (sr as f32);

            // wait for either: sleep or stop signal
            tokio::select! {
                _ = sleep(Duration::from_millis(0)) => {},
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() { break; }
                }
            }
        } 
         Err(e) => {
        let _ = tx.send(format!(r#"{{"error":"ffmpeg read","msg":"{}"}}"#, e));
        break;
    }
}
        }
    // best-effort terminate ffmpeg
    let _ = child.kill().await;
    });

    *worker = Some(handle);
    HttpResponse::Ok().body("started")
}

#[post("/stop")]
async fn stop(app: web::Data<AppState>) -> impl Responder {
    let _ = app.stop_tx.send(true);
    if let Some(handle) = app.worker.lock().await.take() {
        handle.abort(); 
    }
    HttpResponse::Ok().body("stopped")
}

#[get("/speaker_snapshot")]
async fn speaker_snapshot(app: web::Data<AppState>, cfg: web::Data<Config>) -> impl Responder {
    use byteorder::{LittleEndian, WriteBytesExt};

    let sr = cfg.sample_rate as u32;

    // pull a copy of current buffer
    let pcm_i16: Vec<i16> = {
        let guard = app.speaker_buf.lock().await; 
        guard.iter().copied().collect()
    };

    if pcm_i16.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "no_speaker_audio",
            "message": "No speaker audio captured yet. Please start recording and speak first."
        }));
    }

    // build a PCM16 mono WAV in-memory
    let mut data_bytes = Vec::with_capacity(pcm_i16.len() * 2);
    for s in pcm_i16 {
        data_bytes.write_i16::<LittleEndian>(s).unwrap();
    }

    // WAV header (44 bytes)
    let mut wav = Vec::with_capacity(44 + data_bytes.len());
    let byte_rate = sr * 1 * 16 / 8; // sr * channels * bits/8
    let block_align = 1 * 16 / 8;

    // RIFF header
    wav.extend_from_slice(b"RIFF");
    wav.write_u32::<LittleEndian>((36 + data_bytes.len()) as u32).unwrap();
    wav.extend_from_slice(b"WAVE");

    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.write_u32::<LittleEndian>(16).unwrap();             // PCM fmt chunk size
    wav.write_u16::<LittleEndian>(1).unwrap();              // PCM
    wav.write_u16::<LittleEndian>(1).unwrap();              // mono
    wav.write_u32::<LittleEndian>(sr).unwrap();             // sample rate
    wav.write_u32::<LittleEndian>(byte_rate).unwrap();      // byte rate
    wav.write_u16::<LittleEndian>(block_align).unwrap();    // block align
    wav.write_u16::<LittleEndian>(16).unwrap();             // bits per sample

    // data chunk
    wav.extend_from_slice(b"data");
    wav.write_u32::<LittleEndian>(data_bytes.len() as u32).unwrap();
    wav.extend_from_slice(&data_bytes);

    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "audio/wav"))
        .body(wav)
}


#[get("/stream/raw")]
async fn stream_raw(app: web::Data<AppState>) -> impl Responder {
    let rx = app.tx.subscribe();

    // Build an SSE response
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(line) => {
                let frame = format!("data: {}\n\n", line);
                Some((Ok::<Bytes, Infallible>(Bytes::from(frame)), rx))
            }
            Err(_) => None,
        }
    });

    // comment keepalive frames every 15s
    let ka = IntervalStream::new(tokio::time::interval(Duration::from_secs(15)))
        .map(|_| Ok::<Bytes, Infallible>(Bytes::from_static(b": keepalive\n\n")));

    // merge both streams
    let merged = stream::select(stream, ka);


    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .insert_header((header::CONNECTION, "keep-alive"))
        .streaming(merged)
}

#[get("/stream/translated")]
async fn stream_translated(app: web::Data<AppState>, cfg: web::Data<Config>, to: web::Query<std::collections::HashMap<String,String>>) -> impl Responder {
    let rx = app.tx.subscribe();
    let target = to.get("to").map(|s| s.as_str()).unwrap_or("en").to_string();

    let http = Client::new();
    let base = cfg.translate_base.clone();
    let coqui_voice = cfg.coqui_voice.clone();
    let server_port = cfg.port;

    // Transform incoming JSON lines: translate "text" when present
    let stream = futures_util::stream::unfold((rx, http, base, target, coqui_voice, server_port), |(mut rx, http, base, target, coqui_voice, server_port)| async move {
        match rx.recv().await {
            Ok(line) => {
                // parse JSON object; if it has "text", translate it
                let mut out_line = line.clone();
                if let Ok(mut node) = serde_json::from_str::<Json>(&line) {
                    if node.is_object() {
                        if let Some(text) = node.get("text").and_then(|v| v.as_str()) {
                            if !text.trim().is_empty() {
                                let tr = translate_text(&http, &base, text, &target).await;
                                if let Some(obj) = node.as_object_mut() {

                                        // generate audio_url for Coqui
                                        let enc_text = urlencoding::encode(&tr);
                                        obj.insert("text".into(), Json::String(tr.clone()));
                                        // Build snapshot URL on this same server
                                        let self_base = format!("http://127.0.0.1:{}", server_port);
                                        let snapshot = format!("{}/speaker_snapshot", self_base);
                                        let audio_url = format!(
                                        "/tts_proxy?lang={}&speaker_wav={}&text={}",
                                        target,
                                        urlencoding::encode(&snapshot),
                                        enc_text
                                    );
                                        obj.insert("audio_url".into(), Json::String(audio_url));
                                }
                                out_line = node.to_string();
                            }
                        } else {
                            // pass-through any non-text JSON from worker (e.g., VU frames)
                            out_line = node.to_string();
                        }
                    }
                }
                let frame = format!("data: {}\n\n", out_line);
                Some((Ok::<Bytes, Infallible>(Bytes::from(frame)), (rx, http, base, target, coqui_voice, server_port)))
            }
            Err(_) => None,
        }
    });

    // keepalive comments every 15s
    let ka = IntervalStream::new(tokio::time::interval(Duration::from_secs(15)))
        .map(|_| Ok::<Bytes, Infallible>(Bytes::from_static(b": keepalive\n\n")));

    let merged = stream::select(stream, ka);

    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .insert_header((header::CONNECTION, "keep-alive"))
        .streaming(merged)
}

#[get("/tts_proxy")]
async fn tts_proxy(req: HttpRequest, cfg: web::Data<Config>) -> impl Responder {
    use actix_web::HttpResponse;
    use reqwest::Client;

    let params: std::collections::HashMap<_, _> =
        url::form_urlencoded::parse(req.query_string().as_bytes()).into_owned().collect();

    let text  = params.get("text").map(|s| s.as_str()).unwrap_or("");
    let lang0 = params.get("lang").map(|s| s.as_str()).unwrap_or("en").to_string();

    // voice is optional; treat empty/"default"/"none" as not provided
    let raw_voice = params.get("voice").map(|s| s.as_str()).unwrap_or(&cfg.coqui_voice);
    let voice_opt = match raw_voice.trim().to_ascii_lowercase().as_str() {
        "" | "default" | "none" => None,
        _ => Some(raw_voice.trim()),
    };

    // allow ?speaker_wav=... override, else use env-configured default
    let speaker_wav_cfg = cfg.coqui_speaker_wav.clone();
    let speaker_wav_qs  = params.get("speaker_wav").map(|s| s.as_str());
    let speaker_wav_opt = {
        let chosen = speaker_wav_qs.unwrap_or(speaker_wav_cfg.as_str());
        let trimmed = chosen.trim();
        if trimmed.is_empty() { None } else { Some(trimmed) }
    };

    // --- resolve speaker_wav URL -> local temp file, if needed ---
    let http = Client::new();
    let mut speaker_wav_local: Option<String> = speaker_wav_opt.map(|s| s.to_string());
    let mut tmp_to_cleanup: Option<PathBuf> = None;

    if let Some(ref s) = speaker_wav_local {
        if s.starts_with("http://") || s.starts_with("https://") {
            // fetch the snapshot bytes
            let resp = match http.get(s).send().await {
                Ok(r) => r,
                Err(e) => {
                    return HttpResponse::BadGateway()
                        .body(format!("snapshot fetch error: {}", e));
                }
            };

            // Check if the snapshot endpoint returned an error
            if !resp.status().is_success() {
                let status = resp.status();
                let error_text = resp.text().await.unwrap_or_default();

                // If it's a 400 from our speaker_snapshot endpoint, try without speaker_wav
                if status == 400 {
                    log::warn!(
                        "Speaker snapshot not ready, falling back to TTS without speaker_wav: {}",
                        error_text
                    );
                    speaker_wav_local = None;
                } else {
                    return HttpResponse::BadGateway()
                        .body(format!("snapshot fetch failed with status {}: {}", status, error_text));
                }
            } else {
                let bytes = match resp.bytes().await {
                    Ok(b) => b,
                    Err(e) => {
                        return HttpResponse::BadGateway()
                            .body(format!("snapshot read error: {}", e));
                    }
                };

                // write to a temp .wav
                let mut p = std::env::temp_dir();
                let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
                p.push(format!("coqui_ref_{}.wav", ts));
                if let Err(e) = fs::write(&p, &bytes) {
                    return HttpResponse::InternalServerError()
                        .body(format!("temp write error: {}", e));
                }
                speaker_wav_local = Some(p.to_string_lossy().to_string());
                tmp_to_cleanup = Some(p);
            }
        }
    }

    if text.trim().is_empty() {
        return HttpResponse::BadRequest().body("missing text");
    }

    let base = cfg.coqui_base.trim_end_matches('/').to_string();
    let url  = format!("{}/tts", base);

    let chunks = normalize_and_chunk(text, 220);

    // language candidates + autodetect fallback
    let mut lang_candidates: Vec<String> = match lang0.to_lowercase().as_str() {
        "ko" | "kr" | "ko-kr"            => vec!["ko".into(), "ko-KR".into(), "Korean".into()],
        "ja" | "jp" | "ja-jp"            => vec!["ja".into(), "ja-JP".into(), "Japanese".into() ],
        "zh" | "zh-cn" | "zh-hans"       => vec!["zh".into(), "zh-CN".into(), "cmn-Hans-CN".into(), "Chinese".into()],
        "zh-tw" | "zh-hant"              => vec!["zh-TW".into(), "cmn-Hant-TW".into(), "Chinese".into()],
        "fr" | "fr-fr"                   => vec!["fr".into(), "fr-FR".into(), "French".into()],
        _                                => vec![lang0.clone()],
    };
    lang_candidates.push("__AUTO__".into());

    async fn try_once(
        http: &Client,
        url: &str,
        lang_opt: Option<&str>,
        _voice_opt: Option<&str>,
        speaker_wav_opt: Option<&str>,
        chunks: &[String],
    ) -> Result<Vec<u8>, (u16, String)> {
        let mut wav_concat: Vec<u8> = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            let mut body = serde_json::json!({
                "text": chunk,
                "audio_format": "wav"
            });
            if let Some(l) = lang_opt {
                body["language"] = serde_json::Value::String(l.to_string());
            }
            if let Some(wav) = speaker_wav_opt {
                body["speaker_wav"] = serde_json::Value::String(wav.to_string());
            }

            let resp = http.post(url).json(&body).send().await
                .map_err(|e| (502u16, format!("connect error: {}", e)))?;
            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let msg = resp.text().await.unwrap_or_default();
                return Err((status, msg));
            }
            let mut bytes = resp.bytes().await
                .map_err(|e| (502u16, format!("bytes error: {}", e)))?
                .to_vec();

            if i == 0 {
                wav_concat.append(&mut bytes);
            } else {
                // naive WAV concat: strip 44-byte header from subsequent chunks
                if bytes.len() > 44 { wav_concat.extend_from_slice(&bytes[44..]); }
                else { wav_concat.extend_from_slice(&bytes); }
            }
        }
        Ok(wav_concat)
    }

    let mut last_err: Option<(u16, String)> = None;
    for cand_lang in &lang_candidates {
        let lang_opt = if cand_lang == "__AUTO__" { None } else { Some(cand_lang.as_str()) };

        // try with voice (if provided), then without voice
        for v in [voice_opt, None] {
            match try_once(&http, &url, lang_opt, v, speaker_wav_local.as_deref(), &chunks).await {
                Ok(wav) => {
                    if let Some(p) = tmp_to_cleanup.take() { let _ = fs::remove_file(p); }
                    return HttpResponse::Ok()
                        .insert_header((header::CONTENT_TYPE, "audio/wav"))
                        .body(wav);
                }
                Err(e) => { last_err = Some(e); }
            }
        }
        if let Some(p) = tmp_to_cleanup.take() { let _ = fs::remove_file(p); }
    }

    let (code, msg) = last_err.unwrap_or((502u16, "unknown coqui error".into()));
    HttpResponse::BadGateway().body(format!("coqui error {}: {}", code, msg))
}


#[get("/demo")]
async fn demo_page() -> impl Responder {
    let html = r#"<!doctype html>
<html>
<head>
<meta charset="utf-8" />
<title>Live Translator + Coqui</title>
<style>
body { font-family: system-ui, Arial; margin: 2rem; }
.log { max-height: 40vh; overflow:auto; border:1px solid #ccc; padding:0.5rem; }
.entry { margin:.25rem 0; }
</style>
</head>
<body>
<h1>Live Translator + Coqui</h1>
<p>
  <button id="start">start</button>
  <button id="stop">stop</button>
  <div id="beat" style="
  width: 20px; height: 20px;
  border-radius: 50%;
  background: red;
  margin: 1rem 0;
  opacity: 0.3;
  transition: opacity 0.1s, transform 0.1s;
"></div>
  <label>Translate to: <input id="lang" value="en" size="4"></label>
    <label style="margin-left:1rem;">ASR mode:
    <select id="mode">
      <option value="single">single (inline)</option>
      <option value="multi" selected>multi (thread pool)</option>
    </select>
  </label>
  <label style="margin-left:1rem;">ASR language:
  <select id="asr_lang">
    <option value="auto">auto</option>
    <option value="en" selected>English (en)</option>
    <option value="ja">Japanese (ja)</option>
    <option value="fr">French (fr)</option>
    <option value="es">Spanish (es)</option>
    <option value="de">German (de)</option>
    <option value="ko">Korean (ko)</option>
    <option value="zh">Chinese (zh)</option>
  </select>
</label>

</p>
<div class="log" id="log"></div>
<script>
const log = (m) => {
  const d = document.createElement('div');
  d.className = 'entry';
  d.textContent = m;
  document.getElementById('log').appendChild(d);
  document.getElementById('log').scrollTop = 1e9;
};

async function callStart() {
    const mode = document.getElementById('mode').value || 'single';
    const asrLang = (document.getElementById('asr_lang')?.value || 'auto');
    const qs = '/start?mode=' + encodeURIComponent(mode) +
             '&lang=' + encodeURIComponent(asrLang);
    const r = await fetch(qs, {method:'POST'});
    log('/start -> ' + (await r.text()));
}

async function callStop() {
    const r = await fetch('/stop', {method:'POST'});
    log('/stop -> ' + (await r.text()));
}

document.getElementById('start').onclick = async () => {
    await callStart();
    startBeat();
};
document.getElementById('stop').onclick = async () => { 
    await callStop();
    stopBeat();
};

let es;
function connect() {
  const to = document.getElementById('lang').value || 'en';
  if (es) es.close();
  es = new EventSource('/stream/translated?to=' + encodeURIComponent(to));
  es.onmessage = (ev) => {
    try {
      const o = JSON.parse(ev.data);
      if (o.text) {
        log(`[${(o.t0??0).toFixed?.(2)}] ${o.text}`);
        if (o.audio_url) {
          const a = new Audio(o.audio_url);
          a.play().catch(()=>{});
        }
      }
    } catch(e) {
      log('bad JSON: ' + ev.data);
    }
  };
  es.onerror = () => log('SSE error (will keep trying)');
}
connect();
document.getElementById('lang').addEventListener('change', connect);
let beatTimer = null;
const CADENCE_MS = 1200;

function pulseBeat() {
  const b = document.getElementById('beat');
  if (!b) return;
  b.style.opacity = '1';
  b.style.transform = 'scale(1.5)';
  setTimeout(() => {
    b.style.opacity = '.3';
    b.style.transform = 'scale(1)';
  }, 150);
}

function startBeat() {
  stopBeat();          // ensure only one timer
  pulseBeat();         // immediate visual on click
  beatTimer = setInterval(pulseBeat, CADENCE_MS);
}

function stopBeat() {
  if (beatTimer) {
    clearInterval(beatTimer);
    beatTimer = null;
  }
  const b = document.getElementById('beat');
  if (b) {
    b.style.opacity = '.3';
    b.style.transform = 'scale(1)';
  }
}
</script>
</body>
</html>"#;
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/html; charset=utf-8"))
        .body(html)
}


#[get("/debug/sse")]
async fn debug_sse() -> impl Responder {
    let ticks = IntervalStream::new(tokio::time::interval(Duration::from_millis(500)))
        .enumerate()
        .map(|(i, _)| {
            let frame = format!("data: {{\"tick\":{}}}\n\n", i);
            Ok::<Bytes, Infallible>(Bytes::from(frame))
        });

    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .insert_header((header::CONNECTION, "keep-alive"))
        .streaming(ticks)
}


#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Init logger
    env_logger::init();
    info!("Starting Live Translator...");

    let cfg = Config::from_env();

    info!("HTTP server listening on 127.0.0.1:{}", cfg.port);

    let shared = web::Data::new(AppState::new());

    let cfg_data = web::Data::new(cfg.clone());

    HttpServer::new(move || {
        App::new()
            .app_data(cfg_data.clone()) // share Config with handlers
            .app_data(shared.clone()) // provide AppState to handlers
            .wrap(Cors::permissive()) // allow any origin for quick testing
            .service(health) 
            .service(start)            
            .service(stop)             
            .service(stream_raw)
            .service(debug_sse)
            .service(stream_translated)
            .service(tts_proxy)
            .service(speaker_snapshot)
            .service(demo_page)
    })
    .bind(("127.0.0.1", cfg.port))?
    .run()
    .await
}
