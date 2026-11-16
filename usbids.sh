#!/bin/sh
# Baixa o usb.ids (lista de fabricantes e produtos USB do linux-usb.org,
# GPL-2.0+ ou BSD-3) para data/. Use com usb.loadIds("data/usb.ids").
set -e
cd "$(dirname "$0")"
mkdir -p data
curl -fsSL http://www.linux-usb.org/usb.ids -o data/usb.ids
echo "data/usb.ids atualizado"
