# ADR: Estratégia de Boot e o futuro Wrapper vs Standalone

- **Status:** Proposto (não decidido) — registra o quadro de decisão.
- **Data:** 2026-05-28
- **Relacionado:** `docs/proposals/standalone-btrfs.md`,
  `docs/incidents/2026-05-01-limine-fat32-rollback/postmortem.md`,
  `docs/incidents/2026-05-28-fat32-restore-emergency/postmortem.md`

## Contexto

Dois incidentes da mesma classe (01/05 e 28/05) brickaram o boot ao
restaurar um snapshot com `/boot` em FAT32: o swap de subvolume BTRFS é
atômico, mas o `/boot` (kernel + initramfs) vive fora do snapshot e foi
**reconstruído no momento do restore** (`boot::sync_fat32` → cópia de
vmlinuz + `mkinitcpio`). Essa reconstrução é não-atômica e frágil; uma
falha ou interrupção deixa o kernel em `/boot` descasado dos módulos do
root → nenhum módulo carrega (`unknown filesystem type vfat`) →
Emergency Mode.

O fix da branch `fix/boot-sync-recovery` (gate `verify_synced` +
`abort_reboot_boot_desync`) fecha o **sintoma** (bloqueia o reboot quando
o sync falha), mas não a **causa**: enquanto `/boot` for reconstruído no
restore, a janela de inconsistência existe.

Em paralelo, há a ambição de tornar o `snapgroup` **standalone** —
abandonar o wrapper do Snapper e gerir BTRFS direto, com metadados em
JSON (`docs/proposals/standalone-btrfs.md`). Esta ADR existe porque essas
duas questões — **como tratar o boot** e **continuar wrapper ou virar
standalone** — estão acopladas e não devem ser decididas em separado.

## O acoplamento central

`limine-snapper-sync` (terceiro, do Zesko, GPL-3 — **não** faz parte do
Snapper oficial) resolve nativamente o problema de boot que nos quebrou:
ele faz **staging por-snapshot na hora do save** (estado saudável), copia
o kernel/initramfs de cada snapshot para a ESP com hash BLAKE2B, e gera
uma entrada Limine por snapshot. O rollback vira **boot-first**: você
boota a entrada do snapshot (kernel que já casa com os módulos), confirma
que sobe, e só então torna a troca permanente. Não há reconstrução no
restore.

Porém ele **depende do Snapper** — lê `/.snapshots` e parseia as entradas
do Snapper. Logo:

> Se o `snapgroup` virar standalone (sem Snapper), **não pode delegar boot
> ao `limine-snapper-sync`** — ele cai junto. Standalone implica que o
> `snapgroup` passa a ser dono do boot.

O que **é** reaproveitável no mundo standalone é a primitiva
snapper-independente embaixo do `limine-snapper-sync`:

- **`limine-entry-tool`** (instalado via `limine-mkinitcpio-hook`): gestor
  genérico de entradas Limine. `--add-kernel <nome> <initramfs> <vmlinuz>`
  recebe caminhos, copia para a ESP, calcula hash de verificação
  (`ENABLE_VERIFICATION=yes`) e cria a entrada. Não sabe o que é Snapper.
  É exatamente o ponto de integração de boot para um `snapgroup`
  standalone: no save, chamar `limine-entry-tool --add-kernel snapg-<id>
  <snap>/boot/initramfs <snap>/boot/vmlinuz`.

**Nota de licença:** `limine-snapper-sync` é GPL-3; `snapgroup` é MIT.
Estudar a abordagem é livre; **copiar código** GPL-3 para um projeto MIT
contamina a licença. Tratar como referência de design, não como fonte.

## Opções consideradas

### Caminho 1 — Wrapper enxuto (delega boot)

Continuar wrapper do Snapper. **Remover `boot::sync_fat32`** e delegar todo
o tratamento de `/boot` ao `limine-snapper-sync`. O `snapgroup` fica
responsável só pelo seu diferencial: agrupar `@`+`@home` por `snapgroup-id`
e fazer o swap pareado.

- **(+)** Menos código próprio; apoia-se em ferramenta madura para a parte
  mais frágil (boot). Mantém compat com `btrfs-assistant`, `snapper-gui`.
- **(+)** Mata a classe de bug por **delegação**, não reescrita.
- **(−)** Mantém a dependência do Snapper e do ecossistema Limine/Zesko.
- **(−)** Não realiza a ambição standalone (mas `pacman` hook e `explore`
  ainda são adicionáveis como wrapper).
