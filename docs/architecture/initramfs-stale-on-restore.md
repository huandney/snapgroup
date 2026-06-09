# Initramfs desatualizado no restore (FAT32 + Limine) — decisão em aberto

> **Status:** **DECIDIDO — Opção A (sempre regenerar), implementada** no commit
> `b0bfe17`. As opções abaixo ficam como registro do raciocínio que levou à
> escolha. O residual de pending (§4) segue tratado como regra operacional.
>
> **Data:** 2026-06-06 (decidido 2026-06-09). **Branch:** `fix/doctor-restore-regret`.

---

## 0. TL;DR

Num `/boot` FAT32 + Limine, o snapg pode **pular o sync** durante um restore quando
o `vmlinuz` e os hashes do `limine.conf` já batem. Mas isso é cego para o
**initramfs**: uma atualização de **DKMS sem troca de kernel** (ex: nvidia `.03` →
`.04`) muda o conteúdo do initramfs **sem** mudar o `vmlinuz`. Restaurar um snapshot
anterior nesse cenário deixa o `/boot` com um initramfs incoerente com os módulos do
root restaurado. O boot sobe, mas a sessão gráfica/NVIDIA quebra por mismatch de
versão entre módulo do kernel e userspace.

Não é o mesmo bug do hash do `limine.conf` (aquele **bloqueava** o boot no
bootloader). Este é mais brando (boot sobe, desktop quebra) mas **mais comum**
(todo update de DKMS sem kernel novo cai aqui).

---

## 1. O problema

### 1.1 Por que o initramfs é o ponto cego

O snapg só pode comparar byte a byte o que está **armazenado no snapshot**. Em
FAT32, o `/boot` (e portanto o initramfs) vive **fora** do snapshot — o initramfs é
um artefato **regenerado** por `mkinitcpio`, não um arquivo versionado no subvol.
Consequência: não há "initramfs do snapshot" para comparar. O único sinal
version-locked comparável é o `vmlinuz` (casado com a versão do kernel).

Isto já está documentado no código (`src/boot.rs`, comentário em
`boot_matches_snapshot`): *"compara só o vmlinuz, não o initramfs... num restore de
mesmo kernel o gate pula a regeneração, mantendo o initramfs do sistema vivo — se o
snapshot tiver mkinitcpio.conf/hooks diferentes, o initramfs pode não casar"*. E
remete ao ADR `boot-and-standalone-decision.md §B2.3`, que conclui que o gap só
desaparece de vez com `/boot` no BTRFS.

### 1.2 Cenário concreto (medido)

Transação de pacman real que dispara o caso (kernel **não** muda):

```
linux-cachyos: continua 7.0.11-1-cachyos        (vmlinuz inalterado)
nvidia-580xx-dkms: 580.159.03 → 580.159.04       (DKMS recompila o módulo)
==> dkms install nvidia/580.159.04 -k 7.0.11-1-cachyos
==> Building initramfs for linux-cachyos (7.0.11-1-cachyos)   (mkinitcpio regenera)
Updated: /boot/limine.conf                        (hashes atualizados)
```

Depois disso, restaurar um snapshot tirado **antes** (com nvidia `.03`):

| Sinal de "boot pronto" | Resultado | Por quê |
|---|---|---|
| `vmlinuz` (`boot_matches_snapshot`) | ✓ casa | kernel não mudou → bytes idênticos |
| hashes (`limine_hashes_match`) | ✓ casa | o pacman acabou de atualizá-los p/ o `/boot` atual |
| **initramfs vs root restaurado** | **✗ diverge** | `/boot` tem initramfs com nvidia `.04`; root restaurado tem `.03` |

Como os dois sinais checados passam, `boot_ready` retorna `true`, o sync é **pulado**,
e o `/boot` mantém o initramfs `.04` sobre um root `.03`.

### 1.3 Onde está o gate, no código

- `boot_ready = boot_matches_snapshot && limine_hashes_match` (`src/boot.rs`) — o
  predicado de "boot pronto".
- Short-circuit do sync: `src/boot.rs` em `sync_fat32_paths`
  (`if !interrupted && boot_ready(...) { return }`).
- O caminho completo (`sync_inner`) **sempre** regenera o initramfs (`regen_initramfs`)
  e atualiza os hashes (`refresh_limine_boot_hashes`) — então o problema é **só** o
  short-circuit decidir não chamá-lo.
- Restore chama `boot::sync_fat32(&restored_root)` após o rename-dance
  (`src/commands.rs`), quando `Path::new("/")` (por inode) já é o root antigo.

### 1.4 Severidade

- **Impacto:** boot sobe; módulo nvidia do initramfs (`.04`) diverge do userspace
  restaurado (`.03`) → "NVIDIA kernel/userspace version mismatch" → X/Wayland não
  inicia. Console funciona; recuperável.
- **Frequência:** alta — qualquer update de DKMS (nvidia, vbox, etc.) sem troca de
  kernel produz o estado de gatilho.
- Comparar com o bug de hash do `limine.conf` (já corrigido): aquele era **hard
  block** no bootloader; este é **degradação** pós-boot.

---

## 2. Restrição de fundo: cheap + safe + skip = escolha dois

Não existe um skip do initramfs que seja ao mesmo tempo **barato** e **100% seguro**:

