# Refatoração Regret / Pending / Doctor — Handoff completo

> **Propósito deste documento:** registrar TODA a discussão e as decisões de uma
> longa conversa de design/refatoração, para que uma próxima IA (ou pessoa)
> tenha o contexto completo e **não precise reanalisar** do zero. Cobre: os dois
> problemas que iniciaram a refatoração, a evolução do mecanismo de undo
> (aside → stash → pending), o modelo conceitual do "Regret", o desenho atual do
> gate e do doctor, o estado do código, e o problema **ainda em aberto** de
> interoperação com o `limine-snapper-sync`.
>
> **Branch:** `fix/doctor-restore-regret`. **Versão:** 0.5.0-beta.
> **Última validação:** 24 testes passam, clippy limpo.

---

## 0. TL;DR para quem tem pressa

- O snapg restaura snapshots agrupados renomeando o subvol ativo `@` → `@_snapg_regret`
  (o "Regret"/botão de undo) e promovendo a cópia do destino a `@`. Como o rename é
  metadata-only e o mount sobrevive por inode, **enquanto não se reinicia** o `/`
  fica montado de `@_snapg_regret` — estado chamado **pending**.
- Dois bugs iniciais (corrigidos): (1) o **doctor** poluía o Regret legítimo ao
  recuperar; (2) **membros de grupos** não apareciam num boot de resgate.
- O caso real que disparou tudo foi: restore para kernel mais antigo, interrupção
  com `Ctrl-C`, reboot quebrado, boot manual por snapshot Limine, `snapg doctor`,
  tentativa de restore completo sem o `/` aparecer, e depois recuperação escolhendo
  kernel/root. Esse fluxo mostrou que o doctor estava operando em estado de falha e
  portanto **não pode tratar o sistema montado como "estado bom para salvar como
  Regret"**.
- O mecanismo de preservação do Regret evoluiu: **aside → stash → bloqueio de pending**.
  A ideia de **labels** (archive rotulado) foi cogitada e **descartada**.
- O **gate** (restore/delete) e o **doctor** foram unificados para tratar o pending:
  pending limpo → Reiniciar/Cancelar; pending dessincronizado → leva ao doctor →
  Sincronizar e reiniciar / Cancelar.
- **PROBLEMA EM ABERTO:** o `@_snapg_regret` e a escrita direta no `/boot` colidem
  com o ecossistema `limine-snapper-sync`. Detalhado na §8.
- **Estado:** o trabalho do **gate + doctor + synced/desync** foi **commitado**
  (`960ab73`, pushed); este doc foi adicionado em `d0dd412`. O **lock do mutex
  limine (Gap 1)** ainda **não foi implementado** — é a próxima tarefa.

---

## 0.1 Linha do tempo curta da discussão

Esta seção existe para evitar que a próxima IA confunda "o que motivou" com "o que
foi implementado depois".

1. **Incidente operacional:** o usuário restaurou para uma versão com kernel mais
   antigo, interrompeu o restore no meio com `Ctrl-C`, reiniciou e o boot falhou.
   Entrou por um snapshot Limine e rodou o doctor.
2. **Sintoma 1 no doctor:** no restore completo apareciam `home` e `root_home`, mas
   não aparecia `/`. A hipótese inicial era que o Limine montava o snapshot de forma
   que o Snapper ficasse cego/readonly para o root. A conclusão de engenharia foi:
   no caminho de resgate, `snapg` não deve depender cegamente de `snapper list`.
3. **Sintoma 2 no doctor:** ao "consertar escolhendo a versão do kernel", o Regret
   correto parecia ter sido sobrescrito pelo estado problemático. Regra decidida:
   doctor/recuperação nunca deve atualizar Regret; o estado substituído pelo doctor
   é discard, não undo oferecível.
4. **Primeiro fix:** introdução de `RestoreRegretPolicy::{Update, PreserveExisting}`
   e restore de doctor preservando Regret.
5. **Segundo debate:** remoção do `aside`. O insight foi que permitir restore em
   cima de pending era uma conveniência rara e perigosa. Melhor bloquear/encaminhar
   esse estado do que manter uma terceira categoria de subvol.
6. **Refino posterior:** pending deixou de ser só "Regret no menu" e passou a
   precisar de gate próprio, porque pending limpo e pending dessincronizado têm UX
   e risco diferentes.

---

## 1. Os dois problemas iniciais (origem de tudo)

A refatoração começou a partir de **dois pontos** que o usuário apontou. Uma
implementação inicial foi feita pelo **Codex**, e esta conversa começou com a
**revisão dessa implementação**.

### 1.1 O Regret era atualizado/poluído ao usar a recuperação (doctor)

No fluxo normal de restore, o subvol ativo é promovido a `@_snapg_regret` (o botão
de undo). Mas num **boot de resgate** (você bootou num snapshot read-only porque o
sistema normal não subia), o "ativo" é o snapshot de resgate — promovê-lo a Regret
**destruiria o Regret legítimo anterior** e deixaria o undo apontando para um estado
ruim.

