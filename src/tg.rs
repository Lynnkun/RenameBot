//! Everything that touches grammers (the MTProto client) lives here, so a version bump only needs edits in this file.
//!
//! Telegram has no "rename" call, so renamed files are uploaded again under the requested filename.
//!
//! * Every file whose container does not change (mkv -> mkv, mp4 -> mp4, pdf, zip ...) is streamed byte-for-byte.
//!   FFmpeg is NOT involved, so the video cannot be damaged.
//! * Only when a /single rename asks for another video container (mkv -> mp4) is the file downloaded to temporary
//!   storage and converted: lossless remux first, then "video untouched / audio re-encoded", and only as a last
//!   resort a full re-encode. Every result is checked with ffprobe (video stream present, duration not shortened).
//! * Everything is sent as a plain downloadable document, not Telegram's in-chat player mode.

use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use grammers_client::{
    types::{Downloadable, Media, Message},
    Client, InputMessage,
};
use grammers_session::{PackedChat, PackedType};
use grammers_tl_types as tl;
use tokio::{
    io::{duplex, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

use crate::kinds::{self, Kind};

/// Telegram's limit for bot uploads (2000 MiB).
pub const MAX_UPLOAD: usize = 2000 * 1024 * 1024;
/// Bytes the download half may run ahead of the upload half.
const PIPE_BYTES: usize = 8 * 1024 * 1024;
/// A download that delivers nothing for this long is abandoned (and retried by the caller).
const STALL: Duration = Duration::from_secs(90);

// ------------------------------------------------------------------------------------ channel access

/// Finds the channel from its id alone. A bot that is a member of a channel may ask for it with access hash 0 and
/// gets the real hash back - so nothing has to survive a redeploy.
pub async fn resolve_channel(client: &Client, bare_id: i64) -> Result<PackedChat> {
    let request = tl::functions::channels::GetChannels {
        id: vec![tl::enums::InputChannel::Channel(tl::types::InputChannel {
            channel_id: bare_id,
            access_hash: 0,
        })],
    };
    let chats = match client.invoke(&request).await? {
        tl::enums::messages::Chats::Chats(c) => c.chats,
        tl::enums::messages::Chats::Slice(c) => c.chats,
    };
    for chat in chats {
        if let tl::enums::Chat::Channel(channel) = chat {
            if channel.id == bare_id {
                let hash = channel.access_hash.ok_or_else(|| {
                    anyhow!("Telegram returned the channel without an access key")
                })?;
                let ty = if channel.megagroup {
                    PackedType::Megagroup
                } else if channel.gigagroup {
                    PackedType::Gigagroup
                } else {
                    PackedType::Broadcast
                };
                return Ok(PackedChat {
                    ty,
                    id: channel.id,
                    access_hash: Some(hash),
                });
            }
        }
    }
    bail!("the channel is not visible to this bot (is the bot a member/admin of it?)")
}

/// The real proof of access: post a message in the channel and delete it again.
pub async fn check_post_delete(client: &Client, chat: PackedChat) -> Result<()> {
    let sent = client
        .send_message(
            chat,
            InputMessage::text("🔧 access check - this message deletes itself").silent(true),
        )
        .await
        .context(
            "the bot cannot post in the dump channel (make it an admin with \"Post messages\")",
        )?;
    sent.delete()
        .await
        .context("the bot posted but cannot delete (give it the \"Delete messages\" right)")?;
    Ok(())
}

// ------------------------------------------------------------------------------------------- ranges

/// A file found in the requested range (metadata only).
pub struct FileInfo {
    pub msg_id: i32,
    pub name: String,
    pub mime: Option<String>,
    pub size: usize,
}

pub struct Found {
    pub files: Vec<FileInfo>,
    /// Messages in the range that were deleted or are not files (text, photos, polls...).
    pub skipped: usize,
}

/// Reads message ids `from..=to` (like FileBot's dump ranges) and keeps the ones that carry a file.
pub async fn fetch_range(client: &Client, chat: PackedChat, from: i32, to: i32) -> Result<Found> {
    let ids: Vec<i32> = (from..=to).collect();
    let mut files = Vec::new();
    let mut skipped = 0;

    for chunk in ids.chunks(100) {
        let messages = client.get_messages_by_id(chat, chunk).await?;
        for (id, message) in chunk.iter().zip(messages) {
            match message.as_ref().and_then(|m| m.media()) {
                Some(Media::Document(doc)) => files.push(FileInfo {
                    msg_id: *id,
                    name: doc.name().to_string(),
                    mime: doc.mime_type().map(str::to_string),
                    size: doc.size().max(0) as usize,
                }),
                _ => skipped += 1,
            }
        }
    }
    Ok(Found { files, skipped })
}

/// What the original document says about itself.
struct Meta {
    name: String,
    mime: Option<String>,
    size: usize,
}

fn describe(media: &Media) -> Option<Meta> {
    let Media::Document(doc) = media else {
        return None;
    };
    Some(Meta {
        name: doc.name().to_string(),
        mime: doc.mime_type().map(str::to_string),
        size: doc.size().max(0) as usize,
    })
}

// ------------------------------------------------------------------------------------- temp files

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
const TEMP_PREFIX: &str = "renamebot-";

/// Removes temp files left behind by a run that was killed in the middle of a conversion.
pub fn cleanup_temp() {
    if let Ok(entries) = fs::read_dir(std::env::temp_dir()) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Temporary input/output files used only while one video is being converted. Deleted on drop.
struct VideoTemp {
    input: PathBuf,
    output: PathBuf,
}

impl VideoTemp {
    fn new(extension: &str) -> Result<Self> {
        let dir = std::env::temp_dir();
        for _ in 0..100 {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let stem = format!("{TEMP_PREFIX}{}-{id}", std::process::id());
            let input = dir.join(format!("{stem}-input.bin"));
            let output = dir.join(format!("{stem}-output.{extension}"));
            if output.exists() {
                continue;
            }
            match OpenOptions::new().write(true).create_new(true).open(&input) {
                Ok(file) => {
                    drop(file);
                    return Ok(Self { input, output });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e).context("cannot create a temporary video file"),
            }
        }
        bail!("cannot allocate a unique temporary video filename")
    }
}

impl Drop for VideoTemp {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.input);
        let _ = fs::remove_file(&self.output);
    }
}

// ------------------------------------------------------------------------------------------ FFmpeg

fn video_muxer(extension: &str) -> Option<&'static str> {
    Some(match extension {
        "mp4" | "m4v" => "mp4",
        "mov" => "mov",
        "mkv" => "matroska",
        "webm" => "webm",
        "avi" | "divx" => "avi",
        "wmv" | "asf" => "asf",
        "flv" | "f4v" => "flv",
        "mpg" | "mpeg" | "vob" => "mpeg",
        "3gp" | "3g2" => "3gp",
        "ogv" => "ogg",
        "ts" | "m2ts" | "mts" => "mpegts",
        _ => return None,
    })
}

