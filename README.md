# smartusb: aparelhos USB para Fogu

Uma lib Rust (`fogusmartusb`) com API C, mais um módulo Fogu (`smartusb.fg`) que a usa via
`fogu:ffi`. Ela faz três coisas:

1. **Reconhece** o que está conectado na USB: leitor de código de barras/QR Code, leitor de
   smart card, leitor NFC, token de certificado digital (e-CPF/e-CNPJ), chave FIDO, leitor
   biométrico, RFID/crachá, tarja magnética, balança, impressora, câmera, pen drive, porta
   serial, teclado, mouse e outros.
2. **Avisa** quando algo é conectado ou desconectado.
3. **Lê scanners** em qualquer modo (HID POS, serial/COM ou teclado, inclusive em modo
   teclado **exclusivo** no Windows) com uma API só, e **analisa o conteúdo** lido: PIX,
   NFC-e, chave de NF-e, boleto, EAN/GTIN, GS1, CPF/CNPJ, URL, Wi-Fi, vCard...
4. **Imprime** em impressoras de cupom (ESC/POS: texto, QR Code, código de barras, corte,
   gaveta) ou manda bytes crus para qualquer impressora.
5. **Usa leitores de digital** pelo Windows Biometric Framework: identifica quem tocou o
   leitor (Windows Hello) e captura a imagem da digital.

Funciona no Windows, no Linux e no macOS. No Windows não precisa de driver extra nem de
compilador C.

```
fogusmartusb/     lib Rust (cdylib) com a API C
native/           fogusmartusb.dll / libfogusmartusb.so / .dylib (gerado pelo build)
smartusb.fg       wrapper Fogu: devices(), watch(), openScanner(), openHid()...
smartusb.test.fg  testes (fogu test)
wcopy.fg          leitor NFC wCopy/NSCCN (0416:b030, HID sem PC/SC): Reader e Card
examples/         cartao.fg (registro pessoal num cartão pelo leitor wCopy), barcode.fg (leitor de
                  código de barras), pendrive.fg (gestão de um pen drive)
main.fg           exemplo: lista, espera um scanner e mostra as leituras
devices.json      exemplo de base própria (nomes/tipos dos seus aparelhos)
usbids.ps1/sh     baixa o usb.ids (nomes de ~20 mil aparelhos) para data/
```

<!-- fogu:plataformas -->
## Plataformas, binários e pré-requisitos

Ao importar `smartusb.fg`, a lib reconhece o sistema e a arquitetura
(`system.info.os`/`arch`) e já carrega o binário certo — `.dll` no Windows,
`.so` no Linux, `.dylib` no macOS. Se ele faltar, o programa não para no `import`:
o erro (com a lista de caminhos tentados) volta na primeira chamada que precisar da lib.
`nativeInfo()` mostra o que foi detectado: `{ platform, os, file, path, loaded, error }`.

| Plataforma | Arquivo | Neste repositório |
|---|---|---|
| `windows-x64` | `native/windows-x64/fogusmartusb.dll` | incluído |
| `windows-arm64` | `native/windows-arm64/fogusmartusb.dll` | — |
| `linux-x64` | `native/linux-x64/libfogusmartusb.so` | — |
| `linux-arm64` | `native/linux-arm64/libfogusmartusb.so` | — |
| `macos-x64` | `native/macos-x64/libfogusmartusb.dylib` | — |
| `macos-arm64` | `native/macos-arm64/libfogusmartusb.dylib` | — |

Ordem de busca: o caminho passado a `load(path)`, a variável `FOGU_SMARTUSB_LIB`,
`native/<plataforma>/`, `native/` e o build do cargo no projeto em execução, uma
cópia em `fogu_modules/` e os pacotes instalados com `fogu add` (`$FOGU_HOME/fogu_modules`).

**Pré-requisitos**

- **Windows**: nada (SetupAPI, Raw Input, WinBio e spooler do sistema).
- **Linux**: em tempo de execução `sudo apt install libudev1 libpcsclite1` (e permissão nos `/dev/hidraw*`/`/dev/ttyUSB*`, por regra udev ou grupo `plugdev`/`dialout`). Para compilar o `.so`: `sudo apt install libudev-dev libpcsclite-dev` e `sh build.sh`.
- **macOS**: o `.dylib` precisa ser compilado num Mac (`sh build.sh` lá): linka `IOKit`/`PCSC.framework` do SDK da Apple.

**Gerar os binários**

```sh
fogu run build:native        # Windows: native/windows-<arch>/ (build.ps1)
sh build.sh                  # Linux/macOS/WSL: o sistema atual
sh build.sh all              # Linux x64/arm64 e macOS x64/arm64 (cargo-zigbuild + zig)
```

