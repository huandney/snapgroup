# Proposta: pares de boot guardados no save (C′)

- **Status:** proposta, não implementada.
- **Data:** 2026-11-18.
- **Relacionado:** `docs/architecture/boot-and-standalone-decision.md` (B2.2),
  `docs/architecture/initramfs-stale-on-restore.md`,
  `docs/roadmap/refinamentos-pendentes-doctor.md`.

## 0. TL;DR

No save, o snapgroup guarda na ESP uma cópia do `vmlinuz` e do `initramfs` que
o `/boot` tem naquele momento, uma vez por kernel distinto. No restore de um
checkpoint que tem esse par, o sync do `/boot` deixa de rodar o `mkinitcpio`: ele
confere o par pelo hash, copia para o lugar e troca com `rename`. Checkpoints
sem par (antigos, ou que passaram do teto de espaço) continuam no caminho de
hoje, que reconstrói.

É a B2.2 do documento de decisão, feita pelo snapgroup em vez de delegada ao
`limine-snapper-sync`.

## 1. Por que não `/boot` dentro do Btrfs

O Limine lê só FAT e ISO9660 (`/usr/share/doc/limine/FAQ.md`, Limine 12.10:
"aside from the bare minimum necessities (eg: FAT*, ISO9660)"). Com ele, kernel e
initramfs precisam ficar numa partição FAT. Pôr o `/boot` dentro do Btrfs exige
trocar de bootloader (GRUB ou rEFInd com driver Btrfs) e migrar o `/boot`, que é
a operação mais arriscada que o snapgroup faria. Esta proposta ataca a mesma
causa sem nenhuma das duas coisas.

## 2. O que a investigação mostrou (2026-11-18, máquina do autor)

**O histórico do `limine-snapper-sync` não serve como fonte.**

| | |
|---|---|
| Checkpoints do snapgroup no root (fora da lixeira) | 40, em 12 kernels (7.0.1 a 7.3.4) |
| Snapshots com par em `limine_history` | 8 (`MAX_SNAPSHOT_ENTRIES=8`), os mais novos |
| Kernels desses 8 | só o atual (7.3.4) |

Restaurar um checkpoint do mesmo kernel já não roda o `mkinitcpio` hoje. Os
restores que reconstroem o initramfs, a parte frágil dos incidentes de 01/05,
28/05 e 02/06, são os que mudam de kernel, e esses são justamente os checkpoints
mais antigos, que não têm par no histórico. Além disso, o `limine-snapper-sync`
apaga do histórico o que suas entradas não usam mais: um restore que dependesse
desses arquivos poderia ficar sem eles.

O mapeamento dele (`limine_history/snapshots.json`, formato 1.3.0: número do
snapshot → `fileHashName` de cada arquivo) é confiável, mas não resolve a
cobertura.

**Tamanho.** Cada kernel distinto custa ~330 MB (initramfs de 311 MB, com
Nvidia/DKMS, e vmlinuz de 17 MB). A ESP tem 4 GiB, com 672 MiB em uso. Abaixo de
85% de uso cabem uns 8 pares a mais: cerca de dois meses de atualizações de
kernel no ritmo atual (7.3.0 → 7.3.4 entre 01/11 e 13/11).

**Tempo.** O hook `90-mkinitcpio-install` levou 13–16 s nas seis últimas
atualizações de kernel (pacman.log). No restore, o sync ainda soma o backup de
`/boot` (~330 MB) e a verificação.

## 3. Proposta

### 3.1 No save

1. Ler a entrada principal do `limine.conf` de cada kernel instalado
   (`kernel-id=`): os arquivos de `path:` e `module_path:` e o BLAKE2B
   registrado após o `#`.
2. Se o `boot-check` desses arquivos falhar (hash não confere ou arquivo
   ausente), não guardar nada para este checkpoint: um par que não bate não é
   um par bom. O checkpoint fica no caminho de reconstrução.
3. Para cada arquivo cujo hash ainda não está guardado, copiar para
   `/boot/<machine-id>/snapgroup/<nome>-<b2[..16]>` (temporário + `rename`).
   Na maioria dos saves nada é copiado: o hash vem do `limine.conf`, sem ler
   os 330 MB.
4. Registrar no manifesto (§3.4) que o grupo usa esses arquivos.

O par guardado é o que o `/boot` tinha no instante do save. Num checkpoint do
pacman (PreTransaction) é o par de antes da atualização, que é o que um restore
precisa.

### 3.2 No restore

Com par guardado para o checkpoint:

1. Conferir o BLAKE2B de cada arquivo guardado contra o manifesto. Um arquivo
   corrompido na ESP aparece aqui, antes de mexer em qualquer coisa.
2. Guardar o par atual do sistema, se ainda não estiver guardado. Ele serve de
   volta em caso de falha e substitui o backup de hoje (~330 MB copiados a cada
   restore).
3. Copiar cada arquivo para um temporário ao lado do destino, `fsync`, e trocar
   com `rename`.