**Correção:** uma política de restore que **preserva** o Regret existente. No caminho
de recuperação, o estado substituído vai para `<subvol>_snapg_discard_<label>`
(descartável, limpo no próximo boot pelo "serviço fantasma" `snapg-cleanup`), e o
Regret anterior fica **intacto**. Isso virou `RestoreRegretPolicy { Update, PreserveExisting }`.

### 1.2 Membros de grupos não apareciam (resgate)

Com `/` sendo um snapshot de resgate, o Snapper fica **cego** além do root —
`group::list_groups()` não enumerava todos os membros (root + home + ...). A
implementação inicial só "augmentava" o membro **root** lido do top-level btrfs e
**confiava no snapper** para o resto.

**Correção (refinada depois):** `scan_groups_from_toplevel` lê **todas as configs**
direto do top-level e só forma **grupos completos** (todas as configs presentes),
em vez de confiar no snapper cego. Parser manual mínimo de `info.xml` (sem crate de
XML) extrai o `snapgroup-id` e os campos necessários.

### 1.3 Critério de aceitação para o restore completo no doctor

No modo doctor/resgate, "restore completo" não pode oferecer grupos parciais como se
fossem checkpoints completos. A tela que revelou o bug era parecida com:

```text
↺ Regret                  7.0.11-1-cachyos   2026-06-04 03:03   3 membros
Atual3                    ?                  2026-06-04 09:51   1 membros   #1780566707
Atual2                    ?                  2026-06-04 06:57   1 membros   #1780556224
Atual                     ?                  2026-06-04 06:48   1 membros   #1780555738
```

Se existem 3 configs (`root`, `home`, `root_home`, por exemplo), checkpoints de
`snapg save` devem aparecer com 3 membros. Um checkpoint com 1 membro no restore
**completo** do doctor é evidência de que o scanner ainda está misturando visões:
root reconstruído pelo top-level e outras configs vindas de uma fonte incompleta.

Regra prática para a próxima implementação: em `PreserveExisting`, montar grupos
por fonte snapper-independente para todas as configs ou filtrar grupos incompletos.
Não apresentar `1 membros` como restauração completa. Um restore parcial pode
existir como feature explícita, mas precisa aparecer como parcial e não como
"restore completo".

> **NOTA sobre a memória:** estes dois problemas **não estão registrados** nos
> arquivos de memória do agente de forma específica (só há um `project_snap_tools.md`
> genérico). Este documento é a fonte canônica deles.

---

## 2. Modelo conceitual do "Regret" (o insight central — leia isto)

Vários impasses da conversa só se resolveram quando o modelo abaixo ficou claro.
**É o conceito mais importante do documento.**

- O `@_snapg_regret` **NÃO é uma foto/snapshot congelado**. É o **chão vivo**
  (subvol ativo anterior) **renomeado**. O rename é metadata-only; o kernel continua
  rodando dele por inode. Confirmado no código e na história do git: o rename
  `current → <subvol>_snapg_regret` existe desde o commit `restore-highlander`
  (`22e5451`), **antes** do aside.
- **Pré-reboot:** o Regret é o **presente vivo** — você está montado nele, ele
  continua sendo escrito. Por isso desfazer um restore pré-reboot é **grátis e sem
  perda**: você nunca saiu desse chão. "Desfazer" pré-reboot = cancelar o reboot
  agendado, não viajar no tempo.
- **Pós-reboot:** quando você boota o subvol restaurado, o `@_snapg_regret` **para
  de ser escrito** (ninguém mais o monta) e **vira de fato um instantâneo congelado**
  — exatamente no momento certo (imediatamente antes da troca real).
- **Simetria:** antes do reboot o Regret é vivo (undo grátis); depois do reboot é
  passado congelado (restaurá-lo é uma viagem real, cria discards). Os dois caminhos
  no código são `undo_restore_before_reboot` (pending) e `revert_regret` (archived).

### Consequências de design tiradas desse modelo

- **"Regret = save automático congelado" foi REJEITADO.** Congelaria cedo demais (no
  clique do restore), perdendo o trabalho feito entre o clique e o reboot; e
  reintroduziria a ambiguidade de dois pontos de retorno. O modelo atual (chão vivo
  que congela no reboot) já é o ideal.
- **Data do Regret = momento do restore.** A data deve vir do **otime do subvol
  restaurado (o ativo)**, criado na Fase 1 com `create_snapshot` — não do
  `regret_subvol` (cujo otime é a data de nascimento do subvol base: instalação ou
  restore antigo, irrelevante). Implementado em `regret_creation_time` usando
  `current_subvol`.
- **"Um Regret por sessão" é lei física, não regra a impor.** O Regret é sempre o
  mesmo `@_snapg_regret` (único `O` vivo entre boots). Trocar de checkpoint sem
  reboot reusaria o mesmo subvol (rename ida-e-volta, otime intacto). A exceção que o
  usuário cogitou (atualização de kernel no meio) **não** cria outro Regret — é um
  problema separado do `/boot` (ver §8).

---

## 3. Evolução do mecanismo de preservação do Regret (aside → stash → pending)

Esta é a espinha dorsal da refatoração. Três fases, com as razões de cada salto.

