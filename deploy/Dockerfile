# syntax=docker/dockerfile:1

FROM rust:1-slim-bookworm AS builder

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY src ./src

# The frontend is built outside Docker; keep an empty dist when it is absent.
RUN mkdir -p src/frontend/dist \
    && touch src/frontend/dist/.keep \
    && cargo build --release

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl passwd \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home-dir /app --no-create-home \
        --shell /usr/sbin/nologin travel \
    && mkdir -p /var/lib/travel-ai \
    && chown travel:travel /var/lib/travel-ai

WORKDIR /app

COPY --from=builder /build/target/release/travel-ai /app/travel-ai
COPY src/prompts /app/src/prompts
COPY data /app/data
COPY --from=builder /build/src/frontend/dist /app/src/frontend/dist
COPY migrations /app/migrations

ENV BIND=0.0.0.0:8080
ENV DATABASE_URL=sqlite:///var/lib/travel-ai/app.db

EXPOSE 8080

USER travel

CMD ["/app/travel-ai"]
