#!/usr/bin/env bash
# Install bambu-bridge as a user service on this computer (Linux with systemd).
# Re-running updates it. Your configuration in ~/.config/bambu-bridge/ is kept.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
DEST="$HOME/.local/share/bambu-bridge"
CONF="$HOME/.config/bambu-bridge/config.json"

echo "==> Installing to $DEST"
mkdir -p "$DEST"
cp "$HERE/bambu_bridge.py" "$DEST/"
[ -d "$DEST/venv" ] || python3 -m venv "$DEST/venv"
"$DEST/venv/bin/pip" install -q -r "$HERE/requirements.txt"

if [ ! -f "$CONF" ]; then
	mkdir -p "$(dirname "$CONF")"
	cp "$HERE/config.example.json" "$CONF"
	chmod 600 "$CONF"
	echo "==> Created $CONF — fill in your printer's IP, serial, access code and the Deck's IP,"
	echo "    then run this script again."
	exit 0
fi
chmod 600 "$CONF"

echo "==> Starting the user service"
mkdir -p "$HOME/.config/systemd/user"
cp "$HERE/bambu-bridge.service" "$HOME/.config/systemd/user/"
systemctl --user daemon-reload
systemctl --user enable --now bambu-bridge.service
systemctl --user restart bambu-bridge.service
# Keep running when you are not logged in.
loginctl enable-linger "$USER" 2>/dev/null || true
sleep 3
port=$(python3 -c "import json; print(json.load(open('$CONF')).get('listen_port', 9152))")
if curl -fsS "http://127.0.0.1:$port/bambu.json" >/dev/null; then
	echo "==> Running. Status: http://127.0.0.1:$port/bambu.json"
	echo "    In the widget settings on the Deck, set the Bridge URL to http://<this-computer's-IP>:$port"
else
	echo "==> Not answering yet; see: journalctl --user -u bambu-bridge -f"
fi
