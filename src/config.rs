use std::{collections::HashSet, env, path::PathBuf};

use anyhow::{bail, Context, Result};

pub struct Config {
    pub api_id: i32,
    pub api_hash: String,
    pub bot_token: String,
    /// The number in front of the colon in the bot token. Everything that is bound to one bot (the saved login,
    /// the dump channel's access key) is tagged with it, so a new BOT_TOKEN never reuses the old bot's leftovers.
    pub bot_id: i64,
    pub admins: HashSet<i64>,
    pub logo: PathBuf,
    pub session: PathBuf,
    /// The dump channel's id in its short form (the one in t.me/c/<id>/<msg> links).
    dump_id: i64,
    /// Where the bot remembers the dump channel's access hash between restarts.
    pub dump_cache: PathBuf,
    /// Largest message-id span one /season, /bulk run may cover.
    pub max_batch: usize,
}

fn required(key: &str) -> Result<String> {
    match env::var(key) {
        Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => bail!(
            "missing environment variable {key}; set it in the host/container environment, .env or .env.local"
        ),
    }
}

/// Any spelling of a channel id -> the short id used in message links:
/// -1001234567890, 1001234567890, -1234567890 and 1234567890 all give 1234567890.
pub fn bare_channel_id(raw: i64) -> i64 {
    let n = raw.checked_abs().unwrap_or(i64::MAX);
    if n >= 1_000_000_000_000 {
        n - 1_000_000_000_000
    } else {
        n
    }
}

/// "123456:ABC-DEF..." -> 123456
pub fn bot_id_from_token(token: &str) -> Option<i64> {
    let (id, secret) = token.trim().split_once(':')?;
    if secret.is_empty() {
        return None;
    }
    id.parse::<i64>().ok().filter(|v| *v > 0)
}

impl Config {
    pub fn dump_bare(&self) -> i64 {
        self.dump_id
    }

    pub fn from_env() -> Result<Self> {
        // Process environment > .env.local > .env  (dotenvy never overwrites a variable that already exists).
        // .env.example is deliberately NOT read: it is a public template and must never supply real values.
        let _ = dotenvy::from_filename(".env.local");
        let _ = dotenvy::from_filename(".env");

        let mut admins: HashSet<i64> = HashSet::new();
        for part in required("ADMIN_IDS")?.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            match part.parse::<i64>() {
                Ok(id) if id > 0 => {
                    admins.insert(id);
                }
                _ => log::warn!("ignoring invalid ADMIN_IDS entry {part:?}"),
            }
        }
        if admins.is_empty() {
            bail!("ADMIN_IDS has no valid user id - the bot never runs open to everyone");
        }

        let bot_token = required("BOT_TOKEN")?;
        let Some(bot_id) = bot_id_from_token(&bot_token) else {
            bail!("BOT_TOKEN does not look right - it should be like 123456789:AAF... (copy it again from @BotFather)");
        };

        let dump_raw: i64 = required("DUMP_CHANNEL_ID")?
            .parse()
            .context("DUMP_CHANNEL_ID must be a number like -1001234567890")?;
        let dump_id = bare_channel_id(dump_raw);
        if dump_id == 0 {
            bail!("DUMP_CHANNEL_ID must be a number like -1001234567890");
        }

        Ok(Self {
            api_id: required("API_ID")?
                .parse()
                .context("API_ID must be a number")?,
            api_hash: required("API_HASH")?,
            bot_token,
            bot_id,
            admins,
            logo: env::var("LOGO_PATH")
                .unwrap_or_else(|_| "assets/logo.png".into())
                .into(),
            session: env::var("SESSION_FILE")
                .unwrap_or_else(|_| "renamebot.session".into())
                .into(),
            dump_id,
            dump_cache: env::var("DUMP_CACHE")
                .unwrap_or_else(|_| "dump.chat".into())
                .into(),
            max_batch: env::var("MAX_BATCH")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(500)
                .max(1),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_id_spellings() {
        assert_eq!(bare_channel_id(-1001234567890), 1234567890);
        assert_eq!(bare_channel_id(1001234567890), 1234567890);
        assert_eq!(bare_channel_id(-1234567890), 1234567890);
        assert_eq!(bare_channel_id(1234567890), 1234567890);
    }

    #[test]
    fn token_to_bot_id() {
        assert_eq!(bot_id_from_token("123456789:AAFabc"), Some(123456789));
        assert_eq!(bot_id_from_token(" 42:x "), Some(42));
        assert_eq!(bot_id_from_token("nonsense"), None);
        assert_eq!(bot_id_from_token("abc:def"), None);
        assert_eq!(bot_id_from_token("123:"), None);
    }
}
