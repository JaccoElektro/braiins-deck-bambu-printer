#!/usr/bin/env bash
# Remove the Bambu Lab Printer widget, installed with ./deploy.sh, from a Braiins Deck.
#
# Usage: ./undeploy.sh <deck-ip>
set -euo pipefail

DECK_IP="${1:?usage: ./undeploy.sh <deck-ip>}"

ssh -o BatchMode=yes -o ConnectTimeout=10 "root@$DECK_IP" \
	/nix/var/nix/gcroots/profiles/bmc/current/bin/bmc-nix-cli remove-packages --name widget-bambu-printer

echo "Removed the Bambu Lab Printer widget from the Deck at $DECK_IP."