## Callbacks do fogu:ffi (Fogu 0.7+, testado com o 0.7.4)

A camada nativa usa `ffi.callback` (veja `_native.fg`, igual em todas as libs deste repositório):

- **Sink de saída**: a lib recebe um callback uma vez (`*_set_sink`) e, chamada com
  `buf = nil, cap = 0`, entrega a resposta inteira por ele. As funções antigas, com buffer
  do chamador, continuam exportadas e funcionando (API C compatível).
- **Regras do fogu:ffi respeitadas**: um callback só roda na thread do interpretador, durante
  uma chamada FFI — as threads da lib nunca chamam o Fogu; elas enfileiram e a chamada
  `*_dispatch`/leitura entrega na thread de quem chamou. Um erro dentro de um callback derruba o
  programa, então o callback só guarda os dados e os handlers do usuário rodam depois, fora
  do frame nativo (com `?`, erros e `sleep` normais).
- **Esperas cooperativas**: uma chamada FFI bloqueante congela todas as tarefas `fg`, então as
  esperas longas são feitas em fatias de 10 ms com `time.delay(1ms)` entre elas (o mesmo
  padrão do pacote oficial `hardware`).

As respostas chegam inteiras pelo **sink** (`fsu_set_sink`), sem buffer fixo nem `fsu_pending`. `Watcher.wait`, `Scanner.read`, `Hid.read` e `Printer.read` esperam em fatias de 10 ms cedendo a vez às tarefas `fg` (o que já chegou fica guardado na lib entre as fatias). A captura biométrica do Windows não pode ser fatiada e continua esperando direto. `usb.setCooperative(false)` volta à espera direta.
- **Fogu 0.7.4**: o código compilado chama direto as funções FFI de assinatura numérica (sem
  alocação por chamada), então sondar status/eventos num laço é barato; `fogu .` roda o projeto
  (o script `start` do `fogu.json`); os tipos `int32`, `uint8`, `float64`... valem nas assinaturas.
<!-- /fogu:plataformas -->

## Build e execução

```sh
fogu run build:native       # ou ./build.ps1 (Windows) / ./build.sh (Linux/macOS)
fogu run usbids             # opcional: nomes de fabricantes e produtos
fogu run main.fg
fogu test
cd fogusmartusb && cargo test                 # testes da lib Rust
cargo run --example list                      # lista o que está conectado
cargo run --example scan -- keyboard          # lê de um scanner pelo terminal
cargo run --example exclusive_selftest        # confere o modo teclado exclusivo (Windows, depois do build)
```

Requer Rust ≥ 1.85 (edition 2024). No Linux: `apt install libudev-dev libpcsclite-dev`.

```fogu
import usb from "./smartusb.fg"

fn main() {
    for d in usb.devices()? {
        print(d.kind, d.name)            // barcode_scanner "Honeywell Voyager 1450g"
    }

    const s = usb.openScanner()?          // o primeiro scanner reconhecido
    const r = s.read(30s)?                // nil se ninguém ler nada
    print(r.text, r.symbology)            // "000201..." "qrcode"
    if r.content.type == "pix" { print(r.content.amount, r.content.key) }
    s.close()?
    return true, nil
}
main()
```

## Reconhecimento

`usb.devices()` devolve uma lista com um item por aparelho:

| campo | |
|---|---|
| `id` | identificador pela porta física (`1400-9.2`: barramento e portas). Fica o mesmo enquanto o aparelho continuar na mesma porta |
| `vid`, `pid`, `serial` | VID/PID em hex |
| `name`, `vendor`, `product` | nome legível |
| `kind` | tipo principal: `barcode_scanner`, `smartcard_reader`, `nfc_reader`, `security_token`, `fido_key`, `rfid_reader`, `fingerprint_reader`, `msr`, `scale`, `printer`, `camera`, `phone`, `storage`, `audio`, `network`, `bluetooth`, `serial`, `gamepad`, `touch`, `ups`, `keyboard`, `mouse`, `hid`, `hub`, `vendor`, `unknown` |
| `kinds` | todos os papéis (um teclado com touchpad: `["keyboard", "mouse"]`) |
| `category` | o tipo em português (`"Leitor de código de barras / QR Code"`) |
| `confidence`, `source` | de onde veio o tipo (abaixo) |
| `modes` | como ler pela API de scanner: `hidpos`, `serial`, `keyboard`, `hid` |
| `ports` | portas seriais do aparelho (`COM5`, `/dev/ttyACM0`) |
| `readers` | nomes PC/SC, para usar no `connect()` do [smartcard](../smartcard) |
| `interfaces`, `hid` | classes das interfaces USB e coleções HID (usage page, usage, caminho) |
| `speed`, `usb_version`, `device_version`, `driver` | `driver` só no Windows |

