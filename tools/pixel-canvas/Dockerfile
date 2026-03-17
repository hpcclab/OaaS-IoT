# Build context: repo root  (docker build -f tools/pixel-canvas/Dockerfile .)
# ── Stage 1: build ────────────────────────────────────────────────────────────
FROM node:22-bookworm-slim AS builder
WORKDIR /app

# Install dependencies (workspaces root + frontend only)
COPY tools/pixel-canvas/package.json tools/pixel-canvas/package-lock.json ./
COPY tools/pixel-canvas/frontend/package.json ./frontend/
COPY tools/pixel-canvas/wasm-guest/package.json ./wasm-guest/
RUN npm ci --workspace=frontend --ignore-scripts

# Copy source and build
COPY tools/pixel-canvas/frontend/ ./frontend/
RUN npm run build -w frontend

# ── Stage 2: serve ────────────────────────────────────────────────────────────
FROM nginx:1.27-alpine AS runtime

# nginx docker image auto-substitutes *.template → conf.d/*.conf on startup,
# injecting GATEWAY_URL into the proxy_pass directive.
COPY --from=builder /app/frontend/dist /usr/share/nginx/html
COPY tools/pixel-canvas/nginx.conf.template /etc/nginx/templates/default.conf.template

EXPOSE 80