struct Codecs {
    video: &'static [&'static str],
    audio: &'static [&'static str],
}

/// Common encoders for a container, used only when the streams cannot be copied into it.
fn codecs(extension: &str) -> Option<Codecs> {
    Some(match extension {
        "mp4" | "m4v" | "mov" | "mkv" | "3gp" | "3g2" | "flv" | "f4v" | "ts" | "m2ts" | "mts" => {
            Codecs {
                video: &[
                    "-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-pix_fmt", "yuv420p",
                ],
                audio: &["-c:a", "aac", "-b:a", "192k"],
            }
        }
        "webm" => Codecs {
            video: &[
                "-c:v",
                "libvpx-vp9",
                "-deadline",
                "realtime",
                "-cpu-used",
                "5",
                "-crf",
                "30",
                "-b:v",
                "0",
            ],
            audio: &["-c:a", "libopus", "-b:a", "128k"],
        },
        "avi" | "divx" => Codecs {
            video: &["-c:v", "mpeg4", "-q:v", "4"],
            audio: &["-c:a", "libmp3lame", "-q:a", "3"],
        },
        "mpg" | "mpeg" | "vob" => Codecs {
            video: &["-c:v", "mpeg2video", "-q:v", "4"],
            audio: &["-c:a", "mp2", "-b:a", "192k"],
        },
        "ogv" => Codecs {
            video: &["-c:v", "libtheora", "-q:v", "7"],
            audio: &["-c:a", "libvorbis", "-q:a", "4"],
        },
        "wmv" | "asf" => Codecs {
            video: &["-c:v", "wmv2", "-b:v", "2M"],
            audio: &["-c:a", "wmav2", "-b:a", "128k"],
        },
        _ => return None,
    })
}