4. Escrever os hashes no `limine.conf` (temporário + `rename`). Os valores são
   os do manifesto: não há o que recalcular.
5. Conferir com o `limine_hash_report` (o mesmo do `boot-check`).

Sem par: o sync de hoje, sem mudança.

Tudo sob o mutex do Limine (`/run/lock/boot-partition.lock`), como hoje.

### 3.3 Volta em caso de falha

Uma falha entre 3 e 5 repete o procedimento com o par atual guardado no passo 2.
Interrompido no meio (Ctrl-C, queda de energia), o `/boot` fica com arquivos
inteiros, do par antigo ou do novo, nunca pela metade: só o `rename` muda o que
o Limine lê. O `doctor` detecta o estado pelo hash e reroda o mesmo caminho, que
é idempotente.

### 3.4 Manifesto e retenção

- Manifesto em `/boot/<machine-id>/snapgroup/pairs.json`. Fica na ESP, fora dos
  snapshots, então um restore não o reverte. Conteúdo: arquivos (nome, kernel-id,
  BLAKE2B, tamanho) e, para cada grupo, os arquivos que ele usa.
- No membro root, `snapgroup-boot=<id do par>` só como sinal para o seletor do
  restore. A fonte da verdade é o manifesto.
- Um arquivo sai quando nenhum grupo o usa mais. Grupos na lixeira ainda contam,
  porque podem ser restaurados; a remoção acontece no purge da lixeira.
- Teto de uso da ESP (§5): passando dele, os pares dos checkpoints mais antigos
  saem primeiro, e esses checkpoints voltam ao caminho de reconstrução.

### 3.5 Telas

- Seletor do restore: marcar os checkpoints com boot pronto e os que vão
  reconstruir o initramfs (e quanto tempo isso leva).
- `doctor`: linha informativa com quantos pares estão guardados e o espaço que
  ocupam.

## 4. Segurança e desempenho, comparados com hoje

| | Reconstrução (hoje) | Par guardado |
|---|---|---|
| Escrita dos arquivos | temporário + `rename` | temporário + `rename` |
| Duração da janela com root e `/boot` descasados | backup + `mkinitcpio` (13–16 s) + hashes + verificação | conferência + cópia de ~330 MB (alguns segundos) + hashes |
| Pontos de falha | hooks do mkinitcpio, DKMS, firmware, compressão, espaço | espaço, I/O |
| Dá para saber se o resultado está certo? | só o `vmlinuz` (o initramfs regenerado não tem referência) | sim: os dois arquivos têm hash conhecido antes de começar |
| Conteúdo do initramfs | gerado agora, com o `mkinitcpio.conf` do snapshot | o que bootou naquele dia |
| Backup prévio do `/boot` | cópia de ~330 MB a cada restore | o par atual guardado (cópia só se ainda não houver) |

As duas linhas que mais pesam:

- **"Dá para saber se está certo"** fecha o gap descrito em
  `initramfs-stale-on-restore.md`.
- **"Pontos de falha"** tira do caminho crítico o programa que falhou nos
  incidentes.

## 5. Perguntas em aberto

1. **Teto de espaço.** `BOOT_STAGE_LIMIT_PERCENT` próprio ou o mesmo 85% do
   `limine-snapper-sync`? Os dois disputam a mesma ESP: o que o snapgroup ocupa
   reduz o que o `limine-snapper-sync` aceita guardar antes de recusar entradas.
2. **Dois kernels instalados** (`linux-cachyos` e `-lts`): guardar os dois pares
   por checkpoint dobra o custo. Guardar só o kernel padrão (`default_entry`)?
3. **Primeiro save depois de uma atualização de kernel.** O par novo ainda não
   bootou. Guardar assim mesmo (o `boot-check` garante os hashes) ou só depois
   do primeiro boot com ele?
4. **Checkpoints anteriores à feature.** Ficam no caminho de reconstrução. Vale
   um comando para guardar o par do kernel atual nos checkpoints desse kernel?
   Para kernels antigos não há de onde copiar.
5. **Entradas de fallback** (`initramfs-*-fallback`): o CachyOS com
   `limine-mkinitcpio-hook` não gera hoje; tratar quando o `limine.conf` as
   citar.

## 6. Fases

1. **Guardar no save** e manifesto, com a retenção. Sem mudar o restore. Permite
   medir espaço e cobertura reais por algumas semanas.
2. **Restore com par guardado**, com a volta do §3.3 e a conferência final.
3. **Telas**: marcação no seletor do restore e linha no `doctor`.

## 7. Fora do escopo

- Trocar de bootloader ou pôr o `/boot` dentro do Btrfs (documento de decisão,
  B2.3).
- Mudar o `limine-snapper-sync`, o histórico dele ou as entradas de snapshot que
  ele gera no menu.
- Secure Boot: os arquivos ficam na mesma ESP e com os mesmos hashes que o
  `limine.conf` já registra.
