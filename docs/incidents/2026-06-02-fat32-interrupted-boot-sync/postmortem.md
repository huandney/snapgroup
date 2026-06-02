# Pós-mortem: recuperação via `doctor` sincronizou `/boot` para o root errado (2026-06-02)

Terceiro incidente da mesma classe (ver `2026-05-01-limine-fat32-rollback/`
e `2026-05-28-fat32-restore-emergency/`): dessincronização entre `/boot`
(FAT32) e o root BTRFS após `snapg restore`, resultando em **Emergency Mode**.

A novidade deste incidente **não** é o desync inicial (já coberto pelo
`boot-emergency.log` da manhã e pelo Patch A de detecção de interrupção). É o
que aconteceu na **recuperação**: o `snapg doctor`, rodado de dentro de um
snapshot de resgate, sincronizou o `/boot` para o subvolume que estava montado
em `/` — **não** para o `/@` que boota por padrão — e com isso *afastou* ainda
mais o `/boot` do root real, mantendo o sistema em Emergency Mode.

Artefatos:

- `boot-emergency.log` — journal do Emergency Mode da manhã (~01:26).
- `recovery-emergency-journal.log` — journal do Emergency Mode da tarde
  (~15:09), depois da recuperação malsucedida.

## 1. Ambiente

- **OS:** CachyOS (Arch). Bootloader Limine, `/boot` em **FAT32 (vfat)**,
  partição `/dev/nvme0n1p1`, fora do snapshot.
- **Root:** BTRFS em `/dev/nvme0n1p4`, subvol padrão `/@`.
- **Kernel instalado/atual:** `7.0.10-2-cachyos` (vmlinuz `e1766f22…`).
- **Checkpoint restaurado:** grupo `1777814741` ("Certo", 2026-05-03), cujo
  root contém **apenas** módulos `7.0.3-1-cachyos` (vmlinuz `0def202e…`).
- **machine-id em `/boot`:** `0262533c4ca04359a5f379b8e3f83042`.

## 2. Timeline

1. Usuário roda `snapg restore` para o checkpoint "Certo" (7.0.3-1). O
   rollback BTRFS comita: `/@` passa a ser o root antigo (módulos 7.0.3-1).
2. O `boot::sync_fat32` pós-rollback **é interrompido com Ctrl+C** (teste
   deliberado). O `/boot` **não** chega a ser sincronizado para 7.0.3-1.
3. Reboot → o boot padrão (`subvol=/@`) carrega o kernel de `/boot`
   (ainda 7.0.10-2) contra um `/@` que só tem módulos 7.0.3-1 →
   **Emergency Mode** (`boot-emergency.log`).
4. Usuário usa o Limine para bootar o **snapshot `660`**
   (`/@/.snapshots/660/snapshot`, kernel 7.0.10-2) — que sobe normalmente.
5. De dentro do snapshot 660, roda `snapg doctor`. O doctor usa `/` como
   alvo (= snapshot 660, 7.0.10-2). Como o `/boot` ainda não casava com 660,
   o doctor reporta `NeedsSync`, o usuário aplica, e o doctor **sincroniza o
   `/boot` para 7.0.10-2** (o kernel do snapshot de resgate).
6. Reboot → o boot padrão (`/@`, 7.0.3-1) carrega o kernel de `/boot`
   (agora 7.0.10-2) → módulos 7.0.3-1 não batem → **Emergency Mode** de novo
   (`recovery-emergency-journal.log`, 15:09:06).

Janela total de cada Emergency: o kernel carrega, falha em
`ConditionFileNotEmpty=/lib/modules/7.0.10-2-cachyos/modules.devname`, e o
`mount /boot` falha com `tipo de sistema de arquivos desconhecido "vfat"` —
sintoma de que o kernel não acha **nenhum** dos seus módulos.

## 3. Evidência (estado capturado durante a investigação)

| Onde | Kernel | vmlinuz BLAKE2B (prefixo) |
| --- | --- | --- |
| `/boot` (FAT32) | `7.0.10-2-cachyos` | `e1766f22…` |
| `/@` (boota por padrão) | `7.0.3-1-cachyos` | `0def202e…` |
| snapshot `660` (bootado no resgate) | `7.0.10-2-cachyos` | `e1766f22…` |