/// The last part of a (possibly huge) FFmpeg message, on a char boundary.
fn tail(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count <= max_chars {
        text.to_string()
    } else {
        let skipped: String = text.chars().skip(count - max_chars).collect();
        format!("…{skipped}")
    }
}

fn ffmpeg_args(
    input: &Path,
    output: &Path,
    extension: &str,
    muxer: &str,
    video: &[&str],
    audio: &[&str],
) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    let mut push = |items: &[&str]| a.extend(items.iter().map(OsString::from));

    push(&[
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-y",
        "-fflags",
        "+genpts",
        "-i",
    ]);
    a.push(input.into());
    let mut push = |items: &[&str]| a.extend(items.iter().map(OsString::from));
    // `0:V` = real video streams (no embedded cover art), `0:a?` = all audio. Data streams are dropped.
    push(&["-map", "0:V", "-map", "0:a?"]);
    let matroska = extension == "mkv";
    if matroska {
        // only Matroska accepts any subtitle format and attachments (fonts) as they are
        push(&[
            "-map", "0:s?", "-map", "0:t?", "-c:s", "copy", "-c:t", "copy",
        ]);
    }
    push(&["-map_metadata", "0", "-map_chapters", "0"]);
    push(video);
    push(audio);
    if matches!(extension, "mp4" | "m4v" | "mov" | "3gp" | "3g2") {
        push(&["-movflags", "+faststart"]);
    }
    push(&["-f", muxer]);
    a.push(output.into());
    a
}

/// Runs FFmpeg; kills it as soon as the job is cancelled.
async fn run_ffmpeg(args: &[OsString], cancel: &AtomicBool) -> Result<()> {
    let mut child = Command::new("ffmpeg")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("FFmpeg is required for video files; install FFmpeg in the runtime image")?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("cannot read FFmpeg's output"))?;
    let reader = tokio::spawn(async move {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text).await;
        text
    });

    let status = loop {
        tokio::select! {
            result = child.wait() => break result?,
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                if cancel.load(Ordering::Relaxed) {
                    let _ = child.kill().await;
                    bail!("cancelled");
                }
            }
        }
    };
    let text = reader.await.unwrap_or_default();
    if !status.success() {
        bail!(
            "{}",
            if text.trim().is_empty() {
                format!("process exited with {status}")
            } else {
                tail(&text, 400)
            }
        );
    }
    Ok(())
}

struct Probe {
    has_video: bool,
    duration: Option<f64>,
}

async fn probe(path: &Path) -> Result<Probe> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type:format=duration",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .await
        .context("ffprobe is required to validate videos; install FFmpeg")?;
    if !out.status.success() {
        bail!("{}", tail(&String::from_utf8_lossy(&out.stderr), 300));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut result = Probe {
        has_video: false,
        duration: None,
    };
    for line in text.lines() {
        let line = line.trim();
        if line == "codec_type=video" {
            result.has_video = true;
        } else if let Some(d) = line.strip_prefix("duration=") {
            result.duration = d.parse::<f64>().ok().filter(|d| d.is_finite() && *d > 0.0);
        }
    }
    Ok(result)
}

