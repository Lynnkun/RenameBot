//! Commands, the guided conversation (mostly buttons), and the job runner.
//!
//! Nothing to remember: just send
//!   two dump-channel links (first + last)  -> a batch
//!   one link, or a file                     -> a single file
//! The bot then asks for the name and shows ONE card with buttons: which tags go into the file name
//! (Episode, Season, Quality, Audio, Codec, Source, Subs, Size), the season, the thumbnail (logo / your image / none)
//! and an optional caption. The extension always comes from the original file.
//! /season, /bulk and /single are shortcuts that only change the starting buttons.
//!
//! Jobs go into ONE first-in-first-out queue. A single worker takes them one by one and renames the files of a
//! job strictly one after another, in message order - nothing is ever renamed in parallel.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Result};
use grammers_client::{
    button, reply_markup,
    types::{Chat, Media, Message},
    Client, InputMessage,
};
use grammers_session::{PackedChat, PackedType};
use tokio::sync::{
    mpsc::{unbounded_channel, UnboundedSender},
    Notify,
};

use crate::{
    config::Config,
    link::{self, ChatRef, MsgLink},
    naming::{self, ExtMode},
    tags::{self, Tag, TagSet},
    tg::{self, FileInfo},
    thumb,
};

const HELP: &str = "\
👋 Rename bot - just send me:
• two dump-channel links (first + last) → rename a whole batch
• one link, or a file → rename that one file

Then send the name and tap the buttons. That's all.

🏷 Buttons
Episode · Season · Quality · Audio · Codec · Source · Subs · Size
They are added to the name like [E-1][S-2][1080p][Hindi] My Show.mkv.
Quality, audio... are read from the original file name and skipped when not found. The extension is kept automatically.
🖼 Thumbnail: logo · your image (just send it) · none
💬 Caption: optional text under every file

✏️ In the name
{E:54} starts counting at 54 · {S} puts the season where you want it · {N} = original name
Example: {E:54} My Show → [E-54][S-2] My Show.mkv, [E-55][S-2] My Show.mkv …

/queue see the line · /cancel stop everything
/season · /bulk · /single jump straight into a mode";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Not decided yet: one link may still turn into a range, or into a single file.
    Auto,
    Batch,
    Single,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ThumbSel {
    Logo,
    Custom,
    Off,
}

/// The thumbnail a job really uses.
#[derive(Clone)]
enum Thumb {
    Logo,
    Custom(Vec<u8>),
    Off,
}

/// What an admin picked last time, so the next job starts with the same buttons.
#[derive(Clone, Copy)]
struct Prefs {
    tags: TagSet,
    season: u32,
    thumb_off: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        let mut tags = TagSet::default();
        tags.set(Tag::Episode, true);
        Self {
            tags,
            season: 1,
            thumb_off: false,
        }
    }
}

#[derive(Clone)]
struct PlanItem {
    msg_id: i32,
    new_name: String,
    size: usize,
    caption: Option<String>,
}

#[derive(Clone)]
struct Plan {
    chat: PackedChat,
    items: Vec<PlanItem>,
    skipped: usize,
    thumb: Thumb,
    /// Selected tags that were not found in some names.
    warnings: Vec<String>,
}

enum Step {
    Links,
    End,
    Name,
    Options,
    Caption,
    Confirm(Plan),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum K {
    Name,
    Options,
    Caption,
    Confirm,
    Other,
}

struct Flow {
    mode: Mode,
    step: Step,
    chat: Option<PackedChat>,
    chat_ref: Option<ChatRef>,
    from: i32,
    to: i32,
    files: Vec<FileInfo>,
    skipped: usize,
    /// The name template as typed ("{N}" = keep the original names).
    name: String,
    tags: TagSet,
    season: u32,
    thumb: ThumbSel,
    /// The admin's own image (already a Telegram-ready JPEG), for this job only.
    custom: Option<Vec<u8>>,
    caption: Option<String>,
    /// The message that carries the live buttons.
    card: i32,
}

impl Flow {
    fn new(mode: Mode, p: Prefs) -> Self {
        Self {
            mode,
            step: Step::Links,
            chat: None,
            chat_ref: None,
            from: 0,
            to: 0,
            files: Vec::new(),
            skipped: 0,
            name: String::new(),
            tags: p.tags,
            season: p.season,
            thumb: if p.thumb_off {
                ThumbSel::Off
            } else {
                ThumbSel::Logo
            },
            custom: None,
            caption: None,
            card: 0,
        }
    }

    fn is_batch(&self) -> bool {
        self.mode == Mode::Batch
    }

    fn kind(&self) -> K {
        match self.step {
            Step::Name => K::Name,
            Step::Options => K::Options,
            Step::Caption => K::Caption,
            Step::Confirm(_) => K::Confirm,
            _ => K::Other,
        }
    }
}

/// Most jobs one admin may have waiting.
const MAX_QUEUED: usize = 20;

struct QueuedJob {
    uid: i64,
    dest: PackedChat,
    plan: Plan,
    cancel: Arc<AtomicBool>,
}

struct Running {
    uid: i64,
    cancel: Arc<AtomicBool>,
    files: usize,
}

pub struct App {
    pub client: Client,
    pub cfg: Config,
    flows: Mutex<HashMap<i64, Flow>>,
    /// Waiting jobs, first in first out.
    queue: Mutex<VecDeque<QueuedJob>>,
    running: Mutex<Option<Running>>,
    wake: Notify,
    /// One ordered inbox per admin, so two quick messages (first link, last link) are handled in order.
    inboxes: Mutex<HashMap<i64, UnboundedSender<Message>>>,
    /// The dump channel with its access hash. The hash belongs to THIS bot. It is found from the channel id alone
    /// (`ensure_dump`), proven by posting + deleting a test message, and cached on disk only as a convenience.
    dump: Mutex<Option<PackedChat>>,
    /// Admins who ran into "dump channel unknown" - told as soon as the channel is learned.
    waiting: Mutex<Vec<PackedChat>>,
    /// What each admin picked last time (tags, season, thumbnail on/off): the next job starts with the same buttons.
    prefs: Mutex<HashMap<i64, Prefs>>,
}

impl App {
    pub fn new(client: Client, cfg: Config) -> Self {
        let dump = load_dump(&cfg);
        Self {
            client,
            cfg,
            flows: Mutex::new(HashMap::new()),
            queue: Mutex::new(VecDeque::new()),
            running: Mutex::new(None),
            wake: Notify::new(),
            inboxes: Mutex::new(HashMap::new()),
            dump: Mutex::new(dump),
            waiting: Mutex::new(Vec::new()),
            prefs: Mutex::new(HashMap::new()),
        }
    }
}

const UNKNOWN_DUMP: &str = "I can't reach the dump channel. Make sure this bot is an ADMIN of the dump channel (it needs \
\"Post messages\" and \"Delete messages\" - I prove access by posting a test message and deleting it again) and that \
DUMP_CHANNEL_ID is right. Then post any message in the channel - I connect on my own and tell you here, no restart needed.";

const LOST_DUMP: &str = "❌ Telegram refuses this bot access to the dump channel (this happens after BOT_TOKEN was changed or \
the bot was removed). Add this bot as admin of the dump channel and post any message there - I reconnect on my own and tell \
you here. No restart needed.";

/// Telegram messages are limited to 4096 characters; FFmpeg errors can be far longer.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

// ------------------------------------------------------------------------------------ dump channel

fn dump_chat(app: &App) -> Option<PackedChat> {
    app.dump.lock().unwrap().clone()
}

fn type_tag(ty: PackedType) -> &'static str {
    match ty {
        PackedType::Megagroup => "m",
        PackedType::Gigagroup => "g",
        _ => "b",
    }
}

