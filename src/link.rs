//! Telegram message links -> (chat, message id). Pure string handling, no network.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatRef {
    /// https://t.me/c/<id>/<msg> - private channel or supergroup (id without the -100 prefix)
    Private(i64),
    /// https://t.me/<username>/<msg>
    Username(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsgLink {
    pub chat: ChatRef,
    pub msg_id: i32,
}

/// Every message link found in free text, in order.
pub fn extract_links(text: &str) -> Vec<MsgLink> {
    text.split_whitespace().filter_map(parse).collect()
}

pub fn parse(raw: &str) -> Option<MsgLink> {
    let s = raw
        .trim()
        .trim_matches(|c: char| matches!(c, '<' | '>' | ',' | '(' | ')'));
    if let Some(rest) = s.strip_prefix("tg://") {
        return parse_tg(rest);
    }
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let (host, path) = s.split_once('/')?;
    if !matches!(
        host.to_ascii_lowercase().as_str(),
        "t.me" | "www.t.me" | "telegram.me" | "telegram.dog"
    ) {
        return None;
    }
    let path = path.split(|c| c == '?' || c == '#').next()?;
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();

    match parts.as_slice() {
        // .../c/<channel>/<msg>   or   .../c/<channel>/<topic>/<msg>
        ["c", id, rest @ ..] if !rest.is_empty() => {
            let chat: i64 = id.parse().ok().filter(|v| *v > 0)?;
            let msg_id: i32 = rest.last()?.parse().ok().filter(|v| *v > 0)?;
            Some(MsgLink {
                chat: ChatRef::Private(chat),
                msg_id,
            })
        }
        [user, rest @ ..] if !rest.is_empty() && *user != "c" && valid_username(user) => {
            let msg_id: i32 = rest.last()?.parse().ok().filter(|v| *v > 0)?;
            Some(MsgLink {
                chat: ChatRef::Username(user.to_string()),
                msg_id,
            })
        }
        _ => None,
    }
}

fn parse_tg(rest: &str) -> Option<MsgLink> {
    let (kind, query) = rest.split_once('?')?;
    let get = |key: &str| {
        query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    };
    let msg_id: i32 = get("post")?.parse().ok().filter(|v| *v > 0)?;
    match kind {
        "privatepost" => {
            let chat: i64 = get("channel")?.parse().ok().filter(|v| *v > 0)?;
            Some(MsgLink {
                chat: ChatRef::Private(chat),
                msg_id,
            })
        }
        "resolve" => {
            let user = get("domain")?;
            valid_username(user).then(|| MsgLink {
                chat: ChatRef::Username(user.to_string()),
                msg_id,
            })
        }
        _ => None,
    }
}

fn valid_username(u: &str) -> bool {
    (3..=32).contains(&u.len()) && u.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private(id: i64, msg: i32) -> Option<MsgLink> {
        Some(MsgLink {
            chat: ChatRef::Private(id),
            msg_id: msg,
        })
    }

    #[test]
    fn private_channel_links() {
        assert_eq!(
            parse("https://t.me/c/1234567890/55"),
            private(1234567890, 55)
        );
        assert_eq!(parse("t.me/c/1234567890/55"), private(1234567890, 55));
        assert_eq!(parse("https://t.me/c/123/9/55"), private(123, 55)); // topic id in the middle
        assert_eq!(parse("https://t.me/c/123/55?single"), private(123, 55));
    }

    #[test]
    fn public_links() {
        assert_eq!(
            parse("https://t.me/some_channel/12"),
            Some(MsgLink {
                chat: ChatRef::Username("some_channel".into()),
                msg_id: 12
            })
        );
    }

    #[test]
    fn tg_scheme() {
        assert_eq!(
            parse("tg://privatepost?channel=123&post=5"),
            private(123, 5)
        );
        assert_eq!(
            parse("tg://resolve?domain=abcde&post=7"),
            Some(MsgLink {
                chat: ChatRef::Username("abcde".into()),
                msg_id: 7
            })
        );
    }

    #[test]
    fn rejects_junk() {
        assert_eq!(parse("https://example.com/c/1/2"), None);
        assert_eq!(parse("https://t.me/c/abc/2"), None);
        assert_eq!(parse("https://t.me/somechannel"), None);
        assert_eq!(parse("hello"), None);
    }

    #[test]
    fn finds_two_links_in_one_message() {
        let v = extract_links("from https://t.me/c/1/10 to https://t.me/c/1/20");
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].msg_id, 20);
    }
}