O tipo sai do que dá para saber sem abrir o aparelho. Por ordem de precedência:

1. **`user`**: sua base JSON (`usb.loadIds("devices.json")`), para dar nome e tipo aos
   aparelhos da empresa: `[{ "vid": "0c2e", "pid": "0b61", "name": "Caixa 1", "kind": "barcode_scanner" }]`
   (`pid` omitido vale para todos os produtos do fabricante).
2. **`builtin`**: modelos conhecidos (ACR122U, OMNIKEY, eToken 5110, YubiKey, U.are.U...).
3. **`class`**: classe USB das interfaces (0x0B smart card, 0x07 impressora, 0x08
   armazenamento, 0x0E vídeo...) e **usage page HID**: 0x8C scanner HID POS, 0x8D
   balança, 0x8E tarja magnética, 0xF1D0 FIDO, teclado, mouse, joystick, no-break.
4. **`vendor`**: fabricante conhecido **e** interface coerente. Por exemplo, um Zebra,
   Honeywell, Datalogic, Newland ou Opticon que aparece como teclado ou porta COM é scanner;
   um SafeNet, Rutoken ou Yubico com CCID é token; um DigitalPersona, SecuGen ou Futronic é
   leitor biométrico.
5. **`name`**: palavras no nome do produto (`barcode`, `scanner`, `QR`, `RFID`, `fingerprint`...).

`usb.loadIds("data/usb.ids")` (depois de `fogu run usbids`) completa os **nomes** de
quase qualquer aparelho. O usb.ids não diz o tipo.

Outras funções: `usb.find(kind)`, `usb.scanners()`, `usb.smartcardReaders()`,
`usb.device(seletor)` e `usb.identify({ vid, pid, interfaces, hid, ... })`, que reconhece um
aparelho descrito à mão, sem ele estar conectado.

### Scanners que se apresentam como "USB Keyboard"

Muitos scanners baratos usam o VID/PID de um chip de teclado e se chamam só "USB
Keyboard". Nenhuma informação USB os separa de um teclado. Para esses:

- `usb.detectScanner(30s)` escuta todos os teclados e devolve qual deles mandou uma rajada
  de leitura (`source: "vvvv:pppp"` e o `device`). Grave esse VID/PID na sua base com
  `kind: "barcode_scanner"` e ele passa a ser reconhecido.
- ou leia direto com `usb.openScanner("keyboard")`, que aceita qualquer teclado mas só
  rajadas rápidas, como as de um scanner (intervalo médio ≤ 35 ms, pelo menos 4 caracteres).
  A digitação humana é ignorada.

## Conectar / desconectar

```fogu
const w = usb.watch()?
const dev = w.waitFor("barcode_scanner", 60s)?   // nil se o tempo acabar
// ou eventos crus:
for ev in w.wait(5s)? { print(ev.event, ev.kind, ev.name) }   // added | removed | changed
w.close()?
```

Na primeira espera, os aparelhos já conectados vêm como `added`. `changed` aparece quando
o driver termina de subir depois (a porta COM ou o leitor PC/SC aparecem um pouco depois
do aparelho). `wait` espera em fatias de 10 ms, então outras tarefas `fg` continuam
rodando enquanto se espera (veja "Callbacks do fogu:ffi").

## Scanners

```fogu
const s = usb.openScanner(seletor, opções)?
const r = s.read(timeout)?     // ou s.readText(timeout)
s.write("16540d", false)       // comando (modo serial/HID), hex/buffer/lista, ou texto com true
s.close()?
```

**Seletor**: `""` (o primeiro scanner reconhecido), o `id`, `"vvvv:pppp"`, uma porta (`"COM5"`,
`"/dev/ttyACM0"`), um caminho HID, ou `"keyboard"` (qualquer teclado, com filtro de rajada).

**Modos** (`mode: "auto"` escolhe o melhor que o aparelho oferece):

| modo | | |
|---|---|---|
| `hidpos` | HID POS (usage page 0x8C) | **recomendado**: leitura exclusiva, nada é digitado na janela em foco, códigos longos e binários chegam inteiros |
| `serial` | USB COM / CDC-ACM | também exclusivo. `baud` (9600), termina em CR/LF ou após `idle_ms` sem dados |
| `keyboard` | emulação de teclado | o padrão de fábrica da maioria dos scanners. No Windows lê por Raw Input; no Linux/macOS pelo hidraw |
| `hid` | relatórios HID brutos | tarja magnética, balança, aparelhos do fabricante |

