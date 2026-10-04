FROM mcr.microsoft.com/devcontainers/universal:linux

COPY --from=ghcr.io/jdx/mise:latest /usr/local/bin/mise /usr/local/bin/mise

ENV MISE_DATA_DIR="/mise"
ENV MISE_CONFIG_DIR="/mise"
ENV MISE_CACHE_DIR="/mise/cache"
ENV PATH="/mise/shims:$PATH"

RUN mkdir -p /mise && chown -R 1000:1000 /mise
WORKDIR /tmp/mise-project
COPY mise.toml ./
RUN mise trust
RUN mise bootstrap packages apply -y
RUN mise bootstrap -y
RUN chown -R 1000:1000 /mise
