#!/bin/sh
# Generates /usr/share/nginx/html/config.js from environment variables,
# then starts nginx.
#
# OBSERVATORY_VESSEL_URLS: comma-separated list of vessel daemon URLs.
#   Example: "http://localhost:7600,http://localhost:7601"
#   Default: empty (Observatory shows AddConnectionDialog on first load)

CONFIG_FILE="/usr/share/nginx/html/config.js"
VESSEL_URLS="${OBSERVATORY_VESSEL_URLS:-}"

if [ -z "$VESSEL_URLS" ]; then
  echo 'window.__OBSERVATORY_CONFIG__ = { defaultConnections: [] };' > "$CONFIG_FILE"
else
  # Build JSON array from comma-separated URLs
  JSON="["
  INDEX=0
  OLD_IFS="$IFS"
  IFS=','
  for URL in $VESSEL_URLS; do
    URL=$(echo "$URL" | xargs)  # trim whitespace
    if [ "$INDEX" -gt 0 ]; then JSON="$JSON,"; fi
    JSON="$JSON{\"id\":\"vessel-$INDEX\",\"url\":\"$URL\"}"
    INDEX=$((INDEX + 1))
  done
  IFS="$OLD_IFS"
  JSON="$JSON]"

  echo "window.__OBSERVATORY_CONFIG__ = { defaultConnections: $JSON };" > "$CONFIG_FILE"
fi

exec nginx -g 'daemon off;'