fn type_from_tag(tag: &str) -> PackedType {
    match tag {
        "m" => PackedType::Megagroup,
        "g" => PackedType::Gigagroup,
        _ => PackedType::Broadcast,
    }
}

/// "<bot id> <channel id> <access hash> <type>" saved by `store_dump`. Ignored when it was written for another bot.
fn load_dump(cfg: &Config) -> Option<PackedChat> {
    let text = std::fs::read_to_string(&cfg.dump_cache).ok()?;
    let parts: Vec<&str> = text.split_whitespace().collect();
    let (bot, id, hash, ty) = match parts.as_slice() {
        [bot, id, hash, ty] => (
            bot.parse::<i64>().ok()?,
            id.parse::<i64>().ok()?,
            hash.parse::<i64>().ok()?,
            type_from_tag(ty),
        ),
        // older versions saved just "<channel id> <access hash>"
        [id, hash] => (
            cfg.bot_id,
            id.parse::<i64>().ok()?,
            hash.parse::<i64>().ok()?,
            PackedType::Broadcast,
        ),
        _ => return None,
    };
    (bot == cfg.bot_id && id == cfg.dump_bare()).then(|| PackedChat {
        ty,
        id,
        access_hash: Some(hash),
    })
}

fn store_dump(app: &App, packed: PackedChat) {
    let line = format!(
        "{} {} {} {}",
        app.cfg.bot_id,
        packed.id,
        packed.access_hash.unwrap_or(0),
        type_tag(packed.ty)
    );
    if let Err(e) = std::fs::write(&app.cfg.dump_cache, line) {
        log::warn!("could not save the dump channel key: {e}");
    }
    *app.dump.lock().unwrap() = Some(packed);
}

/// Forget a key that stopped working (wrong bot, bot removed from the channel ...).
fn invalidate_dump(app: &App) {
    *app.dump.lock().unwrap() = None;
    let _ = std::fs::remove_file(&app.cfg.dump_cache);
    log::warn!("the saved dump channel key does not work (any more) - waiting for a post in the channel to learn it again");
}

fn remember_waiting(app: &App, chat: PackedChat) {
    let mut waiting = app.waiting.lock().unwrap();
    if !waiting.iter().any(|c| c.id == chat.id) {
        waiting.push(chat);
    }
}

async fn notify_waiting(app: &App) {
    let list: Vec<PackedChat> = std::mem::take(&mut *app.waiting.lock().unwrap());
    for chat in list {
        let _ = app
            .client
            .send_message(
                chat,
                "✅ Connected to the dump channel. Send your command / links again.",
            )
            .await;
    }
}

/// A post in the dump channel reaches the bot as an update, and that update carries the channel's access hash.
async fn learn_dump(app: &App, msg: &Message) {
    let chat = msg.chat();
    if chat.id() != app.cfg.dump_bare() {
        return;
    }
    let packed = chat.pack();
    let Some(hash) = packed.access_hash else {
        return;
    };
    if dump_chat(app).and_then(|c| c.access_hash) == Some(hash) {
        return;
    }
    store_dump(app, packed);
    log::info!("learned the dump channel ({})", packed.id);
    notify_waiting(app).await;
}

/// Gives a dump channel that is PROVEN to work: the cached key is tested by posting + deleting a message; if that
/// fails (or nothing is cached - e.g. after a redeploy on a host without a volume) the channel is looked up from its
/// id and tested again. `Err` carries the text to show the admin.
async fn ensure_dump(app: &App) -> std::result::Result<PackedChat, String> {
    if let Some(chat) = dump_chat(app) {
        match tg::check_post_delete(&app.client, chat).await {
            Ok(()) => return Ok(chat),
            Err(e) => {
                log::warn!("saved dump channel key failed the post/delete test: {e:#}");
                invalidate_dump(app);
            }
        }
    }
    match tg::resolve_channel(&app.client, app.cfg.dump_bare()).await {
        Ok(chat) => match tg::check_post_delete(&app.client, chat).await {
            Ok(()) => {
                log::info!(
                    "dump channel {} found and verified (test message posted and deleted)",
                    chat.id
                );
                store_dump(app, chat);
                Ok(chat)
            }
            Err(e) => {
                log::warn!("dump channel found but the post/delete test failed: {e:#}");
                Err(format!(
                    "❌ I found the dump channel but: {}",
                    clip(&format!("{e:#}"), 400)
                ))
            }
        },
        Err(e) => {
            log::info!("dump channel lookup failed: {e:#}");
            Err(UNKNOWN_DUMP.to_string())
        }
    }
}

/// Runs once at start-up so a broken channel shows in the log immediately, not at the first command.
pub async fn startup_check(app: &Arc<App>) {
    match ensure_dump(app).await {
        Ok(_) => log::info!("dump channel OK"),
        Err(why) => log::warn!("dump channel NOT usable yet: {}", why.replace('\n', " ")),
    }
}

fn is_channel_error(e: &anyhow::Error) -> bool {
    let text = format!("{e:#}");
    [
        "CHANNEL_INVALID",
        "CHANNEL_PRIVATE",
        "PEER_ID_INVALID",
        "CHAT_ID_INVALID",
        "CHAT_FORBIDDEN",
    ]
    .iter()
    .any(|k| text.contains(k))
}

/// Errors a retry cannot fix.
fn is_permanent(e: &anyhow::Error) -> bool {
    if is_channel_error(e) {
        return true;
    }
    let text = format!("{e:#}").to_ascii_lowercase();
    [
        "is gone",
        "has no file",
        "does not carry a file",
        "is empty",
        "larger than 2 gb",
        "cancelled",
        "unsupported video output container",
        "ffmpeg could not",
        "no video stream",
        "ffmpeg is required",
        "ffprobe is required",
        "ffmpeg output does not contain a valid video stream",
        "ffmpeg produced an invalid",
        "ffmpeg created an empty video file",
        "this source is not recognized as a video",
        "the normalized video is larger than 2 gb",
    ]
    .iter()
    .any(|k| text.contains(k))
}

