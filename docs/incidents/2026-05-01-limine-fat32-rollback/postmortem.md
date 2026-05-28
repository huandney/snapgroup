# Pós-mortem: Rollback BTRFS + FAT32 + Limine (2026-05-01)

Pós-mortem consolidado do incidente em que um `snapg restore` num sistema
CachyOS com `/boot` em FAT32 e bootloader Limine resultou em corrupção do
histórico do Limine e Kernel Panic no boot seguinte. O incidente motivou os
três fixes implementados no commit `96c065c "Fix legacy FAT32 boot sync"`.

## 1. Ambiente

- **OS:** Arch Linux / CachyOS.
- **Bootloader:** Limine (com `limine-snapper-sync` e validação BLAKE2B).
- **Partições:**
  - `/boot` (ou `/efi` montado em `/boot`): **FAT32** — contém `vmlinuz` e
    `initramfs`.
  - `/` (Raiz): **BTRFS** — `/usr/lib/modules/<kver>/` com drivers
    (incl. Nvidia, ZFS).

## 2. Timeline da Falha

1. Sistema atualizado via `pacman -Syu` (Kernel 7.0.1 → 7.0.3, Nvidia v2
   instalados). `vmlinuz-v2` foi escrito em `/boot` (FAT32). Módulos
   `nvidia.ko` v2 escritos no BTRFS.
2. Usuário executou `snapg restore` para voltar ao Checkpoint anterior à
   atualização (Kernel 7.0.1).
3. O snapg fez o rollback BTRFS corretamente (rename-swap dos subvolumes
   funcionou), mas a sincronização de `/boot` apresentou três falhas
   simultâneas (ver §3).
4. Usuário recorreu a `limine snapper restore` como ferramenta externa de
   recuperação. Esse comando **também resultou em Kernel Panic do Limine**
   no boot seguinte — porque o snapg já havia corrompido os backups de
   `limine_history/`, e o `limine snapper restore` apenas copiou de volta
   os artefatos errados.
5. Recuperação final exigiu restaurar a partição FAT32 manualmente via
   Acronis.

## 3. Causa Raiz

A causa raiz é a **dessincronização de estado entre FAT32 e BTRFS**. O
rollback do BTRFS é atômico (rename de subvolumes, metadata-only), mas
não toca em FAT32. O `vmlinuz` continuou apontando para o kernel novo
enquanto `/usr/lib/modules/` voltou ao kernel antigo — kernel panic em
cascata por mismatch de módulos.

O `snapg` tentou mitigar isto com `boot::sync_fat32`, mas três bugs na
implementação anterior pioraram a situação em vez de corrigi-la:

### Bug 1 — Hash BLAKE2B do Limine inválido após cópia

**Sintoma:**
```
PANIC: Blake2b hash for URI
  'boot():/<machine-id>/linux-cachyos/vmlinuz-linux-cachyos'
  does not match!
```

**Causa:** O `limine-snapper-sync` grava em `/boot/limine.conf` um hash
criptográfico junto ao caminho do kernel
(`path: boot():/.../vmlinuz-linux-cachyos#0def202e745...`). Quando o snapg
copia o kernel antigo por cima do novo, o conteúdo do arquivo muda, mas o
hash gravado continua o do conteúdo anterior. No boot, o Limine recalcula
e detecta o mismatch — para por panic em vez de carregar código não
verificado.

**Fix aplicado (`boot.rs:347` `refresh_limine_boot_hashes`):** após
qualquer cópia para `/boot`, o snapg faz parsing do `limine.conf`,
recalcula o BLAKE2B dos arquivos referenciados (via `b2sum`) e re-injeta
o hash atualizado nas chaves `path:`, `kernel_path:`, `module_path:` e
`image_path:`. Escrita atômica via `rename` de arquivo temporário.

> **Nota:** O plano original (`REPORT_FOR_CODEX.md`) sugeria simplesmente
> raspar o `#hash` do `limine.conf` e deixar o Limine sem validação. O fix
> implementado vai além — recomputa o hash correto — preservando a
> verificação criptográfica no boot.

### Bug 2 — Varredura recursiva corrompeu `limine_history/`

**Sintoma (log do snapg):**
```
boot sync: vmlinuz copiado → /boot/<id>/linux-cachyos/vmlinuz-linux-cachyos
boot sync: vmlinuz copiado → /boot/<id>/limine_history/vmlinuz-linux-cachyos_sha256_48cfae...
boot sync: vmlinuz copiado → /boot/<id>/limine_history/vmlinuz-linux-cachyos_sha256_a17539...
```

**Causa:** `scan_boot_dir` descia recursivamente para encontrar `vmlinuz*`
e `initramfs*` em layouts BLS. Sem skip explícito, entrou em
`/boot/<machine-id>/limine_history/` — pasta onde o `limine-snapper-sync`
guarda backups históricos do bootloader nomeados com sufixo `_sha256_*` —
e sobrescreveu cada backup com o kernel restaurado. A redundância
histórica do Limine foi destruída.

**Consequência colateral:** quando o usuário rodou `limine snapper restore`
como ferramenta externa de recuperação, ela copiou um arquivo de
`limine_history/` de volta para o caminho ativo. Como o histórico já
estava corrompido pelo snapg, o `limine snapper restore` repôs lixo —
explicando o panic mesmo após o "restore externo".

**Fix aplicado (`boot.rs:250` `is_ignored_boot_dir`):** lista explícita de
diretórios ignorados durante a varredura: `limine_history` e
`.snapg_boot_backup` (o próprio backup do snapg).

### Bug 3 — `mkinitcpio` lendo configuração do live system