/// An output is accepted only when it has a video stream and is not noticeably shorter than the source.
async fn check_output(output: &Path, source: &Probe) -> Result<()> {
    let size = fs::metadata(output)
        .with_context(|| format!("FFmpeg did not create {}", output.display()))?
        .len();
    if size == 0 {
        bail!("FFmpeg created an empty video file");
    }
    let out = probe(output).await?;
    if !out.has_video {
        bail!("the converted file has no video stream");
    }
    if let (Some(before), Some(after)) = (source.duration, out.duration) {
        if before > 5.0 && after < before * 0.97 {
            bail!("the converted file is shorter than the original ({after:.0}s of {before:.0}s)");
        }
    }
    Ok(())
}

/// Converts into the requested container. Returns a note when the result is not a pure lossless remux.
async fn convert_video(
    input: &Path,
    output: &Path,
    extension: &str,
    cancel: &AtomicBool,
) -> Result<Option<String>> {
    let muxer = video_muxer(extension)
        .ok_or_else(|| anyhow!("unsupported video output container .{extension}"))?;
    let source = probe(input)
        .await
        .map_err(|e| anyhow!("FFmpeg could not read the source video: {e:#}"))?;
    if !source.has_video {
        bail!("FFmpeg could not read the source video: it has no video stream");
    }

    const COPY_V: &[&str] = &["-c:v", "copy"];
    const COPY_A: &[&str] = &["-c:a", "copy"];
    let mut attempts: Vec<(Option<&str>, &[&str], &[&str])> = vec![(None, COPY_V, COPY_A)];
    if let Some(c) = codecs(extension) {
        attempts.push((Some("audio re-encoded, video untouched"), COPY_V, c.audio));
        attempts.push((Some("fully re-encoded"), c.video, c.audio));
    }

    let mut last = String::new();
    for (note, video, audio) in attempts {
        let _ = fs::remove_file(output);
        let args = ffmpeg_args(input, output, extension, muxer, video, audio);
        let outcome = match run_ffmpeg(&args, cancel).await {
            Ok(()) => check_output(output, &source).await,
            Err(e) => Err(e),
        };
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        match outcome {
            Ok(()) => return Ok(note.map(str::to_string)),
            Err(e) => {
                log::info!(
                    "conversion attempt ({}) failed: {e:#}",
                    note.unwrap_or("lossless remux")
                );
                last = format!("{e:#}");
            }
        }
    }
    let _ = fs::remove_file(output);
    bail!("FFmpeg could not convert this video into .{extension}: {last}")
}

// -------------------------------------------------------------------------------------- the transfer

