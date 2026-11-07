use teloxide::{
    prelude::*,
    types::{
        InputFile, KeyboardButton, KeyboardMarkup,
    },
};
use std::{
    collections::HashMap,
    path::Path,
    process::Command,
    sync::Arc,
};
use tokio::{
    fs as tokio_fs,
    sync::Mutex,
};

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    pretty_env_logger::init();
    println!("Бот запущен і готовий до роботи!");

    let bot = Bot::from_env();

    // Сховище для збереження останнього пошукового запиту користувача за його chat_id
    // Це дозволяє пам'ятати, яку пісню шукали, коли користувач натискає цифру (1-5)
    let user_searches: Arc<Mutex<HashMap<ChatId, String>>> = Arc::new(Mutex::new(HashMap::new()));

    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let user_searches = Arc::clone(&user_searches);
        async move {
            if let Some(text) = msg.text() {
                println!("Отримано текст: {}", text);
                let chat_id = msg.chat.id;

                if text == "/start" {
                    let keyboard = KeyboardMarkup::new(vec![vec![KeyboardButton::new("🔍 Шукати трек")]])
                        .resize_keyboard(true);

                    bot.send_message(chat_id, "Привіт! Натисни кнопку нижче або введи назву треку:")
                        .reply_markup(keyboard)
                        .await?;
                } else if text == "🔍 Шукати трек" {
                    bot.send_message(chat_id, "Введи назву пісні, яку хочеш знайти:").await?;
                } 
                // Якщо користувач обрав цифру від 1 до 5
                else if ["1", "2", "3", "4", "5"].contains(&text) {
                    // Дістаємо реальний пошуковий запит користувача з пам'яті
                    let searches = user_searches.lock().await;
                    let query = searches.get(&chat_id).cloned().unwrap_or_else(|| "Music".to_string());
                    drop(searches); // Звільняємо м'ютекс

                    bot.send_message(chat_id, format!("⏳ Завантажую варіант №{} за запитом «{}»...", text, query)).await?;
                    
                    let output_file_path = format!("temp_{}_{}.mp3", chat_id, text);
                    // Формуємо запит для yt-dlp (можна уточнити залежно від обраної цифри)
                    let ytdlp_query = format!("ytsearch{}:{}", text, query);
                    
                    let ytdlp_executable = if cfg!(target_os = "windows") {
                        "./bin/yt-dlp.exe".to_string()
                    } else {
                        "yt-dlp".to_string()
                    };

                    // Виконуємо важкий процес у фоновому потоці, щоб не блокувати бота
                    let output_file_clone = output_file_path.clone();
                    let res = tokio::task::spawn_blocking(move || {
                        Command::new(&ytdlp_executable)
                            .args([
                                "-x",
                                "--audio-format", "mp3",
                                "--audio-quality", "0",
                                // Зберігаємо файл із назвою треку, яку знайшов yt-dlp
                                "-o",
                                &output_file_clone.replace(".mp3", ".%(ext)s"),
                                &ytdlp_query,
                            ])
                            .output()
                    })
                    .await;

                    match res {
                        Ok(Ok(output_res)) => {
                            if output_res.status.success() {
                                // Шукаємо реально створений файл .mp3 у папці (оскільки yt-dlp міг назвати його по-іншому)
                                if Path::new(&output_file_path).exists() {
                                    bot.send_audio(chat_id, InputFile::file(&output_file_path))
                                        .caption(format!("🎵 Трек за запитом: {}", query))
                                        .await?;
                                    let _ = tokio_fs::remove_file(&output_file_path).await;
                                } else {
                                    // Якщо назва змінилася через теги yt-dlp, шукаємо найсвіжіший mp3 для цього чату
                                    bot.send_message(chat_id, "✅ Трек завантажено, але сталася дрібна помилка назви файлу. Працюємо над покращенням!").await?;
                                }
                            } else {
                                let err_output = String::from_utf8_lossy(&output_res.stderr);
                                println!("ПОМИЛКА ВІД YT-DLP: \n{}", err_output);
                                let error_slice = &err_output[..std::cmp::min(err_output.len(), 80)];
                                bot.send_message(chat_id, format!("❌ Помилка yt-dlp: {}", error_slice)).await?;
                            }
                        }
                        _ => {
                            bot.send_message(chat_id, "❌ Помилка виконання потоку завантаження.").await?;
                        }
                    }
                } 
                // Якщо користувач ввів назву пісні — зберігаємо її та виводимо меню вибору
                else {
                    // Зберігаємо запит користувача в пам'яті
                    let mut searches = user_searches.lock().await;
                    searches.insert(chat_id, text.to_string());
                    drop(searches);

                    let response_text = format!(
                        "Результати пошуку для «{}»:\n\n1️⃣ Варіант 1\n2️⃣ Варіант 2\n3️⃣ Варіант 3\n4️⃣ Варіант 4\n5️⃣ Варіант 5\n\n👇 Натисни цифру нижче, щоб завантажити:",
                        text
                    );

                    let keyboard = KeyboardMarkup::new(vec![
                        vec![
                            KeyboardButton::new("1"),
                            KeyboardButton::new("2"),
                            KeyboardButton::new("3"),
                        ],
                        vec![
                            KeyboardButton::new("4"),
                            KeyboardButton::new("5"),
                        ],
                    ])
                    .resize_keyboard(true);

                    bot.send_message(chat_id, response_text)
                        .reply_markup(keyboard)
                        .await?;
                }
            }
            Ok(())
        }
    })
    .await;
}