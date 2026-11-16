# Baixa o usb.ids (lista de fabricantes e produtos USB do linux-usb.org,
# GPL-2.0+ ou BSD-3) para data/. Use com usb.loadIds("data/usb.ids").
$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force "$PSScriptRoot\data" | Out-Null
Invoke-WebRequest "http://www.linux-usb.org/usb.ids" -OutFile "$PSScriptRoot\data\usb.ids"
Write-Host "data\usb.ids atualizado"