/// Every link must point into the dump channel (t.me/c/<id>/<msg>); the chat part of a link is otherwise ignored.
fn check_dump_links(app: &App, links: &[MsgLink]) -> std::result::Result<(), String> {
    let ok = links
        .iter()
        .all(|l| matches!(&l.chat, ChatRef::Private(id) if *id == app.cfg.dump_bare()));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "❌ Only links from the dump channel work: https://t.me/c/{}/<message id>",
            app.cfg.dump_bare()
        ))
    }
}

// ---------------------------------------------------------------------------------------------- entry

/// Called for every incoming message. Admin messages go through a per-admin inbox, handled strictly in order.
pub fn dispatch(app: &Arc<App>, msg: Message) {
    if !matches!(msg.chat(), Chat::User(_)) {
        tokio::spawn(on_message(app.clone(), msg)); // channel posts: only used to learn the dump channel
        return;
    }
    let uid = msg.chat().id();
    if !app.cfg.admins.contains(&uid) {
        return; // silently ignore everyone else
    }
    let tx = {
        let mut inboxes = app.inboxes.lock().unwrap();
        inboxes
            .entry(uid)
            .or_insert_with(|| {
                let (tx, mut rx) = unbounded_channel::<Message>();
                let app = app.clone();
                tokio::spawn(async move {
                    while let Some(msg) = rx.recv().await {
                        // a panic in one message must not kill the inbox
                        let _ = tokio::spawn(on_message(app.clone(), msg)).await;
                    }
                });
                tx
            })
            .clone()
    };
    let _ = tx.send(msg);
}

async fn on_message(app: Arc<App>, msg: Message) {
    if let Err(e) = handle(&app, &msg).await {
        log::warn!("handler error: {e:#}");
        let _ = msg
            .reply(format!("❌ {}", clip(&format!("{e}"), 500)))
            .await;
    }
}

async fn handle(app: &Arc<App>, msg: &Message) -> Result<()> {
    if matches!(msg.chat(), Chat::Channel(_) | Chat::Group(_)) {
        learn_dump(app, msg).await; // a dump-channel post: remember its access key
        return Ok(());
    }
    if !matches!(msg.chat(), Chat::User(_)) {
        return Ok(()); // everything else is ignored
    }
    let uid = msg.chat().id();
    if !app.cfg.admins.contains(&uid) {
        return Ok(()); // silently ignore everyone else
    }
    let text = msg.text().trim().to_string();

    if let Some(cmd) = command(&text) {
        return on_command(app, msg, uid, &cmd).await;
    }

    let existing = app.flows.lock().unwrap().remove(&uid);
    let flow = match existing {
        Some(flow) => flow,
        None => {
            // no command needed: a file means "rename this", links mean "rename these"
            let mode = if is_document(msg) {
                Mode::Single
            } else if !link::extract_links(&text).is_empty() {
                Mode::Auto
            } else {
                msg.reply("👋 Send me a dump-channel link (two for a batch) or a file.  /help")
                    .await?;
                return Ok(());
            };
            Flow::new(mode, prefs_of(app, uid))
        }
    };

    if let Some(flow) = advance(app, msg, uid, flow, &text).await? {
        app.flows.lock().unwrap().insert(uid, flow);
    }
    Ok(())
}

fn command(text: &str) -> Option<String> {
    let first = text.strip_prefix('/')?.split_whitespace().next()?;
    Some(first.split('@').next()?.to_ascii_lowercase())
}

fn is_document(msg: &Message) -> bool {
    matches!(msg.media(), Some(Media::Document(_)))
}

fn prefs_of(app: &App, uid: i64) -> Prefs {
    app.prefs
        .lock()
        .unwrap()
        .get(&uid)
        .copied()
        .unwrap_or_default()
}

fn save_prefs(app: &App, uid: i64, flow: &Flow) {
    app.prefs.lock().unwrap().insert(
        uid,
        Prefs {
            tags: flow.tags,
            season: flow.season,
            thumb_off: flow.thumb == ThumbSel::Off,
        },
    );
}

