# Pós-mortem: Restore para checkpoint antigo + /boot FAT32 = Emergency Mode (2026-05-28)

Segundo incidente da mesma classe do de 2026-05-01 (`docs/incidents/
2026-05-01-limine-fat32-rollback/`): dessincronização entre `/boot` (FAT32)
e o root BTRFS após um `snapg restore`, resultando em **Emergency Mode** no
boot seguinte. Diferente do primeiro incidente — em que o sync *corrompia*
o `/boot` (hash do Limine, `limine_history/`) — aqui o sync **não chegou a
trocar o kernel**, e o snapg **reiniciou mesmo assim** para um `/boot`
descasado.

## 1. Ambiente

- **OS:** CachyOS (Arch). Bootloader Limine, `/boot` em **FAT32 (vfat)**.
- **Kernel rodando/instalado em 28/05:** `7.0.10-1-cachyos`.
- **Checkpoint restaurado:** grupo `1777814741` ("Certo", 2026-05-03 09:25:41).
  - Snapshot root do grupo: `#401`.
  - Módulos contidos no `#401`: **apenas `7.0.3-1-cachyos`**.

## 2. Timeline da Falha

1. `13:14:43` — usuário roda `snapg save Atual`.
2. `13:14:53` — usuário roda `snapg restore` e seleciona o checkpoint
   `1777814741` (root → snapshot `#401`, módulos `7.0.3`).
3. O rollback BTRFS (rename-swap) aplica: `@` passa a ser cópia writable do
   `#401`. `/usr/lib/modules/` agora só tem `7.0.3-1-cachyos`.
4. `boot::sync_fat32` deveria copiar o `vmlinuz` 7.0.3 do snapshot para
   `/boot` e regenerar o initramfs — deixando `/boot` coerente em 7.0.3.
   **Não completou** (ver §3).
5. `~13:15:07` — `snapg` é morto por SIGKILL durante o reboot
   (`systemctl reboot -i`). Janela total restore→reboot: **~14 segundos**.
6. `13:15:29` — boot sobe com `vmlinuz-7.0.10` (FAT32 não foi trocado),
   mas o root tem só módulos `7.0.3`.
7. Kernel 7.0.10 procura `/usr/lib/modules/7.0.10-1-cachyos` → inexistente
   → `modprobe vfat` falha → `mount: /boot: tipo de sistema de arquivos
   desconhecido "vfat"` → `boot.mount` falha → `local-fs.target` falha →
   **Emergency Mode** (repetido em nova tentativa às `13:19:42`).
8. Recuperação: o usuário rodou `snapg restore` várias vezes (13:37, 13:38,
   14:03) até o sistema voltar a um estado coerente (root `@` + `/boot`
   ambos em 7.0.10). Sistema atual está consistente e bootável.

## 3. Causa Raiz

**Causa imediata.** O `/boot` (FAT32) ficou com o kernel 7.0.10 enquanto o
root BTRFS voltou para o snapshot de 03/05 (módulos 7.0.3). O kernel não
encontra seus próprios módulos e **nenhum** módulo de filesystem carrega —
`vfat` é só o primeiro a aparecer no log. Isso derruba `local-fs.target`.

A causa imediata da imediata: o `sync_fat32` **não trocou o vmlinuz** antes
do reboot. Evidência:

- O boot subiu reportando `Linux version 7.0.10-1-cachyos` — ou seja, o
  `/boot` ainda tinha o kernel novo, não o 7.0.3 do snapshot.
- A janela restore→reboot foi de ~14 s, incluindo navegação na TUI e duas
  confirmações. O `sync_fat32` faz backup (~130 MB para FAT32) + cópia do
  vmlinuz + `mkinitcpio` (imagem de ~130 MB). Não cabe em 14 s.
- Reprodução isolada: `mkinitcpio --nopost -c <snap>/etc/mkinitcpio.conf
  -k 7.0.3-1-cachyos -r <snap> -g /tmp/test.img` a partir do `#401`
  **roda e gera imagem válida (exit 0)**. Logo, o sync *teria* funcionado
  se rodasse até o fim. Ele foi interrompido ou abortou cedo, e o
  `restore_backup()` devolveu o kernel 7.0.10 para o `/boot`.

**Causa arquitetural.** Igual à do incidente de 01/05 (§4 daquele
pós-mortem): com `/boot` em FAT32, kernel e initramfs vivem **fora** do
snapshot BTRFS. O swap de subvolume é atômico; o `/boot` não é. Restaurar
exige *reconstruir* o boot, e qualquer falha ou interrupção deixa `/boot`
descasado do root. A correção definitiva (Opção A — `/boot` dentro do
BTRFS) já foi recomendada e segue não implementada.

### Bugs de fluxo de controle que transformaram o descasamento em brick

