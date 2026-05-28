# ADR: Arquitetura de Boot e Gestão de Snapshot (dois eixos)

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
vmlinuz + `mkinitcpio`). Reconstrução não-atômica e frágil; falha ou
interrupção deixa o kernel em `/boot` descasado dos módulos do root →
nenhum módulo carrega (`unknown filesystem type vfat`) → Emergency Mode.

O fix da branch `fix/boot-sync-recovery` (gate `verify_synced` +
`abort_reboot_boot_desync`) fecha o **sintoma** (bloqueia o reboot quando
o sync falha), não a **causa**.

Em paralelo, há a ambição registrada em `standalone-btrfs.md` de abandonar
o wrapper do Snapper e gerir BTRFS direto, com metadados em JSON.

## A correção de enquadramento: são DOIS eixos independentes

Uma versão anterior desta ADR amarrou "standalone" com "`/boot` em BTRFS"
como se fossem a mesma decisão. **Não são.** São eixos ortogonais:

- **Eixo 1 — Gestão de snapshot:** wrapper do Snapper **vs** standalone
  (BTRFS direto + JSON).
- **Eixo 2 — Arquitetura de boot:** `/boot` em FAT32 **vs** `/boot` em
  BTRFS.

**Os dois incidentes são 100% do Eixo 2.** Não têm relação com o Snapper.
Dá para continuar wrapper do Snapper (mantendo `btrfs-assistant` /
`snapper-gui`), não depender do `limine-snapper-sync`, e mesmo assim
mover `/boot` para dentro do BTRFS — resolvendo a causa-raiz de boot sem
tocar no Eixo 1. As duas decisões devem ser tomadas separadamente.

---

## Eixo 2 — Arquitetura de boot

### B2.1 — FAT32, reconstruindo no restore (estado atual)

`boot::sync_fat32`: no restore, copia o vmlinuz do snapshot para a ESP e
**regenera o initramfs com `mkinitcpio`**, depois reinjeta hashes no
`limine.conf`.

- **(−)** Reconstrução não-atômica entre dois filesystems sem commit
  comum. Janela de inconsistência entre o swap e o fim do sync. **Causa
  dos dois incidentes.**
- **(−)** `mkinitcpio` no caminho de restore: lento (~130 MB), muitas
  superfícies de falha (hooks, firmware, interrupção/SIGKILL/queda de luz).
- **(+)** Compat "out-of-the-box", Secure Boot padrão, sem migração.

### B2.2 — FAT32, com staging no save (modelo do `limine-snapper-sync`)

Copia kernel+initramfs de cada snapshot para a ESP **na hora do save**
(estado saudável), com hash, e gera entrada Limine por snapshot. Rollback
vira **boot-first**: boota a entrada do snapshot (kernel já casa com os
módulos), confirma que sobe, e só então commita.

- **(+)** Sem reconstrução no restore → some a classe de bug.
- **(+)** Boot-first: valida antes de commitar.
- **(−)** Inchaço da ESP (300 MB+ por kernel com Nvidia/DKMS); gestão de
  limite de entradas; FAT32 grande (4–32 GiB).
- **Implementação:** delegar ao `limine-snapper-sync` (acopla ao Snapper,
  ver Eixo 1) **ou** chamar a primitiva snapper-independente
  `limine-entry-tool --add-kernel <nome> <initramfs> <vmlinuz>`.

### B2.3 — `/boot` em BTRFS (Opção A da proposta) — *recomendado*

Kernel e initramfs vivem dentro de `@`. A ESP só guarda o binário do
bootloader (Limine com driver BTRFS — já é o caso aqui).

- **(+)** Rollback **verdadeiramente atômico**: kernel, initramfs, módulos
  e sistema revertem juntos no swap. **`boot::sync_fat32` deixa de
  existir** — some a parte frágil; sem `mkinitcpio` no restore, sem cópia
  cross-filesystem.
- **(+)** Sem inchaço de ESP: kernels vivem nos snapshots, deduplicados
  por CoW.
- **(o)** Resíduo: se o Limine verifica BLAKE2B e o conteúdo de
  `@/boot/vmlinuz` muda no swap, o hash precisa de refresh — operação
  pequena e confiável que o snapgroup **já faz** (`refresh_limine_boot_hashes`).
- **(o)** Boot-first opcional: uma entrada Limine por snapshot com
  `subvol=` (via `limine-entry-tool`); no modelo restore-then-reboot do
  snapgroup pode nem ser necessário.
- **(−)** Exige bootloader que leia BTRFS e o **script de migração
  FAT32→BTRFS** (`standalone-btrfs.md` §4): operação única, disruptiva,
  arriscada.
- **(−)** Secure Boot padrão fica mais complicado (kernel fora da ESP).

**Recomendação do Eixo 2:** B2.3 (`/boot` em BTRFS). É a única opção em que
o rollback é atômico de ponta a ponta e a lógica de boot do snapgroup
encolhe a quase nada. Manter B2.1 só enquanto a migração não acontecer
(com o fail-safe da branch atual). B2.2 só se Secure Boot / compat
obrigarem a manter FAT32.

