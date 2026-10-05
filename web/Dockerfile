# Dashboard: build the Vite app, serve it from nginx, proxy the engine API.
FROM node:22-alpine AS builder
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci
COPY . .
RUN npm run build

FROM nginx:1.27-alpine
# nginx's entrypoint only envsubsts *defined* variables; without a default the
# literal ${ENGINE_API_HOST} survives into the config and nginx exits with
# `unknown "engine_api_host" variable`. Override at runtime for the real host.
ENV ENGINE_API_HOST=127.0.0.1
COPY nginx.conf.template /etc/nginx/templates/default.conf.template
COPY --from=builder /app/dist /usr/share/nginx/html
EXPOSE 80
CMD ["nginx", "-g", "daemon off;"]