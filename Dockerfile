# Build and run on the same base family so runtime GLIBC matches the build environment.
FROM lukemathwalker/cargo-chef:latest-rust-trixie AS chef
WORKDIR /app

FROM chef AS planner
COPY . ./
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . ./
RUN cargo build --release --bin renamebot

FROM chef AS runtime
WORKDIR /app
RUN apt-get update \
    && apt-get install -y --no-install-recommends ffmpeg ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/renamebot /usr/local/bin/renamebot
# The thumbnail is read at runtime, so copy the logo/assets into the final image too.
COPY --from=builder /app/assets/ /app/assets/
ENV LOGO_PATH=/app/assets/logo.png

# Secrets are NEVER baked into the image: BOT_TOKEN, API_ID, API_HASH, ADMIN_IDS and DUMP_CHANNEL_ID come from the
# host's environment variables at run time. (.env.example is not copied and not read.)

# Everything that must survive a redeploy lives in /data: the login session and the dump-channel key.
# On Railway: add a Volume to the service and set its mount path to /data (Docker's VOLUME keyword is not allowed
# there). Without a volume the bot still finds the dump channel again after every deploy (it looks it up from
# DUMP_CHANNEL_ID and proves access by posting + deleting a test message), but it signs in again each time.
ENV SESSION_FILE=/data/renamebot.session \
    DUMP_CACHE=/data/dump.chat
RUN mkdir -p /data

# Runs as root on purpose: Railway mounts volumes root-owned, so a non-root user could not write the session.

CMD ["/usr/local/bin/renamebot"]