/// Transfers one file under `new_name`, attaches a thumbnail when supported and sends it as a plain document.
/// `progress` counts source bytes read; `cancel` aborts the transfer. Returns an optional note for the report.
#[allow(clippy::too_many_arguments)]
pub async fn send_renamed(
    client: &Client,
    dest: PackedChat,
    media: &Media,
    new_name: &str,
    caption: Option<&str>,
    thumbnail: Option<&[u8]>,
    progress: &Arc<AtomicU64>,
    cancel: &Arc<AtomicBool>,
) -> Result<Option<String>> {
    let meta = describe(media).ok_or_else(|| anyhow!("the message does not carry a file"))?;
    if meta.size == 0 {
        bail!("the file is empty (0 bytes) - Telegram cannot store that");
    }
    if meta.size > MAX_UPLOAD {
        bail!("the file is larger than 2 GB - bots cannot upload that");
    }
    let source_kind = kinds::classify(meta.mime.as_deref(), &meta.name);

    // `convert_to` is Some only when a video must really change container; otherwise bytes are copied untouched.
    let (mime, kind, convert_to): (Option<String>, Kind, Option<String>) = if source_kind
        == Kind::Video
    {
        // the container the file really has: from its name, else from its MIME type
        let source_ext = kinds::file_ext(&meta.name).or_else(|| {
            meta.mime
                .as_deref()
                .and_then(kinds::ext_from_mime)
                .map(str::to_string)
        });
        let target_ext = kinds::file_ext(new_name)
            .or_else(|| source_ext.clone())
            .ok_or_else(|| {
                anyhow!("the video has no recognized output extension; use .mp4, .mkv, .webm, .mov, .avi or another video extension")
            })?;
        let target_muxer = video_muxer(&target_ext).ok_or_else(|| {
            anyhow!("unsupported video output container .{target_ext}; use .mp4, .mkv, .webm, .mov, .avi, .flv, .ts, .mpg or another video format")
        })?;
        let same_container = source_ext
            .as_deref()
            .and_then(video_muxer)
            .map_or(true, |m| m == target_muxer);
        let output_mime = kinds::mime_from_ext(&target_ext)
            .filter(|m| m.starts_with("video/"))
            .map(str::to_string)
            .or_else(|| meta.mime.clone());
        (
            output_mime,
            Kind::Video,
            if same_container {
                None
            } else {
                Some(target_ext)
            },
        )
    } else {
        let mime = kinds::pick_mime(meta.mime.as_deref(), &meta.name, new_name);
        let kind = kinds::classify(mime.as_deref(), new_name);
        if kind == Kind::Video {
            bail!("this source is not recognized as a video; changing its extension to a video format would not convert it");
        }
        (mime, kind, None)
    };

    let mut note: Option<String> = None;
    let (uploaded, stored_size) = if let Some(extension) = convert_to {
        // ---- container change: download -> FFmpeg -> upload
        let temp = VideoTemp::new(&extension)?;
        let mut file = tokio::fs::File::create(&temp.input)
            .await
            .context("cannot open a temporary file to prepare the video")?;
        let source = Downloadable::Media(media.clone());
        let mut chunks = client.iter_download(&source);
        let mut total = 0usize;
        loop {
            let next = tokio::time::timeout(STALL, chunks.next())
                .await
                .map_err(|_| anyhow!("download stalled (no data for {} s)", STALL.as_secs()))??;
            let Some(chunk) = next else { break };
            if cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            total += chunk.len();
            progress.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        drop(file);
        if total != meta.size {
            bail!("the download delivered {total} of {} bytes", meta.size);
        }

        note = convert_video(&temp.input, &temp.output, &extension, cancel).await?;

        let output_size = usize::try_from(fs::metadata(&temp.output)?.len())
            .map_err(|_| anyhow!("the converted video is too large for this platform"))?;
        if output_size > MAX_UPLOAD {
            bail!("the converted video is larger than 2 GB; it cannot be uploaded");
        }
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let mut converted = tokio::fs::File::open(&temp.output).await?;
        let uploaded = client
            .upload_stream(&mut converted, output_size, new_name.to_string())
            .await?;
        (uploaded, output_size)
    } else {
        // ---- same container: stream the original bytes straight into the new upload
        let size = meta.size;
        let (mut reader, mut writer) = duplex(PIPE_BYTES);
        let dl_client = client.clone();
        let dl_source = Downloadable::Media(media.clone());
        let dl_progress = progress.clone();
        let dl_cancel = cancel.clone();

        let download = tokio::spawn(async move {
            let mut chunks = dl_client.iter_download(&dl_source);
            let mut total = 0usize;
            loop {
                let next = tokio::time::timeout(STALL, chunks.next())
                    .await
                    .map_err(|_| {
                        anyhow!("download stalled (no data for {} s)", STALL.as_secs())
                    })??;
                let Some(chunk) = next else { break };
                if dl_cancel.load(Ordering::Relaxed) {
                    bail!("cancelled");
                }
                total += chunk.len();
                dl_progress.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                writer.write_all(&chunk).await?;
            }
            writer.shutdown().await?;
            if total != size {
                bail!("the download delivered {total} of {size} bytes");
            }
            Ok::<(), anyhow::Error>(())
        });

        let upload_result = client
            .upload_stream(&mut reader, size, new_name.to_string())
            .await;
        drop(reader); // unblocks the download task if the upload failed first
        let download_result = match download.await {
            Ok(result) => result,
            Err(join_error) => Err(anyhow::Error::from(join_error)),
        };
        let uploaded = match (upload_result, download_result) {
            (Ok(uploaded), Ok(())) => uploaded,
            (Ok(_), Err(download_error)) => return Err(download_error),
            (Err(upload_error), Err(download_error)) => {
                if format!("{download_error:#}")
                    .to_ascii_lowercase()
                    .contains("broken pipe")
                {
                    return Err(anyhow::Error::from(upload_error));
                }
                return Err(download_error);
            }
            (Err(upload_error), Ok(())) => return Err(anyhow::Error::from(upload_error)),
        };
        (uploaded, size)
    };

    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }

    // Set the MIME type before attaching the media. `file` deliberately sends a downloadable document, not a player.
    let mut input = InputMessage::text(caption.unwrap_or(""));
    if let Some(m) = mime.as_deref() {
        input = input.mime_type(m);
    }
    input = input.file(uploaded);
    if kind.takes_thumbnail() {
        if let Some(bytes) = thumbnail {
            let mut thumb_reader: &[u8] = bytes;
            let thumb_upload = client
                .upload_stream(&mut thumb_reader, bytes.len(), "thumb.jpg".to_string())
                .await?;
            input = input.thumbnail(thumb_upload);
        }
    }

    let sent = client.send_message(dest, input).await?;
    verify_stored(&sent, stored_size).await?;
    Ok(note)
}