A maioria dos scanners troca de modo lendo um código de configuração do manual (procure
"USB HID POS", "USB COM"/"USB CDC" ou "USB Keyboard").

**Opções**: `exclusive` (modo teclado, Windows: veja abaixo), `layout` (`"us"` padrão, ou
`"abnt2"`: o layout que o **scanner** emula),
`terminator` (`"enter"` padrão ou `"tab"`), `idle_ms` (silêncio que encerra uma leitura sem
terminador), `min_length`, `max_gap_ms` (filtro de rajada do modo teclado; `false` desliga),
`baud`, `port`, `usage_page`, `aim` (`true`: separa o prefixo de simbologia AIM, como `]Q1`).

**Leitura**: `{ text, raw, mode, aim, symbology, content, device, source, time }`. No modo
teclado vêm também `keys`, `avg_gap_ms` e `terminator`, e `source` diz qual teclado mandou.

### Modo teclado

O scanner manda **códigos de tecla**, não texto. A lib traduz sozinha, sem passar pelo
layout do sistema, então:

- um scanner em layout US funciona igual num Windows configurado em ABNT2 (o clássico `/`
  que vira `;`, ou `ç` no lugar de `;`, não acontece);
- o Caps Lock do computador não inverte as letras;
- Ctrl+tecla vira caractere de controle (o `GS` = Ctrl+] separa os campos GS1);
- Alt+teclado numérico (`Alt+0199` = `Ç`), que muitos scanners usam para não depender de
  layout, é decodificado;
- as teclas mortas do ABNT2 (`~a` = `ã`) são compostas.

No Windows a leitura é por **Raw Input**: separa o scanner do teclado de verdade e funciona
com o programa em segundo plano. Por padrão as teclas também chegam à janela em foco; com
**`exclusive: true`** elas são engolidas:

```fogu
const s = usb.openScanner("0c2e:0b61", { exclusive: true })?   // um scanner específico
```

Como funciona: um hook de teclado (`WH_KEYBOARD`) que o Windows carrega nos programas com
janela pergunta à lib, a cada tecla, se ela veio do scanner. O Raw Input da mesma tecla
chega antes, com o aparelho de origem, então só as teclas do scanner são bloqueadas; o
teclado de verdade continua normal. Para conferir na sua máquina, rode
`cargo run --example exclusive_selftest` (injeta Shift, inofensivo, e confere que foi
bloqueado). `info().blocked` conta as teclas engolidas. Limites do Windows: o hook não
entra em programas de **32 bits**, em programas **elevados** (quando o seu não é) nem
naqueles que leem o teclado por Raw Input/DirectInput (jogos). Nesses casos as teclas
passam. O modo exclusivo exige um scanner específico (não vale com `"keyboard"`, que
bloquearia o teclado de verdade) e a lib carregada como DLL.

No Linux, dê acesso ao `/dev/hidraw*` com uma regra udev, por exemplo
`SUBSYSTEM=="hidraw", ATTRS{idVendor}=="0c2e", MODE="0660", GROUP="plugdev"`. No macOS,
permita "Monitoramento de entrada" ao terminal ou ao app.

### Exemplo: leitor de código de barras pela USB

`examples/barcode.fg` acha o leitor, lê códigos e mostra o conteúdo analisado (EAN, PIX,
NF-e, boleto, URL, Wi-Fi, vCard...). Leitores baratos se apresentam como "USB Keyboard",
então na primeira vez, sem leitor reconhecido, ele pede uma leitura, descobre de qual
teclado ela veio e grava o VID/PID em `devices.json` com `kind: "barcode_scanner"`; das
próximas vezes o leitor já aparece em `usb.scanners()`.

```sh
fogu run examples/barcode.fg                    # lê até ficar 60s sem leitura
fogu run examples/barcode.fg --exclusivo        # Windows: as teclas do leitor não vão para a janela em foco
fogu run examples/barcode.fg --layout abnt2     # o leitor emula teclado ABNT2
fogu run examples/barcode.fg --csv leituras.csv # guarda cada leitura (hora;texto;simbologia;tipo;origem)
fogu run examples/barcode.fg --aprender         # redescobre qual teclado é o leitor
fogu run examples/barcode.fg --leitor keyboard  # qualquer teclado (só rajadas de leitor)
```