- O único skip totalmente seguro é gerar o initramfs num temp e comparar byte a byte
  com o `/boot` atual — mas isso **roda o `mkinitcpio`** (a parte cara), anulando o
  ganho.
- Qualquer skip barato (comparar metadados de fontes, etc.) é **heurístico**.

Logo, toda solução cai num dos lados: ou **sempre regenera** (abre mão do skip
barato, mantém a garantia) ou **pula por heurística** (abre mão da garantia, ganha
velocidade).

---

## 3. Opções consideradas

### Opção A — Sempre regenerar no restore (burro e correto)

No restore FAT32 que envolve root, nunca usar `vmlinuz`/hash como autorização para
pular `mkinitcpio`. Sempre rodar o caminho completo. Otimização segura opcional: não
recopiar o `vmlinuz` se os bytes já forem idênticos.

- **Mudança:** remover o short-circuit de `sync_fat32_paths`; deletar
  `BootSyncPanel::already_synced` (vira código morto, quebraria `-D warnings`);
  simplificar `boot_will_change` para "root participa → avisa"; opcional: skip de
  cópia do vmlinuz em `sync_inner`. ~10 linhas líquidas.
- **Prós:** correção **garantida** (sem premissa, sem heurística); pouco código;
  alinhado à doutrina (§5, §9.3); erro de sync agora é ruidoso e recuperável
  (backup + `verify_synced` com hash).
- **Contras:** todo restore paga o sync completo (~130MB de backup + `mkinitcpio`,
  alguns segundos) mesmo quando nada do initramfs mudou; reabre a janela de
  interrupção (`Ctrl-C`) a cada restore — mitigada pelo backup/verify.

### Opção B — Comparação local de fontes do initramfs (skip heurístico)

Quando `vmlinuz`/hash batem, comparar os **metadados** (tamanho + mtime, recursivo)
das fontes do initramfs entre o root ativo (`/`, que gerou o `/boot` atual) e o root
restaurado: `/usr/lib/modules/<kver>`, `/etc/mkinitcpio.conf`, `/etc/mkinitcpio.d/`,
`/etc/initcpio`, `/usr/lib/initcpio`. Se nada diferir, pular a regeneração.

- **Prós:** evita regeneração (e a janela de interrupção) no caso comum de restaurar
  para um snapshot sem mudança de kernel/DKMS/config; sem arquivo de estado, sem
  ferramenta externa, só std.
- **Contras / buracos (importantes):**
  1. **Premissa:** assume que o `/boot` reflete as fontes **atuais** de `/`. Se as
     fontes mudaram sem rodar `mkinitcpio` (edição manual, pacote que solta arquivo
     sem hook), o skip libera um initramfs velho.
  2. **`len + mtime` é heurística, não conteúdo** — mesmo tamanho + mesmo mtime com
     conteúdo diferente passa como igual (raro, mas é o tipo de raro que derruba um
     tool de segurança).
  3. **Symlinks ignorados** (`is_file()` pula symlinks): mudanças em `build`/`source`
     em `modules/<kver>` não são vistas.
  4. ~80 linhas de comparação recursiva num caminho frio; o `/usr/lib/modules/<kver>`
     tem milhares de arquivos → milhares de `stat()` × 2 com cache frio (não é
     "<10ms"/"instantâneo").
- **Para não vender garantia falsa**, se adotada: documentar como heurística (não
  "garantido") e incluir symlinks na comparação.

### Opção C — `/boot` no BTRFS

O initramfs passa a viver dentro do snapshot e é restaurado atomicamente; o gap some
de vez. É decisão **arquitetural**, fora do escopo deste fix; já discutida no ADR
`boot-and-standalone-decision.md`.

---

## 4. Residual relacionado: gate de pending

Independente da opção escolhida para o restore, o **gate de pending**
(`pending_boot_synced` → `boot_already_synced` → `boot_ready`, em
`src/commands.rs`) continua cego ao initramfs. Cenário: restaurar (sync roda, `/boot`
ok), ficar em pending, rodar `pacman`/DKMS — o `mkinitcpio` regenera o initramfs a
partir do root **vivo** (`@_snapg_regret`, o antigo) e atualiza os hashes; aí
`pending_boot_synced` compara `/boot` vs destino, vmlinuz/hash batem → oferece
**Reiniciar** com initramfs do root errado.

Tratamento atual previsto: **regra operacional** ("resolver o pending logo; não rodar
pacman no pending"), consistente com a regra já existente do limine. Fechar isso em
código exigiria sempre sincronizar no "Reiniciar" do pending (perde o reboot limpo
instantâneo). Decisão acoplada à da §3 — registrar junto.

---

## 5. Critérios para a decisão

- **Perfil de uso:** quantos restores por ciclo? Se raros, o custo da Opção A é
  irrelevante e a garantia vence.
- **Tolerância a heurística:** o tool já mordeu o usuário 2× em edge cases de `/boot`
  (hash do limine; este DKMS). Para um comando de segurança, garantia > velocidade.
- **Doutrina do projeto:** §5 (explícito e chato > esperto), §9.3 (não otimizar
  caminho frio) favorecem A.

> **Decisão:** Opção A, implementada em `b0bfe17`. Opção C (`/boot` no BTRFS)
> fica como feature futura separada — ver `docs/proposals/snapg-migrate-boot.md`.
