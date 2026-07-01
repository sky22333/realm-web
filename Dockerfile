FROM node:22-alpine AS web
WORKDIR /web
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

FROM rust:1.96.1-alpine AS builder
RUN apk add --no-cache build-base musl-dev pkgconf sqlite-dev
WORKDIR /app
COPY backend/Cargo.toml backend/Cargo.lock backend/build.rs ./
COPY backend/src ./src
COPY --from=web /web/dist /frontend/dist
ENV SKIP_WEB_BUILD=1
RUN cargo build --release

FROM alpine
RUN apk add --no-cache ca-certificates
COPY --from=builder /app/target/release/realm-web /usr/local/bin/realm-web
WORKDIR /app
RUN mkdir -p /app/data
ENV DATA_DIR=/app/data
EXPOSE 888
ENTRYPOINT ["realm-web"]
