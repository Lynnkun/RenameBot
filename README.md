# renamebot

Renames Telegram files (mkv, mp4, any format) **without touching the video** and gives them your logo as thumbnail.
Single files, or a whole range of channel messages in one go with an episode counter.

## Using it (nothing to learn)

Send the bot:

* **two dump-channel links** (first + last) → a batch
* **one link, or a file** → a single file (send a name right after the link to skip a step)

Then send the name and use the buttons on the card. `/season`, `/bulk` and `/single` still exist as shortcuts;
they only change which buttons start switched on. `/queue`, `/cancel`, `/help` as before.

**Tags** (buttons, for every batch and single): `Episode` `Season` `Quality` `Audio` `Codec` `Source` `Subs` `Size`
→ `[E-1][S-2][1080p][Hindi][HEVC][WEB-DL][ESub][1.46GB] My Show.mkv`.
Quality, audio, codec, source and subs are read from the *original file name* (Telegram gives no stream info without
downloading); a tag that is not in the name is left out for that file and the preview says how many. Episode and
Season are counters (single files have no counters). Your last choice is remembered for the next job (until restart).

**Season**: switching it on shows `➖ Season n ➕`; or just send a number while the card is open.

**Name tokens** (old logic unchanged): `{E}` counts from 1, `{E:54}` from 54 (ten files end at `[E-63]`), `{S}` places
the season tag yourself, `{N}` = the original name. A button *Keep original names* = `{N}`.
No `{E}` and Episode on? A counter is added in front so files never share a name. Typing `{E}` yourself always counts.
The extension always comes from the original file; `/single` only adds it if you left it out.

**Thumbnail** (per job): `Logo` (default `assets/logo.png`) · `My image` (send a photo or image file while the card is
open) · `None`. Any image is converted to what Telegram accepts (JPEG, ≤320 px, ≤200 KB).

**Caption** (optional, per job): tap *Caption* and send text. `{E}` `{S}` `{N}` (original name) and `{NAME}` (new name)
work in it. No caption = none, as before.

## How a rename works (and its limits)

Telegram has no rename call: the file name is part of the upload. The bot therefore **streams each file from the
source message straight into a new upload** with the new name (no copy on disk). That means:

* every file is downloaded **and** uploaded again: time and data use are 2× the file size, one file at a time, in order;
* limit is 2 GB per file (bot accounts);
* files arrive as documents with your thumbnail; the video itself is untouched, but the in-chat inline player /
  duration badge of the original is not copied;
* the bot must be a **member of the source channel** (like FileBot's dump channel), or the file must be sent/forwarded to the bot.

## Setup (Termux)

```bash
pkg install rust
cp .env.example .env.local        # fill API_ID, API_HASH (my.telegram.org), BOT_TOKEN, ADMIN_IDS
mkdir -p assets && cp /path/to/logo.png assets/logo.png
cargo test                        # checks the naming, tag and link logic
cargo run --release
```

The logo is re-read for every job, so replacing `assets/logo.png` needs no restart.
Only the ids in `ADMIN_IDS` can use the bot; everyone else is ignored.

## If it does not compile

`grammers` changes its API between releases and this project pins `0.7.0`. All calls into it are in `src/tg.rs`
and the `try_once` / `main` functions; the parts most worth checking first are `InputMessage::thumbnail`,
`upload_stream`, `iter_download`, `next_update` (Option or not) and the `PackedChat` fields. The buttons use
`button::inline`, `reply_markup::inline`, `InputMessage::reply_markup`, `Client::edit_message` and the
`CallbackQuery` methods (`sender`, `data`, `load_message`, `answer`) - all in `src/bot.rs` / `main.rs`.