> **B2.1 validado em campo (2026-05-28):** com os fixes da branch
> `fix/boot-sync-recovery`, um `snapg restore` cross-version real (7.0.3 vs
> 7.0.10, FAT32 + Limine) bootou limpo até `Graphical Interface`, sem
> Emergency Mode. B2.1 está hoje *funcional e seguro* no caminho feliz — a
> recomendação por B2.3 é por **atomicidade**, não por B2.1 estar quebrado.
> Ver `docs/incidents/2026-05-28-fat32-restore-emergency/postmortem.md` §5.

---

## Eixo 1 — Gestão de snapshot

### A1.1 — Wrapper do Snapper (estado atual) — *recomendado*

`snapgroup` lê snapshots/configs do Snapper, tagueia o par `@`+`@home`
com `snapgroup-id` e faz o swap pareado.

- **(+)** Snapper é projeto openSUSE maduro e estável; manutenção barata.
- **(+)** Compat de ecossistema **de graça**: `btrfs-assistant`,
  `snapper-gui`. (Útil hoje, declarado pelo dono do projeto.)
- **(+)** `pacman` hook, snapshots por evento/tempo e `explore` são
  adicionáveis **sem** abandonar o Snapper.
- **(o)** O agrupamento root+home continua sendo responsabilidade do
  `snapgroup` — o Snapper trata configs como universos independentes.

### A1.2 — Standalone (BTRFS direto + JSON)

`snapgroup` gere snapshots nativamente (`/.snapshots_snapg/` + `info.json`).

- **(+)** Controle total de metadados; semântica de grupo limpa em JSON.
- **(−)** Reescreve toda a gestão que o Snapper já faz; perde
  `btrfs-assistant` / `snapper-gui` e qualquer tooling Snapper-aware.
- **(−)** Argumentos de performance/indireção XML·D-Bus são **fracos** —
  Snapper roda em milissegundos (julgado no postmortem de 01/05 §4).

**Recomendação do Eixo 1:** A1.1 (continuar wrapper). O standalone não
paga o custo da reescrita; os ganhos reais (grupo, JSON, hooks) ou já
existem ou são adicionáveis como wrapper. **Esta decisão é independente do
boot** e não precisa ser tomada para resolver os incidentes.

---

## Ecossistema Limine (referência)

- **`limine-snapper-sync`** (Zesko, terceiro, **GPL-3**, **não** faz parte
  do Snapper oficial): camada *snapper-aware* que faz o staging do Eixo
  B2.2. **Depende do Snapper** (lê `/.snapshots`). Útil só no par
  wrapper + FAT32-staging.
- **`limine-entry-tool`** (instalado via `limine-mkinitcpio-hook`):
  primitiva **snapper-independente** de gestão de entradas Limine.
  `--add-kernel <nome> <initramfs> <vmlinuz>` copia para a ESP, calcula
  hash (`ENABLE_VERIFICATION=yes`) e cria a entrada. É o ponto de
  integração para staging/boot-first em **qualquer** combinação de eixos.
- **Licença:** `limine-snapper-sync` é GPL-3; `snapgroup` é MIT. Estudar a
  abordagem é livre; **copiar código** GPL-3 para projeto MIT contamina a
  licença. Tratar como referência de design, não fonte.

---

## Combinações e recomendação consolidada

| Eixo 1 \ Eixo 2 | B2.1 FAT32-reconstrói | B2.2 FAT32-staging | B2.3 /boot BTRFS |
|---|---|---|---|
| **A1.1 Wrapper** | estado atual (frágil) | delega ao snapper-sync **ou** entry-tool | **recomendado** |
| **A1.2 Standalone** | (não fazer) | via entry-tool (muito código próprio) | atômico, mas exige migração + reescrita |

**Recomendação consolidada: A1.1 + B2.3** — continuar wrapper do Snapper
(mantém `btrfs-assistant`, manutenção barata, sem depender do
`limine-snapper-sync`) **e** mover `/boot` para dentro do BTRFS (mata os
dois incidentes, elimina o `sync_fat32`). As duas pernas são
independentes: a do Eixo 1 pode ficar como está hoje; a do Eixo 2 é a que
destrava o boot.

## Pendências que destravam a decisão

- **(Eixo 2, bloqueador real)** Viabilidade/segurança da migração
  FAT32→BTRFS. PoC: Limine com driver BTRFS, kernels dentro de `@`,
  validar boot e um rollback atômico de ponta a ponta.
- **(Eixo 2)** Como o `limine.conf` referencia o kernel com `/boot` em
  BTRFS (`subvol=@/boot/...`) e se o hash BLAKE2B precisa de refresh
  pós-swap — decide se sobra alguma lógica de boot ou zero.
- **(Eixo 2)** Secure Boot é requisito? Se sim, pesa contra B2.3 e reabre
  B2.2.
- **(Eixo 1)** Confirmar que `pacman` hook / `explore` cobrem a vontade do
  projeto como wrapper, encerrando a tentação do standalone.

## Status

Não decidido. A ADR fixa os dois eixos, suas opções e a recomendação
(A1.1 + B2.3). O bloqueador concreto é a PoC de migração `/boot`→BTRFS
(Eixo 2); o Eixo 1 pode permanecer como está sem custo.