async fn on_command(app: &Arc<App>, msg: &Message, uid: i64, cmd: &str) -> Result<()> {
    match cmd {
        "start" | "help" => {
            msg.reply(HELP).await?;
        }
        "season" | "bulk" | "single" => {
            let mut flow = Flow::new(
                if cmd == "single" {
                    Mode::Single
                } else {
                    Mode::Batch
                },
                prefs_of(app, uid),
            );
            let prompt = match cmd {
                "season" => {
                    flow.tags.set(Tag::Season, true);
                    "📎 Send the FIRST and LAST message link."
                }
                "bulk" => {
                    flow.tags.set(Tag::Season, false);
                    "📎 Send the FIRST and LAST message link."
                }
                _ => "📎 Send the message link, or just forward the file.",
            };
            app.flows.lock().unwrap().insert(uid, flow);
            msg.reply(prompt).await?;
        }
        "go" => {
            let existing = app.flows.lock().unwrap().remove(&uid);
            match existing {
                Some(Flow {
                    step: Step::Confirm(plan),
                    ..
                }) => start_job(app, msg, uid, plan).await?,
                Some(flow) => {
                    app.flows.lock().unwrap().insert(uid, flow);
                    msg.reply("Nothing to start yet - finish the steps first (or /cancel).")
                        .await?;
                }
                None => {
                    msg.reply("Nothing to start. Send me links or a file.")
                        .await?;
                }
            }
        }
        "cancel" => {
            let had_flow = app.flows.lock().unwrap().remove(&uid).is_some();
            let removed = {
                let mut queue = app.queue.lock().unwrap();
                let before = queue.len();
                queue.retain(|j| j.uid != uid);
                before - queue.len()
            };
            let stopping = {
                let running = app.running.lock().unwrap();
                match running.as_ref() {
                    Some(r) if r.uid == uid => {
                        r.cancel.store(true, Ordering::Relaxed);
                        true
                    }
                    _ => false,
                }
            };
            let text = match (stopping, removed) {
                (true, 0) => "🛑 Stopping - the file in progress is abandoned.".to_string(),
                (true, n) => format!("🛑 Stopping the running job and removed {n} queued job(s)."),
                (false, n) if n > 0 => format!("🗑 Removed {n} queued job(s)."),
                (false, _) if had_flow => "Cancelled.".to_string(),
                _ => "Nothing to cancel.".to_string(),
            };
            msg.reply(text).await?;
        }
        "queue" => {
            msg.reply(queue_overview(app, uid)).await?;
        }
        _ => {
            msg.reply("Unknown command. /help").await?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------- conversation

/// Moves the conversation one step. `Ok(None)` = finished, `Ok(Some(flow))` = keep waiting.
/// Mistakes by the user are answered here and keep the flow alive; only real failures return Err.
async fn advance(
    app: &Arc<App>,
    msg: &Message,
    uid: i64,
    mut flow: Flow,
    text: &str,
) -> Result<Option<Flow>> {
    let step = std::mem::replace(&mut flow.step, Step::Links);

    match step {
        Step::Links => {
            if matches!(flow.mode, Mode::Single | Mode::Auto) && is_document(msg) {
                flow.mode = Mode::Single;
                flow.chat = Some(msg.chat().pack());
                flow.from = msg.id();
                flow.to = msg.id();
                return load_files(app, msg, flow, None).await;
            }

            let links = link::extract_links(text);
            let Some(first) = links.first() else {
                msg.reply(format!(
                    "I need a dump-channel message link, like https://t.me/c/{}/55",
                    app.cfg.dump_bare()
                ))
                .await?;
                return Ok(Some(flow)); // step stays Links
            };
            if let Err(why) = check_dump_links(app, &links) {
                msg.reply(why).await?;
                return Ok(Some(flow));
            }
            // proves access by posting + deleting a test message in the dump channel
            let chat = match ensure_dump(app).await {
                Ok(chat) => chat,
                Err(why) => {
                    remember_waiting(app, msg.chat().pack());
                    msg.reply(why).await?;
                    return Ok(Some(flow));
                }
            };
            flow.chat = Some(chat);
            flow.chat_ref = Some(first.chat.clone());
            flow.from = first.msg_id;
            flow.to = first.msg_id;

            if flow.mode == Mode::Single {
                return load_files(app, msg, flow, None).await;
            }
            if let Some(second) = links.get(1) {
                if Some(&second.chat) != flow.chat_ref.as_ref() {
                    msg.reply("Both links must be from the same chat. Send them again.")
                        .await?;
                    return Ok(Some(flow));
                }
                flow.to = second.msg_id;
                return after_range(app, msg, flow).await;
            }
            flow.step = Step::End;
            msg.reply(if flow.mode == Mode::Auto {
                "✅ Got it. Send the LAST link for a batch - or just the new name to rename this one file."
            } else {
                "✅ Got the first one. Now send the LAST message link."
            })
            .await?;
            Ok(Some(flow))
        }

        Step::End => {
            let links = link::extract_links(text);
            if let Some(last) = links.first() {
                if Some(&last.chat) != flow.chat_ref.as_ref() {
                    msg.reply(
                        "That link is from a different chat. Send the LAST link of the same chat.",
                    )
                    .await?;
                    flow.step = Step::End;
                    return Ok(Some(flow));
                }
                flow.to = last.msg_id;
                return after_range(app, msg, flow).await;
            }
            if flow.mode == Mode::Auto && !text.is_empty() {
                // not a link: it is the new name of the one file
                flow.mode = Mode::Single;
                return load_files(app, msg, flow, Some(text.to_string())).await;
            }
            msg.reply("Send the LAST message link.").await?;
            flow.step = Step::End;
            Ok(Some(flow))
        }

        Step::Name => {
            let name = text.trim();
            if name.is_empty() {
                msg.reply("Send the name as text (or tap the button above).")
                    .await?;
                flow.step = Step::Name;
                return Ok(Some(flow));
            }
            flow.name = name.to_string();
            flow.step = Step::Options;
            show(app, msg.chat().pack(), &flow).await;
            Ok(Some(flow))
        }

        Step::Options => {
            flow.step = Step::Options;
            if let Some(media) = msg.media() {
                // an image = the custom thumbnail for this job
                let status = msg.reply("🖼 Reading the image…").await?;
                let made = match tg::download_image(&app.client, &media).await {
                    Ok(raw) => thumb::from_bytes(&raw),
                    Err(e) => Err(e),
                };
                match made {
                    Ok(jpeg) => {
                        flow.custom = Some(jpeg);
                        flow.thumb = ThumbSel::Custom;
                        save_prefs(app, uid, &flow);
                        status
                            .edit(InputMessage::text(
                                "✅ Using your image as the thumbnail (this job only).",
                            ))
                            .await?;
                        show(app, msg.chat().pack(), &flow).await;
                    }
                    Err(e) => {
                        status
                            .edit(InputMessage::text(format!(
                                "❌ {}\nSend another image, or pick Logo / None with the buttons.",
                                clip(&format!("{e:#}"), 300)
                            )))
                            .await?;
                    }
                }
                return Ok(Some(flow));
            }
            if let Ok(n) = text.trim().parse::<u32>() {
                if flow.is_batch() && (1..=999).contains(&n) {
                    flow.season = n;
                    flow.tags.set(Tag::Season, true);
                    save_prefs(app, uid, &flow);
                    show(app, msg.chat().pack(), &flow).await;
                    return Ok(Some(flow));
                }
            }
            msg.reply("👆 Use the buttons above. (Send a number for the season, or an image for the thumbnail.)")
                .await?;
            Ok(Some(flow))
        }

        Step::Caption => {
            let caption = text.trim();
            if caption.is_empty() {
                msg.reply("Send the caption as text (or tap Back).").await?;
                flow.step = Step::Caption;
                return Ok(Some(flow));
            }
            flow.caption = Some(clip(caption, 1000));
            flow.step = Step::Options;
            show(app, msg.chat().pack(), &flow).await;
            Ok(Some(flow))
        }

        Step::Confirm(plan) => {
            msg.reply("Tap ▶️ Start on the preview (or send /go), or ❌ Cancel.")
                .await?;
            flow.step = Step::Confirm(plan);
            Ok(Some(flow))
        }
    }
}

async fn after_range(app: &Arc<App>, msg: &Message, mut flow: Flow) -> Result<Option<Flow>> {
    if flow.to < flow.from {
        std::mem::swap(&mut flow.from, &mut flow.to);
    }
    let span = (flow.to - flow.from) as usize + 1;
    if span > app.cfg.max_batch {
        msg.reply(format!(
            "That range spans {span} messages; the limit is {}. Send the links again.",
            app.cfg.max_batch
        ))
        .await?;
        flow.step = Step::Links;
        return Ok(Some(flow));
    }
    flow.mode = Mode::Batch;
    load_files(app, msg, flow, None).await
}

/// Reads the selected messages once. The same message then becomes the name prompt / the options card.
/// `preset` = a name that was already given (one link followed by a name).
async fn load_files(
    app: &Arc<App>,
    msg: &Message,
    mut flow: Flow,
    preset: Option<String>,
) -> Result<Option<Flow>> {
    let chat = flow.chat.ok_or_else(|| anyhow!("no source chat set"))?;
    let status = msg.reply("🔎 Reading the messages…").await?;
    let found = match tg::fetch_range(&app.client, chat, flow.from, flow.to).await {
        Ok(found) => found,
        Err(e) if is_channel_error(&e) && chat.id == app.cfg.dump_bare() => {
            // the saved key is not valid for this bot (token changed, bot removed ...): forget it and re-learn
            invalidate_dump(app);
            remember_waiting(app, msg.chat().pack());
            status.edit(InputMessage::text(LOST_DUMP)).await?;
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    if found.files.is_empty() {
        status
            .edit(InputMessage::text(if flow.mode == Mode::Single {
                "❌ That message has no file. Send another link or file."
            } else {
                "❌ No files found in that range. Send the links again."
            }))
            .await?;
        flow.step = Step::Links;
        return Ok(Some(flow));
    }
    flow.files = found.files;
    flow.skipped = found.skipped;
    flow.card = status.id();
    match preset {
        Some(name) => {
            flow.name = name;
            flow.step = Step::Options;
        }
        None => flow.step = Step::Name,
    }
    show(app, msg.chat().pack(), &flow).await;
    Ok(Some(flow))
}

// ------------------------------------------------------------------------------------- the card

fn btn(label: impl Into<String>, data: &str) -> button::Inline {
    button::inline(label, data.as_bytes().to_vec())
}

fn markup(rows: Vec<Vec<button::Inline>>) -> reply_markup::Inline {
    reply_markup::inline(rows)
}

/// Redraws the card in place. A failed edit (e.g. "message not modified") never ends the conversation.
async fn show(app: &App, chat: PackedChat, flow: &Flow) {
    let (text, keys) = view(flow);
    if let Err(e) = app
        .client
        .edit_message(
            chat,
            flow.card,
            InputMessage::text(text).reply_markup(&keys),
        )
        .await
    {
        let why = format!("{e}");
        if !why.contains("MESSAGE_NOT_MODIFIED") {
            log::warn!("could not update the card: {why}");
        }
    }
}

fn view(flow: &Flow) -> (String, reply_markup::Inline) {
    match &flow.step {
        Step::Name => name_view(flow),
        Step::Caption => caption_view(flow),
        Step::Confirm(plan) => (preview(plan), confirm_keys()),
        _ => options_view(flow),
    }
}

fn total_size(flow: &Flow) -> u64 {
    flow.files.iter().map(|f| f.size as u64).sum()
}

fn name_view(flow: &Flow) -> (String, reply_markup::Inline) {
    let mut text = String::new();
    if flow.is_batch() {
        text.push_str(&format!(
            "✅ {} files · {}\n\n📝 Send the name for them.\n",
            flow.files.len(),
            human(total_size(flow))
        ));
        text.push_str(
            "Counting starts at 1 - use {E:54} to start at 54, {N} for the original name.\n",
        );
        text.push_str("Example: {E:54} My Show");
    } else {
        let orig = flow
            .files
            .first()
            .map(|f| f.name.clone())
            .unwrap_or_default();
        text.push_str(&format!(
            "✅ {}\n\n📝 Send the new name - the extension is added for you.",
            clip(&orig, 120)
        ));
    }
    let mut row = vec![btn(
        if flow.is_batch() {
            "📄 Keep original names"
        } else {
            "📄 Keep original name"
        },
        "keep",
    )];
    if !flow.name.is_empty() {
        row.push(btn("↩️ Back", "back"));
    }
    (text, markup(vec![row, vec![btn("❌ Cancel", "x")]]))
}

fn caption_view(flow: &Flow) -> (String, reply_markup::Inline) {
    let mut text = String::from(
        "💬 Send the caption for every file.\nYou can use {E} {S} {N} (original name) and {NAME} (new name).",
    );
    if let Some(c) = &flow.caption {
        text.push_str(&format!("\n\nNow: {}", clip(c, 200)));
    }
    let mut row = vec![btn("↩️ Back", "back")];
    if flow.caption.is_some() {
        row.push(btn("🗑 Remove caption", "capx"));
    }
    (text, markup(vec![row]))
}

fn confirm_keys() -> reply_markup::Inline {
    markup(vec![vec![
        btn("▶️ Start", "go"),
        btn("◀️ Back", "back"),
        btn("❌ Cancel", "x"),
    ]])
}

fn options_view(flow: &Flow) -> (String, reply_markup::Inline) {
    let batch = flow.is_batch();
    let mut text = if batch {
        format!(
            "🎛 {} files · {}\n",
            flow.files.len(),
            human(total_size(flow))
        )
    } else {
        let orig = flow
            .files
            .first()
            .map(|f| f.name.clone())
            .unwrap_or_default();
        format!("📄 {}\n", clip(&orig, 120))
    };
    if let Some(f) = flow.files.first() {
        text.push_str(&format!("\n👁 {}\n", make_name(flow, 0, f).0));
    }
    if let Some(c) = &flow.caption {
        text.push_str(&format!("💬 {}\n", clip(c, 80)));
    }
    text.push_str(if batch {
        "\nTap to switch things on/off. Send a number = season, an image = thumbnail."
    } else {
        "\nTap to switch things on/off. Send an image = thumbnail."
    });

    let tag_btn = |t: Tag| {
        btn(
            format!(
                "{} {}",
                if flow.tags.has(t) { "✅" } else { "⬜" },
                t.label()
            ),
            &format!("t:{}", t.code()),
        )
    };
    let thumb_btn = |sel: ThumbSel, icon: &str, label: &str, code: &str| {
        btn(
            format!("{} {}", if flow.thumb == sel { "✅" } else { icon }, label),
            code,
        )
    };

    let mut rows: Vec<Vec<button::Inline>> = Vec::new();
    if batch {
        rows.push(vec![tag_btn(Tag::Episode), tag_btn(Tag::Season)]);
    }
    rows.push(vec![
        tag_btn(Tag::Quality),
        tag_btn(Tag::Audio),
        tag_btn(Tag::Codec),
    ]);
    rows.push(vec![
        tag_btn(Tag::Source),
        tag_btn(Tag::Subs),
        tag_btn(Tag::Size),
    ]);
    if batch && flow.tags.has(Tag::Season) {
        rows.push(vec![
            btn("➖", "s:-"),
            btn(format!("Season {}", flow.season), "noop"),
            btn("➕", "s:+"),
        ]);
    }
    rows.push(vec![
        thumb_btn(ThumbSel::Logo, "🖼", "Logo", "th:logo"),
        thumb_btn(ThumbSel::Custom, "📷", "My image", "th:custom"),
        thumb_btn(ThumbSel::Off, "🚫", "None", "th:off"),
    ]);
    rows.push(vec![
        btn("✏️ Name", "name"),
        btn(
            if flow.caption.is_some() {
                "💬 Caption ✓"
            } else {
                "💬 Caption"
            },
            "cap",
        ),
    ]);
    rows.push(vec![
        btn(
            if batch {
                "▶️ Continue"
            } else {
                "▶️ Start"
            },
            "go",
        ),
        btn("❌ Cancel", "x"),
    ]);
    (text, markup(rows))
}

// ------------------------------------------------------------------------------------- the buttons

enum Next {
    Nothing,
    Toast(String),
    Redraw,
    Preview,
    Go,
    Cancel,
}

/// Quick changes that need no network. Runs under the flows lock, so it must not wait for anything.
fn apply(app: &App, uid: i64, flow: &mut Flow, data: &str) -> Next {
    let kind = flow.kind();
    let batch = flow.is_batch();
    match data {
        "x" => Next::Cancel,
        "noop" => Next::Nothing,
        "keep" if kind == K::Name => {
            flow.name = "{N}".to_string();
            flow.step = Step::Options;
            Next::Redraw
        }
        "name" if kind == K::Options => {
            flow.step = Step::Name;
            Next::Redraw
        }
        "cap" if kind == K::Options => {
            flow.step = Step::Caption;
            Next::Redraw
        }
        "capx" if kind == K::Caption => {
            flow.caption = None;
            flow.step = Step::Options;
            Next::Redraw
        }
        "back" if matches!(kind, K::Name | K::Caption | K::Confirm) => {
            flow.step = Step::Options;
            Next::Redraw
        }
        "go" if kind == K::Options => {
            if batch {
                Next::Preview
            } else {
                Next::Go
            }
        }
        "go" if kind == K::Confirm => Next::Go,
        "s:-" | "s:+" if kind == K::Options && batch => {
            flow.season = if data == "s:-" {
                flow.season.saturating_sub(1).max(1)
            } else {
                (flow.season + 1).min(999)
            };
            save_prefs(app, uid, flow);
            Next::Redraw
        }
        "th:logo" if kind == K::Options => {
            flow.thumb = ThumbSel::Logo;
            save_prefs(app, uid, flow);
            Next::Redraw
        }
        "th:off" if kind == K::Options => {
            flow.thumb = ThumbSel::Off;
            save_prefs(app, uid, flow);
            Next::Redraw
        }
        "th:custom" if kind == K::Options => {
            if flow.custom.is_some() {
                flow.thumb = ThumbSel::Custom;
                save_prefs(app, uid, flow);
                Next::Redraw
            } else {
                Next::Toast("Send me the image now (as a photo or a file).".to_string())
            }
        }
        t if kind == K::Options && t.starts_with("t:") => match Tag::from_code(&t[2..]) {
            Some(tag) if batch || !matches!(tag, Tag::Episode | Tag::Season) => {
                flow.tags.toggle(tag);
                save_prefs(app, uid, flow);
                Next::Redraw
            }
            _ => Next::Toast("That one is for batches only.".to_string()),
        },
        _ => Next::Toast("That button does not work any more.".to_string()),
    }
}

/// A button was tapped. `card` is the message that carries it. Returns the small pop-up text to show, if any.
pub async fn on_button(app: &Arc<App>, uid: i64, data: &str, card: Message) -> Option<String> {
    if !app.cfg.admins.contains(&uid) {
        return None;
    }
    let (next, shown) = {
        let mut flows = app.flows.lock().unwrap();
        let Some(flow) = flows.get_mut(&uid) else {
            return Some("This one is old - send the links again.".to_string());
        };
        if flow.card != card.id() {
            return Some("That message is old - use the newest one.".to_string());
        }
        let next = apply(app, uid, flow, data);
        let shown = matches!(next, Next::Redraw).then(|| view(flow));
        (next, shown)
    };

    match next {
        Next::Nothing => None,
        Next::Toast(t) => Some(t),
        Next::Redraw => {
            if let Some((text, keys)) = shown {
                let _ = card
                    .edit(InputMessage::text(text).reply_markup(&keys))
                    .await;
            }
            None
        }
        Next::Cancel => {
            app.flows.lock().unwrap().remove(&uid);
            let _ = card.edit(InputMessage::text("❌ Cancelled.")).await;
            None
        }
        Next::Preview => {
            let now = {
                let mut flows = app.flows.lock().unwrap();
                flows.get_mut(&uid).map(|flow| {
                    let plan = build_plan(flow);
                    flow.step = Step::Confirm(plan);
                    view(flow)
                })
            };
            if let Some((text, keys)) = now {
                let _ = card
                    .edit(InputMessage::text(text).reply_markup(&keys))
                    .await;
            }
            None
        }
        Next::Go => {
            let taken = app.flows.lock().unwrap().remove(&uid);
            let flow = taken?;
            let plan = if let Step::Confirm(p) = &flow.step {
                p.clone()
            } else {
                build_plan(&flow)
            };
            let _ = card.edit(InputMessage::text(preview(&plan))).await; // removes the buttons
            if let Err(e) = start_job(app, &card, uid, plan).await {
                let _ = card
                    .reply(format!("❌ {}", clip(&format!("{e}"), 300)))
                    .await;
            }
            None
        }
    }
}

// ------------------------------------------------------------------------------------- the plan

/// New name of file `i` and the selected tags that this file's name does not contain.
fn make_name(flow: &Flow, i: usize, f: &FileInfo) -> (String, Vec<Tag>) {
    let stem = naming::stem_of(&f.name);
    let found = tags::detect(&f.name);
    let (extras, missing) = tags::extras(flow.tags, &found, f.size);
    let name = if flow.is_batch() {
        let base = naming::parse_template(&flow.name).render(&naming::Parts {
            index: i,
            season: flow.tags.has(Tag::Season).then_some(flow.season),
            episode: flow.tags.has(Tag::Episode),
            extras: &extras,
            stem: &stem,
        });
        naming::with_extension(&base, &f.name, f.mime.as_deref(), ExtMode::Always)
    } else {
        let typed = naming::fill_original(&flow.name, &stem);
        let base = naming::join_extras(&typed, &extras);
        naming::with_extension(&base, &f.name, f.mime.as_deref(), ExtMode::IfMissing)
    };
    (name, missing)
}

fn make_caption(flow: &Flow, i: usize, f: &FileInfo, new_name: &str) -> Option<String> {
    let raw = flow.caption.as_ref()?;
    let text = naming::parse_template(raw).render_free(
        i,
        flow.tags.has(Tag::Season).then_some(flow.season),
        &naming::stem_of(&f.name),
        new_name,
    );
    if text.is_empty() {
        None
    } else {
        Some(clip(&text, 1000))
    }
}

fn build_plan(flow: &Flow) -> Plan {
    let mut missing: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut items = Vec::with_capacity(flow.files.len());
    for (i, f) in flow.files.iter().enumerate() {
        let (new_name, gone) = make_name(flow, i, f);
        for tag in gone {
            *missing.entry(tag.label()).or_insert(0) += 1;
        }
        let caption = make_caption(flow, i, f, &new_name);
        items.push(PlanItem {
            msg_id: f.msg_id,
            new_name,
            size: f.size,
            caption,
        });
    }
    let total = items.len();
    let warnings = missing
        .into_iter()
        .map(|(label, n)| {
            format!("⚠️ {label} not found in {n} of {total} name(s) - left out there.")
        })
        .collect();
    Plan {
        chat: flow.chat.expect("source chat is set once files are loaded"),
        items,
        skipped: flow.skipped,
        thumb: match flow.thumb {
            ThumbSel::Logo => Thumb::Logo,
            ThumbSel::Off => Thumb::Off,
            ThumbSel::Custom => match &flow.custom {
                Some(bytes) => Thumb::Custom(bytes.clone()),
                None => Thumb::Logo,
            },
        },
        warnings,
    }
}

fn preview(plan: &Plan) -> String {
    let total: usize = plan.items.iter().map(|i| i.size).sum();
    let mut out = format!("📋 {} files · {}", plan.items.len(), human(total as u64));
    if plan.skipped > 0 {
        out.push_str(&format!(
            " · {} messages skipped (deleted / not files)",
            plan.skipped
        ));
    }
    out.push_str("\n\n");

    let n = plan.items.len();
    let shown: Vec<usize> = if n <= 5 {
        (0..n).collect()
    } else {
        vec![0, 1, 2, n - 1]
    };
    for (pos, &i) in shown.iter().enumerate() {
        if pos == 3 && n > 5 {
            out.push_str("…\n");
        }
        out.push_str(&format!("{}. {}\n", i + 1, plan.items[i].new_name));
    }

    let too_big = plan
        .items
        .iter()
        .filter(|i| i.size > tg::MAX_UPLOAD)
        .count();
    let empty = plan.items.iter().filter(|i| i.size == 0).count();
    if too_big > 0 {
        out.push_str(&format!("\n⚠️ {too_big} file(s) are over 2 GB - bots cannot upload those, they will be reported as failed."));
    }
    if empty > 0 {
        out.push_str(&format!(
            "\n⚠️ {empty} file(s) are empty (0 bytes) and will be reported as failed."
        ));
    }
    for w in &plan.warnings {
        out.push('\n');
        out.push_str(w);
    }
    out.push_str(match plan.thumb {
        Thumb::Custom(_) => "\n🖼 Thumbnail: your image",
        Thumb::Logo => "\n🖼 Thumbnail: default logo",
        Thumb::Off => "\n🖼 Thumbnail: none",
    });
    if plan.items.iter().any(|i| i.caption.is_some()) {
        out.push_str("\n💬 Caption: on");
    }
    out
}

// ---------------------------------------------------------------------------------------------- jobs

fn queue_overview(app: &App, uid: i64) -> String {
    let mut out = String::new();
    match app.running.lock().unwrap().as_ref() {
        Some(r) => out.push_str(&format!(
            "▶️ Running: {} file(s){}\n",
            r.files,
            if r.uid == uid { " (yours)" } else { "" }
        )),
        None => out.push_str("Nothing is running.\n"),
    }
    let queue = app.queue.lock().unwrap();
    if queue.is_empty() {
        out.push_str("The queue is empty.");
    } else {
        out.push_str("📥 Waiting:\n");
        for (pos, job) in queue.iter().enumerate() {
            out.push_str(&format!(
                "{}. {} file(s){}\n",
                pos + 1,
                job.plan.items.len(),
                if job.uid == uid { " (yours)" } else { "" }
            ));
        }
    }
    out
}

/// Puts a job at the end of the queue. The worker (see `spawn_worker`) runs the jobs one at a time.
async fn start_job(app: &Arc<App>, msg: &Message, uid: i64, mut plan: Plan) -> Result<()> {
    if plan.chat.id == app.cfg.dump_bare() {
        // the key must be proven again right before queueing (and is refreshed if it changed)
        match ensure_dump(app).await {
            Ok(chat) => plan.chat = chat,
            Err(why) => {
                remember_waiting(app, msg.chat().pack());
                msg.reply(why).await?;
                return Ok(());
            }
        }
    }

    let files = plan.items.len();
    let running = app.running.lock().unwrap().is_some();
    let position = {
        let mut queue = app.queue.lock().unwrap();
        if queue.iter().filter(|j| j.uid == uid).count() >= MAX_QUEUED {
            None
        } else {
            queue.push_back(QueuedJob {
                uid,
                dest: msg.chat().pack(),
                plan,
                cancel: Arc::new(AtomicBool::new(false)),
            });
            Some(queue.len() - 1 + usize::from(running))
        }
    };
    let Some(ahead) = position else {
        msg.reply(format!(
            "Too many jobs waiting ({MAX_QUEUED}). Wait for some to finish or /cancel."
        ))
        .await?;
        return Ok(());
    };
    app.wake.notify_one();
    msg.reply(if ahead == 0 {
        format!("▶️ Starting now: {files} file(s), renamed one by one.")
    } else {
        format!("📥 Queued: {files} file(s). {ahead} job(s) ahead of it. /queue shows the line, /cancel removes it.")
    })
    .await?;
    Ok(())
}

/// Takes jobs from the queue one at a time, forever.
pub fn spawn_worker(app: Arc<App>) {
    tokio::spawn(async move {
        loop {
            let next = app.queue.lock().unwrap().pop_front();
            let Some(job) = next else {
                app.wake.notified().await; // notify_one keeps a permit, so a wake-up is never missed
                continue;
            };
            *app.running.lock().unwrap() = Some(Running {
                uid: job.uid,
                cancel: job.cancel.clone(),
                files: job.plan.items.len(),
            });

            let job = Arc::new(job);
            let handle = tokio::spawn({
                let app = app.clone();
                let job = job.clone();
                async move { run_job(&app, &job).await }
            });
            let problem = match handle.await {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(format!("{e:#}")),
                Err(join) => Some(format!("internal error: {join}")),
            };
            *app.running.lock().unwrap() = None;
            if let Some(problem) = problem {
                log::error!("job failed: {problem}");
                let _ = app
                    .client
                    .send_message(job.dest, format!("❌ Job stopped: {}", clip(&problem, 500)))
                    .await;
            }
        }
    });
}

/// The file that is being transferred right now (for the status message).
struct Active {
    index: usize,
    name: String,
    size: usize,
    progress: Arc<AtomicU64>,
}

/// State the job shares with the status ticker and the final report.
struct Shared {
    done: Mutex<usize>,
    finished_bytes: AtomicU64,
    failed: Mutex<Vec<String>>,
    notes: Mutex<Vec<String>>,
    active: Mutex<Option<Active>>,
}

/// Renames the files of one job strictly one after another, in message order.
async fn run_job(app: &Arc<App>, job: &QueuedJob) -> Result<()> {
    let client = &app.client;
    let dest = job.dest;
    let plan = &job.plan;
    let cancel = &job.cancel;
    let total = plan.items.len();

    // Logo = re-read for every job, so replacing the PNG needs no restart. Custom = this job only. Off = none.
    let thumbnail: Option<Vec<u8>> = match &plan.thumb {
        Thumb::Custom(bytes) => Some(bytes.clone()),
        Thumb::Off => None,
        Thumb::Logo => match thumb::load(&app.cfg.logo) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                let _ = client
                    .send_message(
                        dest,
                        format!(
                            "⚠️ Sending without a thumbnail: {}",
                            clip(&format!("{e:#}"), 300)
                        ),
                    )
                    .await;
                None
            }
        },
    };

    let status = client
        .send_message(dest, format!("⏳ Starting… 0/{total}"))
        .await?;
    let started = Instant::now();
    let shared = Arc::new(Shared {
        done: Mutex::new(0),
        finished_bytes: AtomicU64::new(0),
        failed: Mutex::new(Vec::new()),
        notes: Mutex::new(Vec::new()),
        active: Mutex::new(None),
    });
    let ticker = spawn_ticker(status.clone(), shared.clone(), total, started);

    for (i, item) in plan.items.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let progress = Arc::new(AtomicU64::new(0));
        *shared.active.lock().unwrap() = Some(Active {
            index: i,
            name: item.new_name.clone(),
            size: item.size,
            progress: progress.clone(),
        });

        let result = send_one(
            client,
            dest,
            plan.chat,
            item,
            thumbnail.as_deref(),
            &progress,
            cancel,
        )
        .await;

        *shared.active.lock().unwrap() = None;
        match result {
            Ok(note) => {
                *shared.done.lock().unwrap() += 1;
                shared
                    .finished_bytes
                    .fetch_add(item.size as u64, Ordering::Relaxed);
                if let Some(note) = note {
                    shared
                        .notes
                        .lock()
                        .unwrap()
                        .push(format!("{} — {note}", item.new_name));
                }
            }
            Err(_) if cancel.load(Ordering::Relaxed) => {}
            Err(e) => {
                log::warn!("{}: {e:#}", item.new_name);
                shared.failed.lock().unwrap().push(format!(
                    "{} — {}",
                    item.new_name,
                    clip(&format!("{e}"), 200)
                ));
            }
        }
    }
    ticker.abort();

    let stopped = cancel.load(Ordering::Relaxed);
    let done = *shared.done.lock().unwrap();
    let failed = shared.failed.lock().unwrap().clone();
    let notes = shared.notes.lock().unwrap().clone();
    let secs = started.elapsed().as_secs_f64().max(1.0);
    let moved = shared.finished_bytes.load(Ordering::Relaxed);

    let mut report = format!(
        "{} {done}/{total} renamed · {} · {}/s",
        if stopped {
            "🛑 Stopped:"
        } else {
            "✅ Done:"
        },
        clock(secs as u64),
        human((moved as f64 / secs) as u64)
    );
    if !notes.is_empty() {
        report.push_str(&format!(
            "\n\nℹ️ {} converted (not a plain rename):\n",
            notes.len()
        ));
        for line in notes.iter().take(10) {
            report.push_str(&format!("• {}\n", clip(line, 200)));
        }
    }
    if !failed.is_empty() {
        report.push_str(&format!("\n\n❌ {} failed:\n", failed.len()));
        for line in failed.iter().take(10) {
            report.push_str(&format!("• {line}\n"));
        }
    }
    let report = clip(&report, 3800);
    let _ = status.edit(InputMessage::text(&report)).await;
    client.send_message(dest, report).await?; // a fresh message so the user gets a notification
    Ok(())
}

/// One file, with retries. Errors a retry cannot fix (file gone, too big, access lost ...) end it at once.
async fn send_one(
    client: &Client,
    dest: PackedChat,
    source: PackedChat,
    item: &PlanItem,
    thumbnail: Option<&[u8]>,
    progress: &Arc<AtomicU64>,
    cancel: &Arc<AtomicBool>,
) -> Result<Option<String>> {
    let mut last = anyhow!("unknown error");
    for attempt in 0..3u32 {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        progress.store(0, Ordering::Relaxed);
        match try_once(client, dest, source, item, thumbnail, progress, cancel).await {
            Ok(note) => return Ok(note),
            Err(e) => {
                log::warn!(
                    "attempt {} for {} failed: {e:#}",
                    attempt + 1,
                    item.new_name
                );
                let permanent = is_permanent(&e);
                last = e;
                if permanent {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(2u64.pow(attempt + 1))).await;
            }
        }
    }
    Err(last)
}

async fn try_once(
    client: &Client,
    dest: PackedChat,
    source: PackedChat,
    item: &PlanItem,
    thumbnail: Option<&[u8]>,
    progress: &Arc<AtomicU64>,
    cancel: &Arc<AtomicBool>,
) -> Result<Option<String>> {
    // fetch the message again right before use: file references go stale while earlier files are in progress
    let fetched = client.get_messages_by_id(source, &[item.msg_id]).await?;
    let message = fetched
        .into_iter()
        .next()
        .flatten()
        .ok_or_else(|| anyhow!("message {} is gone", item.msg_id))?;
    let media = message
        .media()
        .ok_or_else(|| anyhow!("message {} has no file", item.msg_id))?;

    tg::send_renamed(
        client,
        dest,
        &media,
        &item.new_name,
        item.caption.as_deref(),
        thumbnail,
        progress,
        cancel,
    )
    .await
}

fn spawn_ticker(
    status: Message,
    shared: Arc<Shared>,
    total: usize,
    started: Instant,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(6)).await;
            let text = render_status(&shared, total, started);
            let _ = status.edit(InputMessage::text(&text)).await;
        }
    })
}

