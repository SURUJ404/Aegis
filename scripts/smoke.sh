#!/usr/bin/env sh
# Production smoke test for the Trading Terminal stack.
#
#   API_URL=http://localhost:10000 WEB_URL=http://localhost:4173 sh scripts/smoke.sh
#
# Checks, in order: API health, one real /backtest with every required field and
# no IEEE-754 sentinels, the screener, CORS (front-end origin allowed, any other
# origin rejected), the built front end (shell + hashed asset + SPA fallback),
# and that the bundle really points at the API origin it was built for.
# Exits non-zero if any check fails.

set -eu

API_URL=${API_URL:-http://localhost:10000}
WEB_URL=${WEB_URL:-http://localhost:4173}
WEB_ORIGIN=${WEB_ORIGIN:-$WEB_URL}
# The origin the browser is told to call; equals API_URL unless the smoke test is
# run from inside a container where the API is reached through another name.
API_ORIGIN=${API_ORIGIN:-$API_URL}

if ! command -v curl >/dev/null 2>&1; then
  echo "smoke: curl is required" >&2
  exit 2
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0

ok() { printf '  ok    %s\n' "$*"; }
bad() { printf '  FAIL  %s\n' "$*"; fails=$((fails + 1)); }

status_of() { # url [curl args...]
  url=$1
  shift
  curl -s -o "$tmp/body" -w '%{http_code}' "$@" "$url"
}

echo "smoke: api at $API_URL, web at $WEB_URL"

# --- API ---------------------------------------------------------------
code=$(status_of "$API_URL/health")
if [ "$code" = "200" ]; then ok "GET /health -> 200"; else bad "GET /health -> $code"; fi
if grep -q '"ok":true' "$tmp/body"; then ok "health payload ok:true"; else bad "health payload: $(cat "$tmp/body")"; fi
if grep -q '"data_source"' "$tmp/body"; then ok "health payload data_source"; else bad "health payload has no data_source"; fi

code=$(status_of "$API_URL/symbols")
if [ "$code" = "200" ] && grep -q '"AAPL"' "$tmp/body"; then ok "GET /symbols lists the universe"; else bad "GET /symbols -> $code"; fi

code=$(status_of "$API_URL/backtest" -X POST -H 'content-type: application/json' \
  -d '{"symbol":"AAPL","strategy":"ma","a":20,"b":60,"cost_bps":10}')
if [ "$code" = "200" ]; then ok "POST /backtest -> 200"; else bad "POST /backtest -> $code: $(cat "$tmp/body")"; fi
for field in '"dates"' '"equity"' '"buy_hold_equity"' '"position"' '"trades"' '"exposure"' \
             '"cost_drag_cagr"' '"cagr"' '"sharpe"' '"max_drawdown"' '"buy_hold"'; do
  if grep -q -- "$field" "$tmp/body"; then ok "backtest field $field"; else bad "backtest field $field missing"; fi
done
if grep -Eq 'NaN|Infinity' "$tmp/body"; then bad "backtest payload contains NaN/Infinity"; else ok "backtest payload is finite"; fi

code=$(status_of "$API_URL/screen?strategy=ma&a=20&b=60")
if [ "$code" = "200" ] && grep -q '"symbol"' "$tmp/body"; then ok "GET /screen -> rows"; else bad "GET /screen -> $code"; fi
if grep -Eq 'NaN|Infinity' "$tmp/body"; then bad "screen payload contains NaN/Infinity"; else ok "screen payload is finite"; fi

code=$(status_of "$API_URL/prices?symbol=AAPL&days=10")
if [ "$code" = "200" ] && grep -q '"bars"' "$tmp/body"; then ok "GET /prices -> bars"; else bad "GET /prices -> $code"; fi

# --- CORS --------------------------------------------------------------
code=$(curl -s -o /dev/null -D "$tmp/preflight" -w '%{http_code}' -X OPTIONS \
  -H "Origin: $WEB_ORIGIN" \
  -H 'Access-Control-Request-Method: POST' \
  -H 'Access-Control-Request-Headers: content-type' \
  "$API_URL/backtest")
if grep -qi "access-control-allow-origin: $WEB_ORIGIN" "$tmp/preflight"; then
  ok "CORS allows the front-end origin ($WEB_ORIGIN)"
else
  bad "CORS preflight from $WEB_ORIGIN was not allowed (status $code)"
fi

curl -s -o /dev/null -D "$tmp/foreign" -H 'Origin: https://evil.example' "$API_URL/health"
if grep -qi '^access-control-allow-origin' "$tmp/foreign"; then
  bad "CORS allowed an unknown origin"
else
  ok "CORS rejects an unknown origin"
fi

# --- Front end ---------------------------------------------------------
code=$(status_of "$WEB_URL/")
if [ "$code" = "200" ]; then ok "GET $WEB_URL/ -> 200"; else bad "GET $WEB_URL/ -> $code"; fi
if grep -q 'id="root"' "$tmp/body"; then ok "index.html renders the app shell"; else bad "index.html has no #root"; fi

asset=$(grep -oE '/assets/[A-Za-z0-9._-]+\.js' "$tmp/body" | head -n 1 || true)
if [ -n "$asset" ]; then
  ok "hashed bundle referenced: $asset"
  if [ "$(status_of "$WEB_URL$asset")" = "200" ]; then ok "bundle downloads"; else bad "bundle $asset not served"; fi
  api_host=$(printf '%s' "$API_ORIGIN" | sed -E 's#^(https?://[^/]+).*#\1#')
  api_host=${api_host#http://}
  api_host=${api_host#https://}
  if grep -q "$api_host" "$tmp/body"; then ok "bundle is built for API host $api_host"; else bad "bundle does not mention $api_host"; fi
else
  bad "index.html references no hashed bundle"
fi

code=$(status_of "$WEB_URL/engine")
if [ "$code" = "200" ] && grep -q 'id="root"' "$tmp/body"; then
  ok "SPA fallback serves the shell for /engine"
else
  bad "SPA fallback for /engine -> $code"
fi

# --- Summary -----------------------------------------------------------
if [ "$fails" -gt 0 ]; then
  echo "smoke: $fails check(s) failed"
  exit 1
fi
echo "smoke: all checks passed"
