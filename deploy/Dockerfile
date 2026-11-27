# syntax=docker/dockerfile:1

# 单二进制镜像：distroless/cc（带 glibc，Rust 默认目标是 glibc 链接，不能用 static）
# 镜像是"控制台 + 前端静态资源"，不含 JVM、不含 Flink 发行包。

# ---------- 阶段 1：前端（Vue3 + Vite） ----------
FROM node:24-slim AS web
WORKDIR /web
# 只复制清单，先装依赖，让这一层可缓存
COPY apps/web/package.json apps/web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY apps/web/ ./
RUN npm run build

# ---------- 阶段 2：Rust 后端 ----------
FROM rust:1-slim AS builder
WORKDIR /src

RUN apt-get update && apt-get install -y --no-install-recommends pkg-config \
 && rm -rf /var/lib/apt/lists/*

# workspace 里的 path 依赖必须一起进镜像
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates

RUN cargo build --release --locked -p flinkops-api --bin flinkops \
 && cargo build --release --locked -p flinkops-cli

# 预创建 /data 并交给 nonroot（distroless 里没有 shell，无法运行时 mkdir/chown）
RUN mkdir -p /data && chown -R 65532:65532 /data

# ---------- 阶段 3：运行 ----------
FROM gcr.io/distroless/cc-debian12:nonroot
WORKDIR /app

COPY --from=builder /src/target/release/flinkops    /app/bin/flinkops
COPY --from=builder /src/target/release/flinkopsctl /app/bin/flinkopsctl
COPY --from=web     /web/dist                       /app/web
COPY --from=builder --chown=65532:65532 /data       /data
COPY LICENSE NOTICE /app/

ENV FLINKOPS_BIND=0.0.0.0:8080 \
    FLINKOPS_WEB_DIR=/app/web \
    FLINKOPS_STORE=sqlite:///data/flinkops.db?mode=rwc \
    FLINKOPS_CLUSTERS=local=incluster \
    RUST_LOG=info

USER 65532:65532
VOLUME ["/data"]
EXPOSE 8080

# 探针用 K8s 的 httpGet /healthz（distroless 里没有 shell）
ENTRYPOINT ["/app/bin/flinkops"]