### 3.1 Aside (ponto de partida, herdado)

Quando um restore de checkpoint ocorria e já existia um Regret pendente, o Regret
**anterior** era movido para um nome aside (`<subvol>.snapgroup_regret_aside`) em vez
de deletado, para preservar o botão de undo antigo até o reboot.

**Mecânica em erro (importante):** durante um restore há **dois** Regrets em jogo —
o **novo** (`backup_subvol`, o chão deste restore que vira `_snapg_regret` na Fase 2)
e o **anterior** (movido para aside). No erro, `revert_partial` "pisa" no **Regret
novo** para trazer o chão de volta ao nome ativo; depois `restore_asides` traz o
anterior de volta ao slot canônico — mas **só se o estado é limpo** (`PartialOutcome::Clean`).
Em estado ambíguo (`Indeterminate`), o aside ficava para recuperação **manual**.

**Problema do aside:** coordenar dois Regrets, com um ramo ambíguo que caía no colo
do usuário ("aside órfão de tentativa interrompida" → recuperação manual). Era a peça
mais sutil e frágil do projeto.

### 3.2 Stash (correção pontual de atomicidade)

Numa versão intermediária (block), o Regret **archived** anterior era **deletado
eagerly** antes do rename. Isso quebrava silenciosamente a invariante "uma falha
nunca te deixa pior": se o restore falhasse, você perdia o undo anterior **mesmo
sem o sistema ter mudado**.

**Correção (commit `9c2fbe2`):** em vez de deletar eager, faz **stash** — renomeia o
Regret anterior para `<subvol>_snapg_discard_old-regret_<label>_<pid>` **antes** do
rename-dance; em cada etapa de falha, tenta restaurá-lo (`restore_stashed_backup`,
exposto via nota no erro); no sucesso, `delete_stashed_backup` apaga. O nome usa o
prefixo `_snapg_discard_`, então o `boot-clean` varre como rede de segurança se o
delete falhar.

**Avaliação registrada:** isto recolocou ~99 linhas (parecidas com o aside), mas com
propósito diferente — **atomicidade de falha** (sempre desejada), não a feature de
UX de encadear restores (que foi cortada). O stash é mais enxuto que o aside (35 vs
62 linhas) e justificado por uma invariante de segurança real.

### 3.3 Bloqueio de pending (estado atual, substituiu o aside)

A decisão final: **não permitir restaurar enquanto há um pending** (commit `9c2fbe2`,
refinado depois). Em vez de duas categorias de Regret (regret + aside), há **uma**.
O conflito de slot é resolvido na **política** (proibir o estado de colisão), não no
**mecanismo**. Isso eliminou o aside (`aside_existing_regrets`, `restore_asides`,
`regret_aside_name`, `AsidedRegret`, `PartialOutcome::{Clean,Indeterminate}`) e
simplificou `handle_partial` para um fluxo linear.

**Por que é superior ao aside:** a categoria pending/archived emerge de um **fato
físico** — `pending_restore_from_live` detecta o pending checando se o subvol vivo
termina em `_snapg_regret` (via `subvol_relative_path`/`strip_suffix`). Não é
bookkeeping mantido à mão; é derivado do estado real do mount. Por isso não precisa
da maquinaria de "aside órfão".

### 3.3.1 Por que ficou com mais linhas mesmo simplificando

A métrica correta aqui não é LOC; é **quantidade de estados de produto** que o resto
do sistema precisa conhecer.

Modelo antigo:

- `Regret` — aparece como undo.
- `Discard` — lixo pós-restore/doctor.
- `Aside` — Regret deslocado temporariamente.
- Cleanup conhecia aside.
- UI tinha mensagem manual de aside.
- Falha parcial tinha que decidir entre restaurar aside ou preservá-lo em estado
  ambíguo.

Modelo novo:

- `Regret` — aparece como undo ou é tratado pelo gate de pending.
- `Discard` — lixo seguro para cleanup.
- `Stashed old Regret` — detalhe **privado** do rename-dance, sem UI e sem modelo
  de produto.

As linhas aumentaram porque a refatoração fechou robustez que o aside cobria de
forma global: preservar Regret antigo durante falha do rename-dance, restaurar o
stash em erro e permitir undo quando o subvol base já sumiu. O ganho é que essa
complexidade ficou confinada em `commit_prepared_subvol`/`revert_partial`, não
espalhada por descoberta, UI, cleanup e fluxo de doctor.

Resumo: **menos estados globais, mais código local de atomicidade**. Isso é uma
simplificação arquitetural, não minificação.

### 3.4 Labels (archive rotulado) — COGITADO e DESCARTADO

No meio do caminho cogitou-se substituir o slot fixo `@_snapg_regret` por um modelo
de **archive rotulado** (`@_snapg_regret_<label>`, oferecendo o mais recente como
undo), para eliminar o aside.

**Descartado** porque: (a) sacrificava a descoberta O(1) por nome fixo; (b) exigiria
**tocar o rename-dance do root** — o código mais perigoso do projeto (erro = não
boota). O bloqueio de pending atinge a mesma simplificação **sem** tocar o
rename-dance. A proposta de labels tinha pior relação risco/benefício.