- `findmnt /` no resgate: `subvol=/@/.snapshots/660/snapshot` — **não** `/@`.
- `limine.conf`, entrada padrão: `cmdline: … rootflags=subvol=/@`, apontando
  para `vmlinuz-linux-cachyos#e1766f22…` (7.0.10-2).
- `/boot` e snapshot 660 têm o **mesmo** hash de vmlinuz → o doctor mirou 660.
- `/@` tem hash diferente → é o que realmente boota e está descasado.

## 4. Causa raiz

São **duas** causas independentes, encadeadas:

### 4.1 Interrupção da janela de sync (já endereçada)

O Ctrl+C durante `sync_fat32` deixou o `/boot` sem o kernel do root
restaurado. Coberto pelo Patch A (detecção de `.snapg_boot_backup`
remanescente como `NeedsSync(InterruptedSync)`) e pela Fase 3 da proposta
(bloquear interrupção da janela crítica) — esta última ainda **não**
implementada.

### 4.2 `doctor` mira `/`, não o subvolume que boota por padrão (causa central)

`snapg doctor` sem argumentos usa `/` como root alvo
(`doctor.rs::current_system_target`). Quando o usuário boota um **snapshot de
resgate** pelo Limine, `/ ≠ /@`: o doctor "conserta" o `/boot` para o
ambiente de resgate, não para o `/@` que boota por padrão. Aqui isso
sincronizou `/boot` para 7.0.10-2 (snapshot 660) em vez de 7.0.3-1 (`/@`),
**piorando** o estado.

O sinal correto de "o que vai bootar" já existe e é o mesmo princípio do
Patch B: o `fstab` declara `subvol=/@` para `/`, e o `cmdline` da entrada
padrão do Limine confirma `rootflags=subvol=/@`. O doctor ignora ambos e
confia cegamente no subvol montado.

**Consequência para os Patches A/B:** eles estão corretos, mas pressupõem que
o doctor mira o root certo. Em cenário de resgate, a seleção de alvo do
doctor é o elo fraco — e precede A/B.

## 5. Melhorias propostas

Detalhe e trade-offs na proposta
`docs/proposals/transactional-boot-sync-doctor.md`. Resumo por alavancagem:

1. **Doctor/sync miram o subvol padrão (`/@`), não o `/` montado.** Comparar
   o subvol de `/` (`findmnt -no SOURCE /`) com o declarado pelo fstab/Limine;
   se divergir, montar o `/@` real e mirar nele, ou recusar com instrução
   precisa. *Maior alavancagem; pré-requisito para A/B valerem em resgate.*
2. **Invariante final antes do reboot:** `/boot` casa com o subvol **padrão**,
   não só com o root operado. Backstop universal independente de como o
   mismatch surgiu. Hoje `verify_synced` só checa o root que recebeu o sync.
3. **Bloquear interrupção da janela crítica** (Fase 3): `systemd-inhibit` +
   ignorar SIGINT, com flag de debug para reprodução controlada.
4. **Versão do kernel visível na lista e na confirmação do restore:** expor a
   transição (ex. "7.0.10-2 → 7.0.3-1, downgrade — `/boot` será reescrito").
   Prevenção barata; torna o perigo legível.
5. **`/boot` em BTRFS (ADR §B2.3):** kernel + initramfs + módulos voltam
   juntos e atomicamente no snapshot. Elimina a classe inteira; 1–4 são
   mitigação enquanto a migração não acontece.

Ordem recomendada: 1 + 2 agora (código curto), depois 4 e 3, com 5 como meta
estratégica.

## 6. Estado pendente do sistema

No momento da escrita, a máquina segue **inconsistente**: `/boot` em 7.0.10-2,
`/@` em 7.0.3-1. O boot padrão cai em Emergency; só sobe bootando o snapshot
660 pelo Limine. Duas saídas (a executar quando o usuário decidir a direção):

- **Completar a restauração (ficar no Certo 7.0.3-1):** montar `/@` e
  `snapg doctor --root /mnt/at --boot /boot --apply` para sincronizar `/boot`
  ao kernel 7.0.3-1 do `/@`.
- **Abandonar o teste (voltar ao 7.0.10-2):** `snapg restore` para um snapshot
  7.0.10-2, rolando `/@` de volta para casar com o `/boot` atual.