**Sintoma (log do snapg):**
```
⚠ sincronização do boot falhou:
  nenhum preset .preset encontrado em /etc/mkinitcpio.d
```

**Causa:** `find_mkinitcpio_preset` lia `/etc/mkinitcpio.d` da raiz
**viva**. Durante o rollback, a raiz já tinha sofrido rename-swap de
subvolumes — `/etc/mkinitcpio.d` apontava para um estado volátil em que
o `pacman` tinha acabado de mexer instantes antes. A leitura retornava
vazio.

**Fix aplicado (`boot.rs:324` `find_mkinitcpio_preset`):** a função recebe
agora `restored_root: &Path` e lê de `restored_root/etc/mkinitcpio.d/`.
A geração do initramfs (`regen_initramfs`) também usa
`restored_root/etc/mkinitcpio.conf` como config e passa `-r restored_root`
para o `mkinitcpio` — garantindo que módulos, hooks e config venham todos
do snapshot restaurado, não do live system.

## 4. Discussão Arquitetural — Wrapper vs Standalone, Opção A vs B

O incidente reabriu uma discussão arquitetural maior sobre o futuro do
projeto (`REPORT_FOR_CLAUDE.md` original). Resumo das opções consideradas:

### Wrapper Snapper (modo atual) vs Standalone BTRFS

A proposta `standalone-btrfs.md` (ver `docs/proposals/`) sugere abandonar
o Snapper e chamar `btrfs-progs` diretamente, ganhando controle total de
metadata (JSON) e desbloqueando features (`pacman` hook, `snapg explore`,
auto-snapshots).

**Decisão (corrente):** continuar como wrapper Snapper. Os argumentos
"performance" e "indireção XML/D-Bus" são fracos na prática — Snapper
roda em milissegundos. O custo real do pivot é perder compatibilidade
com `btrfs-assistant`, `snapper-gui` e `limine-snapper-sync`. As features
do draft (`pacman` hook, `explore`) são adicionáveis ao wrapper sem
reescrever o core.

### Opção A (`/boot` em BTRFS) vs Opção B (FAT32 mantido)

Este incidente é específico da **Opção B** (modo legado FAT32). A
**Opção A** — migrar `/boot` para dentro do BTRFS, deixar FAT32 só com
o binário `.efi` — eliminaria os três bugs de uma vez, porque o snapg
nunca tocaria FAT32 durante o rollback: kernel e initramfs viveriam
dentro do snapshot e seriam revertidos atomicamente junto com o resto.

**Estado:** os fixes do commit `96c065c` resolvem a Opção B para sistemas
que não querem ou não podem migrar. A Opção A continua sendo a
arquitetura recomendada para novas instalações — discussão para um ADR
futuro, não bloqueante.

## 5. Status dos Fixes

| Fix | Local | Validado em campo? |
|---|---|---|
| 1 — Hash BLAKE2B do Limine | `boot.rs` `refresh_limine_boot_hashes` + testes unitários do parser | **Sim** (2026-05-28) |
| 2 — Skip de `limine_history` | `boot.rs` `is_ignored_boot_dir` | **Sim** (indireto — sem corrupção no run de 28/05) |
| 3 — `mkinitcpio` lendo do `restored_root` | `boot.rs` `regen_initramfs` (passa `-r`/`-c` do restored_root) | **Sim** (2026-05-28) |

**Validado (2026-05-28):** um `snapg restore` real com kernel mismatch
(snapshot 7.0.3 vs sistema 7.0.10), `/boot` em FAT32 + Limine, **bootou
limpo** em 7.0.3 até `Graphical Interface` — sem panic de hash BLAKE2B
(Fix 1), com initramfs regenerado a partir do `restored_root` (Fix 3), e
sem corromper `limine_history/` (Fix 2). Detalhes e evidência em
`docs/incidents/2026-05-28-fat32-restore-emergency/postmortem.md` §5.

> Nota: as referências de linha originais (`boot.rs:347`, `:250`, etc.)
> ficaram defasadas após os fixes posteriores da branch
> `fix/boot-sync-recovery`; os nomes de função permanecem válidos.

## 6. Lições

- **Walks recursivos em diretórios de sistema precisam de allowlist ou
  denylist explícito.** Generalizar para "qualquer `vmlinuz*` em
  qualquer subdir" foi uma decisão preguiçosa que abriu o Bug 2.
- **Durante rollback, o "estado da verdade" é o `restored_root`, não o
  live system.** Qualquer leitura de config (`mkinitcpio.conf`,
  presets, etc.) deve vir do snapshot restaurado.
- **Validação criptográfica downstream (Limine BLAKE2B) precisa ser
  re-honrada após qualquer escrita.** Raspar o hash é tecnicamente
  válido (Limine faz fallback), mas recalcular é a postura defensiva
  correta.
- **Ferramentas externas de recuperação podem amplificar a corrupção.**
  O `limine snapper restore` falhou porque confiou no histórico que o
  snapg já tinha estragado. Cuidado ao tocar em estado que outros
  utilitários assumem imutável.

## 7. Artefatos

Anexados a este diretório:

- `snapg-restore.log` — log da execução do `snapg restore` que falhou.
- `limine.conf` — snapshot do `/boot/limine.conf` no momento do panic.
- `system.log` — log do sistema (`pacman -Syu` que precedeu a falha).
- `reboot-inhibitor-notes.md` — notas do usuário sobre o inhibitor do
  reboot e tentativa subsequente que retornou
  `Erro de E/S (query default id failed, subvolume is not a btrfs
  subvolume)`. Este último erro ocorreu num estado intermediário
  inconsistente, pós-recuperação parcial.
