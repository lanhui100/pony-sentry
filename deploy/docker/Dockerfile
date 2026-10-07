# deploy/docker/Dockerfile
FROM debian:bookworm-slim

RUN sed -i 's/deb.debian.org/mirrors.ustc.edu.cn/g' /etc/apt/sources.list.d/debian.sources 2>/dev/null || \
    sed -i 's/deb.debian.org/mirrors.ustc.edu.cn/g' /etc/apt/sources.list 2>/dev/null || true

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY target/release/pony-sentry-server /app/pony-sentry-server

EXPOSE 3000
ENV PORT=3000
CMD ["/app/pony-sentry-server"]