---

## 4. Outras decisões e otimizações registradas

- **`commit_prepared_subvol` unificado:** os commits normal (`commit_prep`, cria
  Regret) e de recuperação (`commit_prep_preserving_regret`, cria discard)
  compartilham o rename-dance. A Fase 1 (prepare, IO-pesada) ficou isolada; a Fase 2
  (commit, metadata) foi parametrizada por `CommitMode { Regret, PreserveRegret }`
  via `rollback_group_with_commit`. Boa unificação, sem acoplamento.
- **Parser de `info.xml`:** robustecido. `snapgroup_id_from_info_xml` ancora na
  estrutura `<userdata><key>snapgroup-id</key><value>…</value>` (via `xml_blocks` +
  `xml_text`), com fallback `snapgroup-id=NNN` e `digits_only` (rejeita valor
  não-numérico). Coberto por testes focados. Decisão consciente de **não** puxar
  crate de XML.
- **`base_subvol_of_mountpoint`** (em `rollback.rs`): extraída para remover a
  duplicação do strip de sufixo pending entre `commands.rs` e `rollback.rs`.
  Preserva a semântica divergente do caso `/` (Option vs fallback).
- **Não-recomendação de micro-otimizações de alocação** no parser: o caminho é
  IO-bound e rodado uma vez por interação humana; perseguir allocs ali é desperdício.
  (Discussão longa sobre `unescape_xml`, `digits_only`, etc. — conclusão: caminho
  frio, não otimizar.)

---

## 5. Undo implícito — COGITADO e DESCARTADO

Cogitou-se um "undo implícito": trocar de checkpoint com um pending ativo num clique
só (desfaz o pending e aplica o novo internamente).

**Descartado.** Como o Regret resultante seria sempre o mesmo `O` (chão vivo), o undo
implícito não comprava segurança — só fundia "desfaz + restaura" num clique. Para uma
operação de risco (mexe no que boota), **dois passos explícitos > um passo mágico**.
O bloqueio de pending já entrega isso: para ir a outro checkpoint, você **desfaz**
(reiniciar/cancelar) e então restaura. A transição A→B sempre passa por `O`
fisicamente de qualquer forma; o bloqueio só torna explícito.

---

## 6. Data/rótulo do Regret (commit `e73c1aa`, parcialmente revertido em `960ab73`)

- **Archived:** data = otime do subvol restaurado (`current_subvol`) = momento do
  restore. (Mantido.)
- **Pending (no commit `e73c1aa`):** mostrava rótulo "não reiniciado" no menu de
  restore via `RegretKind` e `regret_when()`.
- **ATENÇÃO:** o trabalho do gate (§7, commit `960ab73`) **remove** o pending do menu
  de restore (e o `RegretKind`/`regret_when`), porque o pending passou a ser tratado
  pelo gate (Reiniciar/Cancelar), não como Regret restaurável no menu. A data archived
  via `current_subvol` permanece; o `list` ganha um aviso de pending separado
  (`print_pending_restore_status`).

---

## 7. Gate + Doctor (desenho atual — commit `960ab73`)

Esta é a parte mais recente. Está **implementada, validada e commitada** (`960ab73`).

### 7.1 Conceito de pending limpo vs dessincronizado

Distinção crucial, detectada por `pending_boot_synced(done)` →
`boot::boot_already_synced(dest_root)` (compara, **byte a byte**, o vmlinuz do
`/boot` com o `vmlinuz` em `<destino>/usr/lib/modules/<kver>`):

| sub-estado | `/boot` vs destino `@` | ação |
|---|---|---|
| **pending limpo** | casa | reiniciar é seguro |
| **pending dessincronizado** | **não** casa | reiniciar quebraria o boot |

(Em `/boot` não-FAT32/nativo, `boot_already_synced` retorna `true` sempre — o kernel
vive dentro do snapshot.)

### 7.2 Comportamento de `restore` e `delete` (gate)

Em `restore_with_policy` e `delete`, **antes** do fluxo normal, detecta pending
(`pending_restore_from_live`) e chama `gate_pending_restore(done)`:

- **pending limpo** → `resolve_pending_clean` → prompt **`select_pending_action`**:
  - **Reiniciar** (concluir) → `reboot_now()` puro (o `/boot` já casa);
  - **Cancelar** → `cancel_pending_restore` → `undo_restore_before_reboot`.