Se nada for lido: confira se o leitor está ligado (bipe/LED ao ler) e configurado como
**teclado USB (HID)**, o padrão de fábrica; leitores em modo USB COM aparecem em `d.ports`
(`--leitor COM5`) e em HID POS em `d.modes` (`hidpos`), e o exemplo os usa do mesmo jeito.

### Conteúdo

`r.content` (e `usb.analyze(texto)`, sem scanner) traz `type` e os campos de cada tipo,
conferindo os dígitos verificadores (`valid`):

| `type` | campos |
|---|---|
| `pix` / `emv_qr` | `key`, `url` (PIX dinâmico), `amount`, `merchant`, `city`, `txid`, `valid` (CRC16) |
| `nfce` | QR Code da NFC-e: `url`, `key` (chave de 44 dígitos), `valid` |
| `nfe_key` | chave de NF-e/NFC-e/CT-e/MDF-e/SAT: `model`, `uf`, `year_month`, `cnpj`, `series`, `number` |
| `boleto` / `arrecadacao` | código de barras de 44 dígitos (DV geral) / contas e tributos (começa com 8) |
| `boleto_linha` / `arrecadacao_linha` | linha digitável (47 / 48 dígitos) |
| `ean13`, `ean8`, `upca`, `gtin14` | `code`, `valid` |
| `gs1` | `fields`: `gtin`, `batch`, `expiry`, `serial`, `sscc`... (GS1-128/DataMatrix) |
| `cpf`, `cnpj` | `code` (só quando o DV confere) |
| `url`, `email`, `phone`, `sms`, `geo`, `otp`, `wifi` (`ssid`, `password`, `security`), `vcard` (`name`, `phone`, `email`, `org`) | |
| `json` (`value`), `number`, `text`, `empty` | |

`symbology` vem do prefixo AIM, se o scanner estiver configurado para mandá-lo (`qrcode`,
`datamatrix`, `ean_upc`, `code128`, `code39`, `itf`, `pdf417`, `aztec`, `gs1_databar`...). Em
HID POS, a lib também procura o identificador no relatório.

### HID POS de qualquer fabricante

Ao abrir um scanner HID POS, a lib lê o **descritor de relatório** do aparelho e usa o que
ele declara (HID Usage Tables, página 0x8C): onde ficam os dados lidos (*Decoded Data*), o
identificador de simbologia e o bit de *dados continuados*, que junta códigos longos
divididos em vários relatórios. Assim o formato de cada fabricante é entendido sem tabela
própria. Se o descritor não estiver disponível ou não declarar esses campos, a lib cai
no formato mais comum (número 0x02, tamanho, dados). A leitura traz `decoder`
(`"descriptor"` ou `"heuristic"`), e `info().hidpos_decoder` diz qual será usado.

## Smart cards e tokens

Leitores de smart card, NFC e tokens são reconhecidos aqui (`kind`, e `readers` com os nomes
PC/SC). Para falar com o cartão, use o [smartcard](../smartcard):

```fogu
import usb from "./smartusb.fg"
import scard from "../smartcard/scard.fg"

const leitor = usb.find(usb.SMARTCARD_KINDS)?
const card = scard.connect(leitor.readers[0])?
```

### Leitor NFC "wCopy Smart Reader" (NSCCN, 0416:b030)

