mod bot;
mod config;
mod kinds;
mod link;
mod naming;
mod tags;
mod tg;
mod thumb;

use std::{path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use grammers_client::{Client, Config as TgConfig, Update};
use grammers_session::Session;

async fn connect(cfg: &config::Config, fresh: bool) -> Result<Client> {
    let session = if fresh {
        Session::new()
    } else {
        Session::load_file_or_create(&cfg.session)?
    };
    Client::connect(TgConfig {
        session,
        api_id: cfg.api_id,
        api_hash: cfg.api_hash.clone(),
        params: Default::default(),
    })
    .await
    .context("cannot connect to Telegram")
}

/// A failed save must never take the bot down (read-only or missing volume): warn and keep running.
fn save_session(client: &Client, path: &Path) {
    if let Err(e) = client.session().save_to_file(path) {
        log::warn!(
            "could not save the session to {}: {e} - the bot will have to sign in again after a restart; \
             mount a persistent volume there",
            path.display()
        );
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cfg = config::Config::from_env()?;
    tg::cleanup_temp(); // leftovers of a previous run that was killed mid-conversion

    let mut client = connect(&cfg, false).await?;

    // A saved login belongs to ONE bot. If BOT_TOKEN was changed, the old session would keep running as the old
    // bot and its dump-channel access key would be useless - so start from scratch in that case.
    if client.is_authorized().await? {
        let me = client
            .get_me()
            .await
            .context("cannot read which account the saved session belongs to")?;
        if me.id() != cfg.bot_id {
            log::warn!("the saved session belongs to another bot - BOT_TOKEN changed. Logging in again with the new token.");
            drop(client);
            let _ = std::fs::remove_file(&cfg.session);
            let _ = std::fs::remove_file(&cfg.dump_cache);
            client = connect(&cfg, true).await?;
        }
    }
    if !client.is_authorized().await? {
        client
            .bot_sign_in(&cfg.bot_token)
            .await
            .context("bot sign-in failed - check BOT_TOKEN, API_ID and API_HASH")?;
    }
    save_session(&client, &cfg.session);
    log::info!("signed in as bot {} - waiting for messages", cfg.bot_id);

    let session_path = cfg.session.clone();
    let app = Arc::new(bot::App::new(client.clone(), cfg));

    bot::spawn_worker(app.clone());
    {
        // Find the dump channel and prove access (post + delete a test message) without waiting for a command.
        let app = app.clone();
        tokio::spawn(async move { bot::startup_check(&app).await });
    }
    {
        // Keep the session (auth key, update state) fresh on disk, not only at start-up.
        let client = client.clone();
        let path = session_path.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                save_session(&client, &path);
            }
        });
    }

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                log::info!("shutting down");
                break;
            }
            update = client.next_update() => match update {
                Ok(Update::NewMessage(message)) if !message.outgoing() => {
                    bot::dispatch(&app, message);
                }
                Ok(Update::CallbackQuery(query)) => {
                    // a button on the card was tapped
                    let app = app.clone();
                    tokio::spawn(async move {
                        let uid = query.sender().id();
                        let data = String::from_utf8_lossy(query.data()).to_string();
                        let toast = match query.load_message().await {
                            Ok(card) => bot::on_button(&app, uid, &data, card).await,
                            Err(e) => {
                                log::warn!("cannot load the message of a button: {e}");
                                None
                            }
                        };
                        let answer = query.answer();
                        let _ = match toast {
                            Some(text) => answer.text(text).send().await,
                            None => answer.send().await,
                        };
                    });
                }
                Ok(_) => {}
                Err(e) => {
                    log::warn!("update error: {e}");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    save_session(&client, &session_path);
    tg::cleanup_temp();
    Ok(())
}

/// Deploys stop the old container with SIGTERM: leave cleanly so the session is saved and temp files are removed.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