- **pending dessincronizado** → **`confirm_run_doctor`** ("Rodar o doctor para
  sincronizar o /boot?"):
  - sim → `resolve_pending_sync(done)` → prompt **`select_pending_sync_action`**:
    - **Sincronizar e reiniciar** → `complete_pending_restore` (sync `/boot` com o
      destino + `reboot_now`);
    - **Cancelar** → `cancel_pending_restore`.

> **NOTA de implementação:** o "Rodar o doctor" do gate **não relança o processo
> `snapg doctor`** (evita conflito com o lock global do snapg, pego no `main`). Ele
> chama `resolve_pending_sync` **inline** — funcionalmente o mesmo fluxo do doctor no
> pending, sob o mesmo lock. Há a opção (não feita) de mostrar o diagnóstico de boot
> completo (tabela de kernels) nesse prompt; só não foi adicionado.

### 7.3 Comportamento do `doctor` (unificado)

`doctor::run`, no caminho implícito (sem `--root/--boot`), agora detecta o pending
**antes** do `detect_rescue_boot`:

```rust
if has_pending_restore()? {
    let _lock = lock::acquire()?;
    return doctor_resolve_pending();   // limpo → reiniciar/cancelar; dessinc → sincronizar/cancelar
}
if let Some(ctx) = detect_rescue_boot()? {
    return resolve_rescue(ctx, apply); // resgate-real → menu A/B/C (inalterado)
}
```

Motivo: um pending faz o `/` montado ser `@_snapg_regret`, que **diverge do fstab** e
o `detect_rescue_boot` **confundia com resgate** (mostrava o menu A/B/C de 3 opções).
Agora o pending vai direto para o mesmo prompt de reiniciar/cancelar do restore/delete.
O menu A/B/C de resgate fica **só para o resgate-real** (boot manual num snapshot
read-only de `.snapshots`).

### 7.3.1 Não duplicar o "desfazer pending" no doctor

Durante a discussão houve uma fase em que o doctor mostrava uma opção explícita:

```text
Desfazer restauração sem reboot — volta ao estado anterior e restaura o Regret antigo
```

e o restore completo também mostrava `↺ Regret` para o pending. Isso é duplicação:
as duas entradas executam a mesma intenção de produto (cancelar/desfazer o restore
pendente). A regra final desejada é ter **uma única metáfora por estado**:

- Se a UX escolhida for "pending aparece no restore", então o menu do doctor não
  deve ter a ação separada; o usuário entra em "Mudar o que boota" e escolhe
  `↺ Regret`.
- Se a UX escolhida for "gate de pending", então o pending não deve aparecer como
  Regret restaurável no menu de checkpoints; o gate oferece Reiniciar/Cancelar
  antes de abrir restore/delete.

Não manter os dois ao mesmo tempo. A duplicidade confundiu porque parecia haver duas
operações diferentes, quando fisicamente ambas passam por `undo_restore_before_reboot`.

### 7.4 Lock (importante para entender o fluxo)

- `restore`/`delete` pegam o lock global do snapg (`/run/snapgroup.lock`) no `main`.
  O gate roda sob esse lock; `cancel_pending_restore`/`complete_pending_restore`
  **assumem o lock pego** (não pegam de novo).
- O `doctor::run` pega o lock **só quando confirma pending** (`has_pending_restore`
  é leitura pura, sem lock); `doctor_resolve_pending` roda sob esse lock.

### 7.5 Diagnóstico do doctor no pending (CORRIGIDO)

Eram **dois** problemas, e a leitura original deste doc estava errada num deles.

**Problema A (referência errada):** a tabela do doctor comparava o `/boot` com o
subvol **VIVO** (`@_snapg_regret`), não com o **destino** (`@`, o que vai bootar).
No pending isso mostrava "✗ /boot difere do /" mesmo quando o `/boot` casava com o
destino. **Corrigido:** `diagnosis_for` (doctor.rs), quando `root == "/"` e há
pending, diagnostica contra o destino via `commands::pending_dest_diagnosis`
(monta o top-level com guard RAII).

**Problema B (o sinal era insuficiente — descoberto depois):** a versão antiga deste
doc concluía que, se o vmlinuz casa com o destino, "está tudo certo". **Errado.** O
usuário reiniciou nesse estado e **ficou preso no estágio do Limine** (bootloader
recusou a entrada). Causa: um restore interrompido com `Ctrl-C` deixou os **hashes
BLAKE2B do `limine.conf`** inconsistentes com os arquivos do `/boot`. O Limine valida
esses hashes no boot; vmlinuz casar **≠** bootável. **Corrigido:** novo
`BootIssue::HashMismatch` e a função `limine_hashes_match`; um predicado único
`boot_ready = boot_matches_snapshot && limine_hashes_match` passou a ser o sinal de
"boot pronto" em **todos** os gates (`diagnose_boot`, `verify_synced`, o early-return
de `sync_fat32_paths` e `boot_already_synced` — este último é o que o gate de pending
usa para decidir Reiniciar). Sem unificar os quatro, o doctor acusava mas o sync/gate
liberavam mesmo assim.

Pendência herdada: a parte de integração (`limine_hashes_match` com `b2sum` real,
mount do top-level) não tem teste automatizado — validar pelo teste manual §9.2.5.

---

## 8. PROBLEMA EM ABERTO: interoperação com `limine-snapper-sync`

Este é o assunto **ativo, não resolvido**. O ambiente do usuário usa **Limine** com
`/boot` **FAT32** e o ecossistema `limine-snapper-sync`.

### 8.1 O que foi observado

Durante o pending, o `limine-snapper-notify` mostrou:

```
Root_subvolume_path=/@ does not match the expected path /@_snapg_regret from /proc/mounts
```

### 8.2 O ecossistema limine (mapeado)

Pacotes: `limine`, `limine-mkinitcpio-hook`, `limine-snapper-sync`. Componentes:

- **`limine-snapper-watcher`** — daemon (`limine-snapper-sync.service`, app Java/ELF)
  que sincroniza boot entries com a lista de snapshots do snapper.
- **`limine-snapper-notify`** — autostart de desktop (`/etc/xdg/autostart/`), checa
  no login (foi quem mostrou a mensagem).
- **`limine-entry-tool`** + hooks de mkinitcpio (pacman hooks) — geram o `limine.conf`.
- **Mutex global:** `flock` em `/tmp/limine-global.lock` (lib `/usr/lib/limine/limine-mutex`,
  funções `mutex_lock`/`mutex_unlock`, com `flock --timeout=30`). Pacman hook,
  entry-tool e scripts de mkinitcpio adquirem esse lock antes de tocar o `/boot`.
- Config `/etc/limine-snapper-sync.conf`: `ROOT_SUBVOLUME_PATH="/@"` (fixo),
  `ROOT_SNAPSHOTS_PATH="/@/.snapshots"`, `RESTORE_METHOD=replace`.

### 8.3 Os dois gaps

- **Gap 1 (SÉRIO — race condition):** o snapg escreve no `/boot` (`sync_fat32_paths`:
  copia vmlinuz, regenera initramfs via mkinitcpio, atualiza hashes BLAKE2B do
  `limine.conf` via `refresh_limine_boot_hashes`) **sem adquirir** `/tmp/limine-global.lock`.
  O lock próprio do snapg só serializa instâncias do snapg. Então `snapg` e um hook
  de mkinitcpio (ou o watcher) podem escrever no `/boot` ao mesmo tempo → **corrupção
  potencial**.
- **Gap 2 (BENIGNO — aviso transitório):** o `@_snapg_regret` no `/proc/mounts`
  diverge do `ROOT_SUBVOLUME_PATH="/@"`, então watcher/notify reclamam. Detecta e
  aborta a sincronização (não corrompe); some no reboot.
- **Gap 3 (sobreposição):** snapg e limine-snapper-sync ambos gerenciam o `/boot` em
  função de snapshots.

### 8.4 Estado real do usuário (medido) — CONCLUSÃO ORIGINAL ERRADA

Medido montando o top-level read-only:
- destino `@`: módulos `7.0.10-2-cachyos`; vivo `@_snapg_regret`: módulos `7.0.11-1-cachyos`.
- vmlinuz do `/boot` **CASA** (bytes) com o do destino `@` (7.0.10).

**Conclusão ORIGINAL (errada):** "o estado está limpo porque o vmlinuz casa".

**Correção:** vmlinuz casar **não** garante boot. O usuário reiniciou nesse exato
estado e **ficou preso no Limine** (bootloader recusou a entrada) — os hashes BLAKE2B
do `limine.conf` estavam inconsistentes com os arquivos do `/boot` (restore
interrompido com `Ctrl-C`). O sinal correto de "boot pronto" inclui a verificação de
hash, não só o byte-compare do vmlinuz (ver §7.5, Problema B). O "✗ difere do /" do
doctor tinha duas causas misturadas: a referência errada (comparava com o vivo) **e**
um desync real de hash. Ambas tratadas. O aviso do limine-snapper-notify (Gap 2)
segue sendo ruído transitório à parte.

### 8.5 Análise das soluções propostas (por outra IA) — vereditos

- **Gap 1 (lock flock): FAZER, com 3 ajustes.** A direção (flock no mesmo
  `/tmp/limine-global.lock`) está certa. Ajustes obrigatórios:
  1. **Usar timeout** (não `LOCK_EX` puro/bloqueante — pode travar o restore
     indefinidamente; o limine usa 30s). Fazer `LOCK_EX|LOCK_NB` em loop com sleep,
     ou `alarm`/`SIGALRM`.
  2. **Centralizar dentro de `sync_fat32_paths`** (não nos call sites — `sync_fat32`
     é chamado em vários lugares; envolver call site por call site esquece algum).
  3. **Gated na presença do limine** (ex: `/usr/lib/limine/limine-mutex` existe?) —
     senão `File::create` cria lixo num sistema sem limine. Usar `OpenOptions` sem
     `truncate`.
- **Gap 2 (bind mount da config): NÃO FAZER.** A proposta de bind-mountar uma config
  falsa (`ROOT_SUBVOLUME_PATH="/@_snapg_regret"`) sobre `/etc/limine-snapper-sync.conf`
  foi **rejeitada**:
  1. Se o watcher **regenerar** o `limine.conf` com a config falsa, pode gerar
     entradas com `subvol=/@_snapg_regret` (o estado pré-restore) → **bootar o subvol
     errado / anular o restore**. O comportamento do watcher (Java stripped) é
     incerto — apostar o `/boot` para silenciar um log é risco/benefício péssimo.
  2. Piora o caso "pacman no pending" (hooks leriam a config falsa).
  3. Bind mount órfão em crash deixa `/etc` "sequestrado" — debug infernal.
  4. Acopla aos internals do limine.
  - **Alternativa honesta:** aceitar o aviso (documentar) **ou** pausar o
    watcher/notify durante o pending — **nunca** falsificar a config.
- **Gap 3 (manter sync no snapg): CONCORDO.** Atomicidade: o snapg precisa garantir o
  `/boot` gravado e **verificado** antes de declarar o restore completo. Delegar a um
  daemon assíncrono terminaria o restore "no escuro". Manter no snapg + lock = correto.

### 8.6 Próximo passo recomendado para o limine

Implementar **só o Gap 1** (lock do mutex com os 3 ajustes), que fecha a race real.
Tratar o Gap 2 com documentação ("após restore, reinicie ou cancele logo; não rode
pacman no pending"). **Não** fazer o bind mount.

---

## 9. Estado do código (o que está feito vs pendente)

### Commits na branch `fix/doctor-restore-regret` (do mais antigo ao mais novo)

1. **`fd686fa` Fix: preserve Regret during doctor recovery** — os 2 fixes iniciais
   (rehydrate root members do top-level; doctor via preserve-existing-Regret +
   discards; `commit_prepared_subvol` compartilhado; parser info.xml + testes).
   *Ainda continha o aside.*
2. **`9c2fbe2` Fix: simplify pending restore recovery** — aside → **stash** +
   pending-as-Regret (`RegretKind::PendingRestore`); `revert_partial` recupera com
   base subvol ausente.
3. **`14df553` Refactor: rebuild rescue group scan and fold pending undo into
   restore** — `scan_groups_from_toplevel` (todas configs, grupos completos); remove
   o caminho de undo dedicado do doctor; extrai `base_subvol_of_mountpoint`.
4. **`e73c1aa` Refine: date Regret by restore time, label pending as not-rebooted** —
   data archived via `current_subvol`; rótulo de pending no menu (depois revertido).
5. **`960ab73` Refactor: handle pending restore via gate, split clean vs desynced
   /boot** — o gate synced/desync + doctor unificado (detalhado abaixo).
6. **`d0dd412` Docs: add regret/pending/doctor refactor handoff** — este documento +
   `archived-subvols-design.html`.

### Conteúdo do commit `960ab73` (validado: 24 testes, clippy limpo, +239/−100)

`src/commands.rs`, `src/doctor.rs`, `src/ui/restore.rs`, `src/ui/snapshots.rs`:
- **Gate synced/desync** (`gate_pending_restore`, `pending_boot_synced`,
  `resolve_pending_clean`, `resolve_pending_sync`, `complete_pending_restore`,
  `cancel_pending_restore`).
- **Doctor unificado** (`has_pending_restore`, `doctor_resolve_pending`; `doctor::run`
  detecta pending antes do resgate).
- **Remoção do pending-como-Regret** do menu de restore (`RegretKind`, `regret_when`
  removidos); Regret no menu volta a ser só archived; `list` ganha
  `print_pending_restore_status`.
- **UI nova:** `select_pending_action` (limpo), `confirm_run_doctor` +
  `select_pending_sync_action` + `PendingSyncAction` (dessincronizado).
- Hint corrigida em `abort_reboot_boot_desync` ("escolha 'Cancelar a restauração'"
  em vez de "selecione o Regret").

### Feito depois (b + b′ + unificação)

- **Diagnóstico do doctor no pending (§7.5, Problema A)** — `diagnosis_for` diagnostica
  contra o destino, não contra o `_snapg_regret` vivo (`pending_dest_diagnosis` +
  `ToplevelMountGuard`).
- **Verificação de hash do `limine.conf` (§7.5, Problema B)** — `BootIssue::HashMismatch`,
  `limine_hashes_match`, e o predicado único `boot_ready` usado em `diagnose_boot`,
  `verify_synced`, no early-return de `sync_fat32_paths` e em `boot_already_synced`.

### NÃO feito (pendências — próximos passos)

- **Gap 1 do limine** (lock do mutex `/tmp/limine-global.lock` dentro de
  `sync_fat32_paths`, com timeout, gated na presença do limine) — **não implementado**.
  Rebaixado de "SÉRIO" para condicional/baixo: o watcher fica inerte sem `inotifywait`
  e o snapg não dispara o plugin de snapper no restore; o vetor real é só um
  `pacman`/`snapper create` externo na janela pending. O flock segue sendo o fix
  correto se quiser a garantia.

---

## 9.1 Invariantes que a próxima IA deve preservar

Estas regras são mais importantes que o formato atual do código:

1. **Doctor não atualiza Regret.** Qualquer fluxo iniciado pelo doctor/resgate deve
   usar semântica `PreserveExisting`: o estado substituído vira discard; Regret
   legítimo permanece.
2. **Restore normal (`Update`) é o único caminho que cria/substitui Regret.**
   Recovery/doctor não deve chamar o caminho normal por conveniência.
3. **Pending é detectado pelo mount vivo, não por arquivo auxiliar.** Se o subvol
   relativo de uma config termina com `_snapg_regret`, essa config está pendente.
4. **Pending bloqueia novos checkpoints.** Para trocar de checkpoint, o usuário
   precisa resolver o pending primeiro: reiniciar/concluir ou cancelar/desfazer.
5. **Restore completo no doctor exige grupos completos.** Se a config listada pelo
   Snapper é `N`, um checkpoint completo precisa ter `N` membros. Não oferecer
   grupo parcial com aparência de restore completo.
6. **`/boot` FAT32 é parte da transação do produto.** O snapg só pode dizer que o
   restore está pronto para reboot depois de sincronizar/verificar `/boot` contra o
   root que vai bootar, não contra o root vivo quando há pending.
7. **O Regret archived não pode ser perdido em falha que deixou o sistema intacto.**
   Se o código precisar liberar o slot `_snapg_regret`, deve stashear e restaurar
   em falha, não deletar eager.
8. **Não confiar em Snapper no resgate Limine.** Snapper é bom caminho normal, mas
   o doctor precisa de primitiva top-level Btrfs para ler snapshots quando `/` é um
   snapshot de resgate ou quando `.snapshots` visto pelo ambiente vivo não reflete
   o sistema-alvo.

---

## 9.2 Testes manuais que importam mais que `cargo test`

`cargo test` cobre parser e utilitários, mas os bugs reais são de integração com
Btrfs, Snapper, Limine e `/boot`. Antes de declarar arquitetura pronta, testar em
ambiente descartável:

1. `snapg save` com 3 configs; `snapg restore`; antes do reboot, rodar `snapg restore`
   de novo. Resultado esperado: pending é detectado e novos checkpoints não são
   oferecidos como se fossem seguros.
2. Bootar em snapshot Limine/resgate e abrir doctor. Resultado esperado: restore
   completo mostra checkpoints com todos os membros; não aparecem grupos `1 membros`
   quando o save tinha 3 configs.
3. Usar doctor para recuperar root/kernel. Resultado esperado: Regret anterior não
   é substituído pelo estado quebrado.
4. Interromper restore entre Fase 2 e reboot quando possível em VM. Resultado
   esperado: doctor/restore consegue cancelar pending mesmo se o subvol base já
   estiver ausente.
5. Em FAT32/Limine, após restore, verificar que `/boot/vmlinuz*` casa byte a byte
   com o root que vai bootar, não com o root vivo `_snapg_regret`.

---

## 9.3 Armadilhas de implementação recorrentes

- **Não medir simplificação por LOC.** O objetivo foi reduzir estados globais
  (`aside` saiu), não reduzir linhas. Helpers extras no rename-dance podem ser
  aceitáveis se preservam atomicidade.
- **Não reintroduzir `snapg undo` público sem decisão explícita.** A intenção mais
  recente foi concentrar a UX em `snapg restore`/gate/doctor, não criar comando
  concorrente.
- **Não tratar checkpoint parcial como "quase completo".** Isso foi exatamente o
  bug observado na tela do usuário.
- **Não tentar silenciar `limine-snapper-sync` falsificando config.** O aviso de
  `_snapg_regret` em `/proc/mounts` é transitório; bind-mount/config fake pode
  gerar boot entries erradas.
- **Cuidado com o binário instalado.** O usuário normalmente testa via pacote
  instalado (`/usr/bin/snapg`); mudanças no source só aparecem depois de
  reinstalar com o PKGBUILD ou rodar explicitamente o binário de `target/`.

---

## 10. Preferências do usuário e convenções (não reanalisar)

- **Idioma:** responder em **pt-BR**.
- **Estilo de engenharia:** procedural, caminho-feliz à esquerda, zero-else como
  padrão, dados abertos. Quer **alternativas + trade-offs com efeito concreto**
  (números/allocs/indireções), tom denso e didático. É **programador autodidata**.
- **Commits:** **sem** `Co-Authored-By` do Claude. Prefixos capitalizados
  (`Fix:`, `Refactor:`, `Refine:`, `Feature:`, `Docs:`), corpo explicando o "porquê".
- **Sujeira:** não criar temporários/backups além do necessário; remover o que criar.
- **Instalação:** tudo via **pacman** (PKGBUILD local) — nunca cargo/pip/npm/curl.
  Consequência prática: **o binário em `/usr/bin/snapg` é o instalado**; mudanças no
  código-fonte só têm efeito após reinstalar (ou rodar `target/.../snapg`
  diretamente). Verificar isto antes de concluir "não mudou nada".
- **Verificação:** `cargo test` + `cargo clippy` ao fim de cada mudança de lógica.

---

## 11. Glossário rápido de nomes de subvol

- `@`, `@home`, ... — subvols ativos canônicos.
- `<subvol>_snapg_regret` — o "Regret" (chão vivo renomeado; botão de undo).
- `<subvol>.snapgroup_prep` — cópia writable intermediária do snapshot-destino (Fase 1).
- `<subvol>_snapg_discard_<label>` — estado descartável (recuperação/doctor; limpo no
  boot pelo `snapg-cleanup.service`).
- `<subvol>_snapg_discard_old-regret_<label>_<pid>` — stash do Regret anterior durante
  o commit.
- `<subvol>.snapgroup_regret_aside` — **REMOVIDO** (era o aside).