- **(o)** O agrupamento continua sendo do `snapgroup`; o
  `limine-snapper-sync` é per-config e não conhece o par root+home.

### Caminho 2 — Standalone + `/boot` em BTRFS (Opção A da proposta)

Abandonar o Snapper, metadados em JSON, e mover `/boot` para dentro do
subvolume BTRFS (ESP fica só com o binário do bootloader).

- **(+)** Rollback **verdadeiramente atômico**: kernel, initramfs, módulos
  e sistema revertem juntos no swap. `sync_fat32` deixa de existir.
- **(+)** Sem cópia de kernel no save (sem inchaço da ESP); kernels vivem
  nos snapshots e são deduplicados por CoW.
- **(+)** Boot-first continua possível: uma entrada Limine por snapshot com
  `subvol=` apontando ao snapshot, que já carrega o próprio `/boot`. A
  ponte é `limine-entry-tool` (snapper-independente).
- **(−)** Exige bootloader que leia BTRFS (Limine com driver — já é o caso
  aqui) e o **script de migração FAT32→BTRFS** (`standalone-btrfs.md` §4),
  que é uma operação única, disruptiva e arriscada.
- **(−)** Reescreve toda a gestão de snapshot que hoje é do Snapper.

### Caminho 3 — Standalone + FAT32 com staging próprio (Opção B)

Abandonar o Snapper **e** reimplementar, sozinho, o staging por-snapshot na
ESP (o que o `limine-snapper-sync` faz), via `limine-entry-tool`.

- **(+)** Evita o bug (staging no save, não reconstrução no restore);
  mantém boot rápido/desencriptado e compat Secure Boot.
- **(−)** **Pior custo/benefício:** a maior quantidade de código frágil sob
  responsabilidade exclusiva, mais inchaço de ESP (300 MB+ por kernel com
  Nvidia/DKMS), gestão de limite de entradas.
- **Veredito:** só se justifica se Secure Boot / compatibilidade obrigarem
  a manter `/boot` em FAT32. Caso contrário, evitar.

## Critério de decisão

A escolha Wrapper-vs-Standalone reduz a **uma pergunta**:

> **Você quer ser dono do boot?**

- **Não** → Caminho 1. Fica wrapper, delega boot ao `limine-snapper-sync`,
  `snapgroup` enxuto focado no agrupamento.
- **Sim** → o jeito certo de ser dono do boot é o Caminho 2 (`/boot` em
  BTRFS, atômico). O gargalo real vira o script de migração, não o resto.

O Caminho 3 (standalone reinventando staging FAT32) é o meio-termo a
**evitar**: paga o custo de owner sem o benefício da atomicidade.

Argumentos historicamente usados pró-standalone (performance,
indireção XML/D-Bus) são fracos — o Snapper roda em milissegundos
(julgado no postmortem de 01/05 §4). O argumento **forte** para standalone
é outro: **boot bem-feito (Opção A) + semântica de grupo limpa em JSON**,
não velocidade.

## Recomendação

1. **Curto prazo (já feito):** manter o fail-safe da branch
   `fix/boot-sync-recovery` como stopgap, independente do rumo.
2. **Decisão de rumo:** responder "dono do boot?". Se sim, comprometer-se
   com o Caminho 2 e tratar a viabilidade do script de migração como o
   próximo bloqueador a estudar. Se não, executar o Caminho 1 (remover
   `sync_fat32`, delegar) e encerrar a ambição standalone.
3. **Não** seguir o Caminho 3.

## Pendências que resolvem a incerteza

- **Viabilidade/segurança da migração FAT32→BTRFS** (decide se o Caminho 2
  é realista). Prova de conceito: instalar Limine com driver BTRFS, mover
  kernels para dentro de `@`, validar boot e um rollback atômico de
  ponta a ponta.
- **Secure Boot é requisito?** Se sim, pesa contra `/boot` em BTRFS e
  reabre o Caminho 1/3.
- **Confirmar** que `limine-entry-tool --add-kernel` cobre o staging
  por-snapshot necessário (nome de entrada, `subvol=` no cmdline, hash)
  sem precisar do `limine-snapper-sync` — validação de uma chamada real.

## Status

Não decidido. Esta ADR fixa o quadro e o critério; a decisão fica para
quando a pergunta "dono do boot?" e a viabilidade da migração estiverem
respondidas.