fn render_status(shared: &Shared, total: usize, started: Instant) -> String {
    let done = *shared.done.lock().unwrap();
    let failed = shared.failed.lock().unwrap().len();
    let active = shared.active.lock().unwrap().as_ref().map(|a| {
        (
            a.index,
            a.name.clone(),
            a.size,
            a.progress.load(Ordering::Relaxed),
        )
    });

    let moved = shared.finished_bytes.load(Ordering::Relaxed) + active.as_ref().map_or(0, |a| a.3);
    let secs = started.elapsed().as_secs_f64().max(1.0);

    let mut out = format!("⏳ {done}/{total} done");
    if failed > 0 {
        out.push_str(&format!(" · {failed} failed"));
    }
    out.push_str(&format!(
        " · {}/s · {}",
        human((moved as f64 / secs) as u64),
        clock(secs as u64)
    ));
    if let Some((index, name, size, got)) = active {
        let pct = if size > 0 {
            (got as f64 / size as f64 * 100.0).min(100.0)
        } else {
            0.0
        };
        out.push_str(&format!(
            "\n\n#{} {}\n{pct:.0}% · {} / {}",
            index + 1,
            name,
            human(got),
            human(size as u64)
        ));
    }
    out
}

fn clock(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, secs % 3600 / 60)
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}
