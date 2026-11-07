use std::{
    collections::HashMap,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use teloxide::{
    prelude::*,
    types::{InputFile, KeyboardButton, KeyboardMarkup},
};
use tokio::{fs, sync::Mutex};

#[derive(Clone, Debug)]
struct Track {
    id: String,
    title: String,
    artist: String,
    duration: Option<u64>,
}

type Searches = Arc<Mutex<HashMap<ChatId, Vec<Track>>>>;
type AudioCache = Arc<Mutex<HashMap<String, String>>>;
static FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn ytdlp() -> String {
    if cfg!(windows) {
        "bin/yt-dlp.exe".to_owned()
    } else {
        "yt-dlp".to_owned()
    }
}

fn search_tracks(query: String) -> Result<Vec<Track>, String> {
    let output = Command::new(ytdlp())
        .args([
            "--dump-single-json",
            "--flat-playlist",
            "--no-warnings",
            "--skip-download",
            &format!("ytsearch5:{query}"),
        ])
        .output()
        .map_err(|e| format!("Не вдалося запустити yt-dlp: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).chars().take(300).collect());
    }

    let data: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Не вдалося прочитати результати пошуку: {e}"))?;
    let entries = data["entries"].as_array().ok_or("Пошук не повернув список треків")?;
    let tracks: Vec<_> = entries
        .iter()
        .filter_map(|entry| {
            let id = entry["id"].as_str()?.to_owned();
            let title = entry["title"].as_str()?.to_owned();
            let artist = entry["uploader"].as_str()
                .or_else(|| entry["channel"].as_str())
                .unwrap_or("Невідомий виконавець")
                .to_owned();
            Some(Track {
                id,
                title,
                artist,
                duration: entry["duration"].as_u64(),
            })
        })
        .take(5)
        .collect();
    Ok(tracks)
}

fn display_duration(duration: Option<u64>) -> String {
    match duration {
        Some(seconds) => format!("{}:{:02}", seconds / 60, seconds % 60),
        None => "тривалість невідома".to_owned(),
    }
}

fn escape_html(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn track_button(index: usize, track: &Track) -> String {
    let label = format!("{} — {}", track.title, track.artist);
    let label: String = label.chars().take(55).collect();
    format!("{}️⃣ {label}", index + 1)
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    pretty_env_logger::init();
    log::info!("Музичний бот запущено");

    let bot = Bot::from_env();
    let searches: Searches = Arc::new(Mutex::new(HashMap::new()));
    let audio_cache: AudioCache = Arc::new(Mutex::new(HashMap::new()));
    let ytdlp_path = ytdlp();

    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let searches = Arc::clone(&searches);
        let audio_cache = Arc::clone(&audio_cache);
        let ytdlp_path = ytdlp_path.clone();
        async move {
            let Some(text) = msg.text().map(str::trim) else { return Ok(()); };
            let chat_id = msg.chat.id;

            if text == "/start" || text == "/help" {
                let keyboard = KeyboardMarkup::new(vec![vec![KeyboardButton::new("🔍 Знайти пісню")]])
                    .resize_keyboard(true);
                bot.send_message(chat_id,
                    "🎵 <b>Привіт! Я допоможу знайти пісню.</b>\n\nНадішли назву пісні або виконавця — покажу 5 результатів. Обери потрібний, і я надішлю аудіо.\n\nМожна також скористатися кнопкою пошуку нижче.")
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .reply_markup(keyboard)
                    .await?;
                return Ok(());
            }

            if text == "🔍 Знайти пісню" || text == "🔎 Інший пошук" {
                bot.send_message(chat_id, "Напиши назву пісні або виконавця — знайду найкращі збіги 🎧")
                    .await?;
                return Ok(());
            }

            let selected = text.chars().next().and_then(|c| match c {
                '1' | '2' | '3' | '4' | '5' => c.to_digit(10).map(|n| n as usize - 1),
                _ => None,
                }).filter(|_| text.contains("️⃣"));

            if let Some(index) = selected {
                let track = searches.lock().await.get(&chat_id).and_then(|list| list.get(index)).cloned();
                let Some(track) = track else {
                    bot.send_message(chat_id, "Цей список уже застарів. Надішли запит ще раз, щоб знайти пісню 🎵").await?;
                    return Ok(());
                };

                let total_started = Instant::now();
                if let Some(file_id) = audio_cache.lock().await.get(&track.id).cloned() {
                    match bot.send_audio(chat_id, InputFile::file_id(file_id))
                        .caption("Готово! Гарного прослуховування 🎶")
                        .await
                    {
                        Ok(_) => {
                            log::info!("track={} reused from Telegram cache in {:.2}s", track.id, total_started.elapsed().as_secs_f64());
                            return Ok(());
                        }
                        Err(error) => {
                            log::warn!("Telegram cached audio send failed for {}: {}; downloading again", track.id, error);
                            audio_cache.lock().await.remove(&track.id);
                        }
                    }
                }

                let status = bot.send_message(chat_id, format!("⏳ Завантажую <b>{}</b> — {}…", escape_html(&track.title), escape_html(&track.artist)))
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .await?;

                let sequence = FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
                let prefix = format!("music_{}_{}_{}", chat_id.0, std::process::id(), sequence);
                let pattern = format!("{prefix}.%(ext)s");
                let video_url = format!("https://www.youtube.com/watch?v={}", track.id);
                let ytdlp_path = ytdlp_path.clone();
                let download_started = Instant::now();
                let download = tokio::task::spawn_blocking(move || {
                    Command::new(ytdlp_path)
                        .args([
                            "--no-warnings", "--no-playlist", "--no-part",
                            // Parallel fragment downloads speed up DASH/HLS audio streams.
                            "--concurrent-fragments", "4",
                            // Keep this audio-only: falling back to `best` can trigger a
                            // needless audio/video merge before we can send the track.
                            "-f", "bestaudio[ext=m4a]/bestaudio",
                            "-o", &pattern, &video_url,
                        ])
                        .output()
                }).await;

                let result: Result<PathBuf, String> = match download {
                    Ok(Ok(output)) if output.status.success() => {
                        let mut found = None;
                        if let Ok(mut entries) = fs::read_dir(".").await {
                            while let Ok(Some(entry)) = entries.next_entry().await {
                                let name = entry.file_name().to_string_lossy().into_owned();
                                if name.starts_with(&prefix) {
                                    found = Some(entry.path());
                                    break;
                                }
                            }
                        }
                        found.ok_or_else(|| "Завантажений файл не знайдено".to_owned())
                    }
                    Ok(Ok(output)) => Err(String::from_utf8_lossy(&output.stderr).chars().take(300).collect()),
                    Ok(Err(error)) => Err(format!("Не вдалося запустити yt-dlp: {error}")),
                    Err(error) => Err(format!("Завантаження перервано: {error}")),
                };
                log::info!("track={} yt-dlp download took {:.2}s", track.id, download_started.elapsed().as_secs_f64());

                match result {
                    Ok(path) => {
                        let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or_default();
                        let upload_started = Instant::now();
                        let sent = if matches!(extension, "mp3" | "m4a") {
                            bot.send_audio(chat_id, InputFile::file(&path))
                                .title(track.title.clone())
                                .performer(track.artist.clone())
                                .caption("Готово! Гарного прослуховування 🎶")
                                .await
                        } else {
                            bot.send_document(chat_id, InputFile::file(&path))
                                .caption(format!("🎵 {} — {}", track.title, track.artist))
                                .await
                        };
                        if let Ok(message) = &sent {
                            if let Some(audio) = message.audio() {
                                audio_cache.lock().await.insert(track.id.clone(), audio.file.id.clone());
                            }
                        }
                        log::info!("track={} Telegram upload took {:.2}s; total {:.2}s", track.id, upload_started.elapsed().as_secs_f64(), total_started.elapsed().as_secs_f64());
                        let _ = fs::remove_file(path).await;
                        if let Err(error) = sent {
                            bot.send_message(chat_id, format!("Не вдалося надіслати аудіо: {error}")).await?;
                        }
                    }
                    Err(error) => {
                        log::warn!("yt-dlp download failed: {error}");
                        bot.send_message(chat_id, "Не вдалося завантажити трек. Спробуй обрати інший або повторити пошук.").await?;
                    }
                }
                bot.delete_message(chat_id, status.id).await.ok();
                return Ok(());
            }

            if text.is_empty() || text.starts_with('/') {
                bot.send_message(chat_id, "Надішли назву пісні або виконавця, щоб почати пошук 🎵").await?;
                return Ok(());
            }

            let searching = bot.send_message(chat_id, "🔎 Шукаю пісні…").await?;
            let query = text.to_owned();
            let tracks = tokio::task::spawn_blocking(move || search_tracks(query)).await;
            bot.delete_message(chat_id, searching.id).await.ok();

            let tracks = match tracks {
                Ok(Ok(tracks)) => tracks,
                Ok(Err(error)) => {
                    log::warn!("yt-dlp search failed: {error}");
                    bot.send_message(chat_id, "Пошук зараз не вдався. Перевір запит і спробуй ще раз трохи пізніше.").await?;
                    return Ok(());
                }
                Err(error) => {
                    log::error!("Search task failed: {error}");
                    bot.send_message(chat_id, "Пошук перервався. Спробуй ще раз.").await?;
                    return Ok(());
                }
            };

            if tracks.is_empty() {
                bot.send_message(chat_id, "За цим запитом нічого не знайшов. Спробуй іншу назву або додай виконавця 🎵").await?;
                return Ok(());
            }

            let mut rows = Vec::new();
            let mut body = format!("🎧 <b>Знайшов за запитом «{}»:</b>\n\n", escape_html(text));
            for (index, track) in tracks.iter().enumerate() {
                body.push_str(&format!("<b>{}.</b> {} — {} <i>({})</i>\n", index + 1, escape_html(&track.title), escape_html(&track.artist), display_duration(track.duration)));
                rows.push(vec![KeyboardButton::new(track_button(index, track))]);
            }
            rows.push(vec![KeyboardButton::new("🔎 Інший пошук")]);
            let keyboard = KeyboardMarkup::new(rows).resize_keyboard(true).one_time_keyboard(true);
            searches.lock().await.insert(chat_id, tracks);
            bot.send_message(chat_id, body)
                .parse_mode(teloxide::types::ParseMode::Html)
                .reply_markup(keyboard)
                .await?;
            Ok::<(), teloxide::RequestError>(())
        }
    })
    .await;
}