Esse leitor barato (vendido como wCopy, NSCCN, rfcardwriter.com; o Windows o mostra como
"Dispositivo de armazenamento" porque a interface 1 é um disquinho USB com o instalador do
programa do fabricante) **não tem PC/SC nem CCID**: é um aparelho HID com um PN532 dentro,
falando o conjunto de comandos do PN532 numa moldura própria. `wcopy.fg` fala esse
protocolo pelo `openHid` do smartusb (engenharia reversa publicada em
[wrcrooks/wcopy-nfc](https://github.com/wrcrooks/wcopy-nfc), MIT; conferida aqui com o
aparelho de verdade) e devolve um `Card` com a **mesma API do `scard.Card`** do smartcard
para cartões de memória: `uid()`, `readBlock`, `writeBlock`, `readSector`, `readPages`,
`writePages`, `auth`, `dump`.

```fogu
import wcopy from "./wcopy.fg"

const r = wcopy.open()?                      // GetFirmwareVersion confere a conversa
print(r.fw.version)                          // "1.6" (PN532)
const card = r.poll(30s)?                    // nil se ninguém aproximar um cartão
print(card.name(), card.uidHex, card.kind)   // "MIFARE Classic 1K" 8AAE3F05 mifare_classic_1k
const b = card.readBlock(4)?                 // key "" tenta as chaves conhecidas (fábrica, wCopy, NDEF...)
card.writeBlock(4, bytes("46500000000000000000000000000000", "hex"))?
print(card.key())                            // chave que abriu o setor: FFFFFFFFFFFF
r.close()?
```

| | |
|---|---|
| `wcopy.find()` | o aparelho do smartusb, ou nil se não há leitor |
| `wcopy.open(seletor = "0416:b030")` | `Reader`; `fw` = `{ ic, version, revision, support }` |
| `Reader.poll(timeout)` / `waitRemoved(uid, timeout)` | espera um cartão / espera ele sair |
| `Reader.command(cmd, dados)` / `exchange(cmdDoCartão)` | qualquer comando PN532 / InDataExchange |
| `Card` | `kind`, `family`, `sak`, `atqa`, `uidHex`, `name()`, `uid()`, `key()`, `auth(bloco, chave, chaveB)`, `readBlock`, `writeBlock(..., unsafe)`, `readSector`, `readPages`, `writePages(..., unsafe)`, `read`, `write`, `dump`, `reselect()` |
| `wcopy.KNOWN_KEYS` / `KEY_WCOPY` | as chaves tentadas com `key = ""`; a que o programa do fabricante deixa nos trailers (`D3F7D3F7D3F7`) |
| `wcopy.frame` / `parse` / `sectorOf` / `isTrailer` / `kindOf` / `accessBitsValid` | as funções puras (testadas sem leitor) |

Mesmas proteções do smartcard: o bloco 0 e os trailers do Classic e as páginas 0..3 do
Ultralight só com `unsafe = true`, e um trailer com bits de acesso inconsistentes é sempre
recusado. Uma autenticação que falha derruba a sessão RF do cartão; o `Card` seleciona de
novo sozinho (antes de cada setor, entre tentativas de chave e numa leitura que falha).
Cartões ISO 14443-4 (DESFire, pagamento) são reconhecidos (`iso14443_4`) e aceitam
`exchange()`, mas não há leitura de memória neles.

`examples/cartao.fg` grava, lê e apaga um **registro pessoal** (nome, nascimento, documento,
telefone, e-mail) com esse leitor, no mesmo formato do
[smartcard/examples/registro.fg](../smartcard/examples/registro.fg): o cartão gravado por um
leitor é lido pelo outro.

```sh
fogu run examples/cartao.fg                 # ler
fogu run examples/cartao.fg gravar "Ana Silva" 1990-05-12 12345678 912345678 ana@exemplo.pt
fogu run examples/cartao.fg apagar
fogu run examples/cartao.fg dump            # setores em hex, com a chave que abriu cada um
```

## Pen drives e discos USB

Aparelhos de armazenamento aparecem em `usb.devices()` como `storage` (o smartusb não
mapeia a letra da unidade; isso é do sistema de arquivos). `examples/pendrive.fg` junta as
duas pontas: acha a unidade removível (Windows: `fsutil fsinfo drivetype`), mostra rótulo
e espaço, e faz a gestão dos arquivos com `fogu:fs`, sempre dentro do pen drive:

```sh
fogu run examples/pendrive.fg                              # info + raiz
fogu run examples/pendrive.fg listar fotos
fogu run examples/pendrive.fg criar notas/ola.txt "Olá"
fogu run examples/pendrive.fg ler notas/ola.txt
fogu run examples/pendrive.fg pasta fotos/2026
fogu run examples/pendrive.fg copiar C:/Users/eu/doc.pdf docs/   # de/para o PC também; pastas inteiras
fogu run examples/pendrive.fg mover notas/ola.txt notas/velho.txt
fogu run examples/pendrive.fg apagar notas/velho.txt             # pasta com conteúdo: --tudo
fogu run examples/pendrive.fg ejetar                             # remoção segura
fogu run examples/pendrive.fg --unidade E: listar                # outra unidade
```

`mover`, `apagar` e `criar` recusam caminhos fora do pen drive, e `apagar` nunca apaga a raiz.

## Impressoras

```fogu
const p = usb.openPrinter("EPSON TM-T20X")?       // nome no Windows, id, "vvvv:pppp", "COM3", "/dev/usb/lp0"
const r = usb.receipt("cp860")                    // página de código da impressora
r.center().size(2).bold().line("MINHA LOJA").size(1).bold(false)
r.left().line("Pão francês          R$ 4,50")
r.qr("https://exemplo.com/nfce?p=...", 6).barcode("7891000315507", "ean13")
r.feed(3).cut().drawer()
p.print(r)?
print(p.status()?)                                 // { online, drawer, paper_near_end, paper_out } (usb/serial)
p.close()?
```

Caminhos (`openPrinter(selector, { backend })`, `"auto"` escolhe):

| backend | | |
|---|---|---|
| `spooler` | impressora instalada no Windows, em trabalho RAW | o mais simples no Windows. `usb.printers().system` lista os nomes |
| `usbprint` | o driver de impressora USB do Windows, sem instalar a impressora | aparelhos de classe impressora |
| `usb` | bulk direto | Linux/macOS, e Windows quando o driver é WinUSB (impressoras de cupom que vêm com driver WinUSB/libusb) |
| `serial` | porta COM | impressoras seriais ou USB-serial |
| `file` | `/dev/usb/lp0` | Linux |

Comandos ESC/POS (`usb.escpos([...])` ou os métodos do `receipt`): `init`, `codepage`
(`cp437`, `cp850`, `cp860`, `cp1252`), `text`, `line`, `align`, `bold`, `underline`, `invert`,
`size`, `qr` (com `size`, `ec`), `barcode` (`ean13`, `ean8`, `upca`, `upce`, `code39`, `itf`,
`codabar`, `code93`, `code128`; `height`, `width`, `hri`), `feed`, `cut` (`"partial"`), `drawer`
(`pin` 2 ou 5), `beep`, `raw` (hex). O texto é convertido para a página de código escolhida
(acentos inexistentes nela caem para a letra sem acento). `usb.encode(texto, "cp850")` faz
só a conversão. Impressoras de etiqueta (ZPL, EPL, TSPL) recebem a linguagem delas com
`p.write("^XA...^XZ", ...)`. A lib manda os bytes como estão.

`status()` usa `DLE EOT` (ESC/POS) e só funciona onde há leitura (`usb`, `serial`). Pelo
spooler e pelo `usbprint` a impressão é só de ida.

## Leitores de digital

Pelo **Windows Biometric Framework**, ou seja, qualquer leitor com driver do Windows Hello
(os integrados de notebook e os USB da DigitalPersona/HID, SecuGen, Futronic, Goodix,
Synaptics...):

```fogu
print(usb.fingerprintReaders()?)                    // [{ unit_id, description, manufacturer, model, ... }]
const who = usb.identifyFinger(30s)?                // nil se ninguém tocar
if who != nil && who.matched { print(who.account, who.finger) }   // "PC\maria" "right_index"
const img = usb.captureFinger(30s, "digital.bmp")?  // imagem crua (precisa de administrador)
print(img.width, img.height, img.dpi)
```

- `identifyFinger` diz **quem** tocou, entre as contas do Windows com digital cadastrada no
  Windows Hello (`{ matched: false }` se não for ninguém cadastrado). Serve para login e
  confirmação de operador.
- `captureFinger` devolve a imagem (registro ANSI INCITS 381 em `record`, e `.pgm`/`.bmp` se
  pedir) para guardar ou usar com o seu próprio comparador. O Windows só libera a captura
  crua para administrador.
- O WinBio entrega o sensor ao programa em primeiro plano. Um programa de console pede o
  foco (`WinBioAcquireFocus`), o que também exige administrador; sem isso, as chamadas
  esperam até o tempo acabar.
- Leitores sem driver WBF (alguns modelos antigos, que só funcionam com o SDK do
  fabricante) aparecem em `devices()` como `fingerprint_reader`, mas não aqui. No Linux
  use o `fprintd`.

## HID genérico

```fogu
const h = usb.openHid("0801:0002", { usage_page: 65280 })?   // ou um caminho de d.hid[i].path
const report = h.read(1s)?                                    // buffer, ou nil
h.write([0, 1, 2])?                                           // 1º byte = número do relatório
const f = h.getFeature(1, 64)?
h.sendFeature([1, 0x10])?
h.close()?
```

No Windows não dá para abrir teclados e mouses pelo HID (o sistema os segura). Para
teclados use o scanner em modo teclado.

## API C (para usar de outras linguagens)

Mesmas convenções do fogusmartcard: handles `i64`; status `0`/`-1` (mensagem em
`fsu_last_error`); respostas JSON/hex num buffer do chamador terminadas em NUL, retornando o
tamanho, `-(tamanho necessário)` se o buffer for pequeno (a saída fica guardada: busque com
`fsu_pending`, sem repetir a leitura) ou `-1` em erro. Com um callback registrado em
`fsu_set_sink(sink)` (`int32_t (*sink)(const uint8_t*, size_t)`), `buf = NULL, cap = 0`
entrega a saída inteira pelo callback (é o que o `smartusb.fg` usa).

```c
int64_t fsu_version(char *buf, int64_t cap);
int64_t fsu_last_error(char *buf, int64_t cap);
int64_t fsu_pending(char *buf, int64_t cap);
int64_t fsu_devices(int32_t flags, char *buf, int64_t cap);                 // flags 1 = hubs
int64_t fsu_device(const char *selector, char *buf, int64_t cap);
int64_t fsu_identify(const char *desc_json, char *buf, int64_t cap);
int64_t fsu_db_load(const char *path);                                      // .json ou usb.ids
int32_t fsu_db_clear(void);
int64_t fsu_db_count(void);
int64_t fsu_pcsc_readers(char *buf, int64_t cap);
int64_t fsu_watch_open(int32_t flags);
int64_t fsu_watch_wait(int64_t watch, int64_t timeout_ms, char *buf, int64_t cap);
int32_t fsu_watch_close(int64_t watch);
int64_t fsu_scanner_open(const char *selector, const char *opts_json);
int64_t fsu_scanner_read(int64_t scanner, int64_t timeout_ms, char *buf, int64_t cap);  // "null" no timeout
int64_t fsu_scanner_write(int64_t scanner, const char *data_hex);
int64_t fsu_scanner_info(int64_t scanner, char *buf, int64_t cap);
int32_t fsu_scanner_close(int64_t scanner);
int64_t fsu_analyze(const char *text, char *buf, int64_t cap);
int64_t fsu_hid_open(const char *selector, const char *opts_json);
int64_t fsu_hid_read(int64_t hid, int64_t timeout_ms, char *buf, int64_t cap);          // "" no timeout
int64_t fsu_hid_write(int64_t hid, const char *data_hex);
int64_t fsu_hid_get_feature(int64_t hid, int32_t report_id, int64_t len, char *buf, int64_t cap);
int32_t fsu_hid_send_feature(int64_t hid, const char *data_hex);
int64_t fsu_hid_info(int64_t hid, char *buf, int64_t cap);
int32_t fsu_hid_close(int64_t hid);
int64_t fsu_printers(char *buf, int64_t cap);
int64_t fsu_printer_open(const char *selector, const char *opts_json);
int64_t fsu_printer_write(int64_t printer, const char *data_hex);
int64_t fsu_printer_read(int64_t printer, int64_t timeout_ms, int64_t max, char *buf, int64_t cap);
int64_t fsu_printer_info(int64_t printer, char *buf, int64_t cap);
int32_t fsu_printer_close(int64_t printer);
int64_t fsu_escpos(const char *commands_json, char *buf, int64_t cap);                 // hex
int64_t fsu_encode(const char *text, const char *codepage, char *buf, int64_t cap);    // hex
int64_t fsu_bio_units(char *buf, int64_t cap);
int64_t fsu_bio_identify(int64_t timeout_ms, char *buf, int64_t cap);                  // "null" no timeout
int64_t fsu_bio_capture(int64_t timeout_ms, const char *save_path, char *buf, int64_t cap);
```

## Limitações

- **Testado sem scanner, impressora USB ou leitor de digital físicos.** Testes automáticos
  cobrem a decodificação (teclas, layouts, descritores HID POS, conteúdo, ESC/POS, páginas
  de código, imagem ANSI 381). Num Windows 11 de verdade foram verificados a enumeração, o
  Raw Input, a lista de impressoras e de leitores de digital, e o modo exclusivo (6 de 6
  teclas injetadas bloqueadas). A leitura ponta a ponta com cada modelo não foi testada.
  Em caso de problema, `raw` (scanner) e `record` (digital) trazem os dados crus.
- **Modo teclado exclusivo**: só no Windows e só em programas de 64 bits, não elevados e
  que leem o teclado pela fila de mensagens (veja acima). No Linux/macOS use HID POS ou
  serial para leitura exclusiva.
- **Aparelhos idênticos sem número de série**: as coleções HID e as portas COM são
  associadas por VID/PID/serial, então dois aparelhos iguais sem serial aparecem com as
  portas/coleções dos dois. O `id` (porta física) continua distinto.
- **Impressoras**: `status()` só por `usb`/`serial`. Impressão gráfica (imagens, logotipos)
  não está no gerador ESC/POS; mande os bytes com `raw`.
- **Biometria**: só Windows (WinBio). Captura crua e foco de console exigem administrador.
  Cadastrar digitais é pelo Windows Hello, não pela lib.
- O reconhecimento por fabricante/nome é heurístico (`confidence`: `high`, `medium`, `low`).
  Para os seus aparelhos, a base própria (`devices.json`) resolve de vez.