Os fixes anteriores (`96c065c`, `2c579bc`, `992e04e`) trataram a
*corretude* do sync (não corromper Limine, multi-kernel, backup/restore).
Nunca trataram a *atomicidade / gate de reboot*:

1. **Falha de sync era apenas um aviso.** Em `commands.rs`
   (`execute_restore_checkpoint` e `execute_restore_regret`), um `Err` de
   `boot::sync_fat32` gerava `eprintln!` e o controle **caía direto em
   `prompt_reboot()`**. O snapg *sabia* que o `/boot` podia estar
   dessincronizado e ainda assim oferecia o reboot.

2. **`restore_backup()` piora este cenário específico.** Pensado para
   proteger a integridade do `/boot` (revertê-lo ao estado pré-sync se o
   sync corromper algo), ele **devolve o kernel NOVO (7.0.10) ao `/boot`**.
   Quando o root foi revertido para um snapshot antigo, isso *garante* o
   mismatch.

3. **`prompt_reboot` usa `systemctl reboot -i`** — ignora inhibitors. Nada
   segura o reboot para o estado quebrado.

4. **Não havia verificação pós-sync.** Nada confirmava que o kernel agora
   em `/boot` tem módulos correspondentes no root restaurado antes de
   liberar o reboot.

## 4. Fixes Aplicados (commit nesta branch `fix/boot-sync-recovery`)

### Fix 1 — Gate de verificação pós-sync (`boot.rs` `verify_synced`)

Após o `sync_inner`, o `sync_fat32` agora compara **byte a byte** cada
`vmlinuz` ativo em `/boot` com o `vmlinuz` do kver correspondente no
snapshot restaurado (`/usr/lib/modules/<kver>/vmlinuz`). Se divergir,
`bail!` — o sync passa a falhar explicitamente quando o `/boot` não casa
com o root, em vez de retornar sucesso para um estado inconsistente.
`sync_fat32` virou auto-verificante: `sync_inner(...).and_then(verify_synced)`.

### Fix 2 — Falha de sync bloqueia o reboot (`commands.rs`)

Tanto em `execute_restore_checkpoint` quanto em `execute_restore_regret`,
um `Err` de `boot::sync_fat32` agora retorna via
`abort_reboot_boot_desync(...)`: imprime o diagnóstico, declara que
reiniciar cai em Emergency Mode, **não chama `prompt_reboot()`**, e dá a
instrução de recuperação específica do caminho (restaurar o Regret no
caso do checkpoint; checagem manual no caso do regret).

O `restore_backup()` foi mantido: ele mantém o `/boot` bootável para o
sistema **vivo atual** — coerente com a recuperação recomendada (restaurar
o Regret traz o root de volta ao estado atual, que casa com esse `/boot`).

## 5. Status dos Fixes

| Fix | Local | Validado em campo? |
|---|---|---|
| 1 — Gate `verify_synced` | `boot.rs` `verify_synced` + `sync_fat32` | **Não** |
| 2 — Bloqueio de reboot em falha de sync | `commands.rs` `abort_reboot_boot_desync` + ambos os caminhos de restore | **Não** |

`cargo build`, `cargo clippy` e `cargo test` (8 testes) passam. **Pendente:**
reexecutar um `snapg restore` real para um checkpoint com kernel mais antigo
que o instalado, com `/boot` em FAT32, e confirmar que (a) o sync completo
deixa o boot coerente, e (b) um sync falho/interrompido bloqueia o reboot em
vez de brickar.

## 6. Lições

- **Operação não-atômica antes de um reboot precisa de gate, não de aviso.**
  Um `eprintln!` seguido de `prompt_reboot()` é o mesmo que não ter aviso:
  o usuário confirma o reboot no fluxo normal e bricka.
- **"Recovery" que restaura o estado errado é pior que não ter recovery.**
  O `restore_backup` devolve o kernel novo ao `/boot`; isoladamente é
  correto, mas sem bloquear o reboot ele garante o mismatch.
- **Verifique o invariante, não o caminho feliz.** Comparar o `vmlinuz` de
  `/boot` com o do snapshot é barato (~16 MB) e fecha tanto o caso de sync
  parcial quanto o de sync que "achou" que terminou.
- **A causa raiz continua sendo FAT32.** Enquanto `/boot` viver fora do
  BTRFS, o rollback nunca será atômico de ponta a ponta. Opção A
  (`/boot` em BTRFS) elimina toda esta classe.

## 7. Artefatos

- `boot-emergency.log` — journal do boot que caiu em Emergency Mode
  (duas tentativas: `13:15:29` e `13:19:42`), com as falhas
  `mount: /boot: ... "vfat"` → `local-fs.target` → `Emergency Mode`.