/// Largest image the bot accepts as a custom thumbnail source.
const MAX_THUMB_SOURCE: usize = 20 * 1024 * 1024;

/// Downloads an image the admin sent as the custom thumbnail: either a normal Telegram photo or an image sent as
/// a file. Anything else (video, pdf, sticker ...) is refused with a message the admin can act on.
pub async fn download_image(client: &Client, media: &Media) -> Result<Vec<u8>> {
    match media {
        Media::Photo(_) => {}
        Media::Document(doc) => {
            let mime_ok = doc.mime_type().map_or(false, |m| m.starts_with("image/"));
            let ext_ok = kinds::file_ext(doc.name()).map_or(false, |e| {
                matches!(
                    e.to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff"
                )
            });
            if !(mime_ok || ext_ok) {
                bail!("that file is not an image");
            }
            if doc.size().max(0) as usize > MAX_THUMB_SOURCE {
                bail!("that image is larger than 20 MB");
            }
        }
        _ => bail!("that is not an image"),
    }

    let source = Downloadable::Media(media.clone());
    let mut chunks = client.iter_download(&source);
    let mut out: Vec<u8> = Vec::new();
    loop {
        let next = tokio::time::timeout(STALL, chunks.next())
            .await
            .map_err(|_| anyhow!("download stalled (no data for {} s)", STALL.as_secs()))??;
        let Some(chunk) = next else { break };
        if out.len() + chunk.len() > MAX_THUMB_SOURCE {
            bail!("that image is larger than 20 MB");
        }
        out.extend_from_slice(&chunk);
    }
    if out.is_empty() {
        bail!("the image is empty");
    }
    Ok(out)
}

/// What Telegram stored must be exactly what was uploaded; a wrong copy is deleted so it cannot be mistaken for good.
async fn verify_stored(sent: &Message, expected: usize) -> Result<()> {
    let Some(Media::Document(doc)) = sent.media() else {
        let _ = sent.delete().await;
        bail!("Telegram did not return the upload as a document/file; the copy was deleted");
    };

    let stored = doc.size().max(0) as usize;
    if stored != expected {
        let _ = sent.delete().await;
        bail!(
            "Telegram stored {stored} bytes instead of {expected} - the damaged copy was deleted"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_is_char_safe() {
        assert_eq!(tail("  short  ", 10), "short");
        assert_eq!(tail("ééééé", 2), "…éé");
    }

    #[test]
    fn muxers() {
        assert_eq!(video_muxer("mkv"), Some("matroska"));
        assert_eq!(video_muxer("m4v"), video_muxer("mp4"));
        assert_eq!(video_muxer("pdf"), None);
        assert!(codecs("mp4").is_some() && codecs("rmvb").is_none());
    }
}
