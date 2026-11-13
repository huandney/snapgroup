# Proposta: espaço e retenção no `snapg delete`

- **Status:** §2, §3 e §4 implementadas em `develop` (merges `3593054`,
  `c6a6338`, `6f25ec7`, `5d30698`); §5 (purge de backups) e §6 (`doctor`)
  pendentes. Onde a implementação divergiu do plano, o texto abaixo descreve
  o que foi feito.
- **Data:** 2026-11-12.
- **Relacionado:** `docs/architecture/restore-highlander.md` §5.2 (naming
  `_backup_`), backlog local de retenção (`TODO.md` §2–§3), que esta proposta
  concretiza.
- **Maquete:** variante A+B em
  <https://claude.ai/artifact/BgHJAuoQothSppxejpyupV> (privada).

## 0. TL;DR

Três peças, um lugar só para agir (`snapg delete`):

1. **Backups órfãos:** o `delete` ganha uma seção com os
   `<base>_backup_<timestamp>` que restores de outras ferramentas deixam no
   top-level. Deleção imediata, sem lixeira.
2. **Espaço:** o picker mostra o espaço exclusivo de cada item e uma barra do
   recuperável que atualiza na hora a cada marcação.
3. **Retenção por intenção:** checkpoint salvo com o nome padrão (Enter) é
   automático e entra na retenção; checkpoint com nome digitado é fixo e nunca
   sai sozinho. Backups órfãos ganham um purge por idade, desligado por padrão.

Nenhum comando novo. `snapg list` continua como está.

---

## 1. Problema

Estado medido em 2026-11-12 nesta máquina:

- **60 backups órfãos** no top-level: 23 `@`, 20 `@home`, 17 `@root`. 3 com
  hora local `YYYY-MM-DD_HH:MM:SS` (formato de `now_local_label()` e do
  btrfs-assistant), 57 em ISO UTC com milissegundos
  (`2026-11-08T03:03:12.448Z`), provavelmente do restore do
  limine-snapper-sync (não confirmado). 55 têm mais de 90 dias, 5 são da
  última semana. Todos são filhos diretos do top-level, nenhum tem subvolume
  aninhado, nenhum é citado por `limine.conf` ou `snapshots.json`; o default é
  o top-level (ID 5).
- **61 checkpoints**, dos quais 44 com o nome padrão `Checkpoint …` e 17
  nomeados. `KEEP_GROUPS=0` (ilimitado), porque a regra atual apagaria os
  nomeados junto.

O snapgroup não cria `_backup_` (usa `_snapg_regret`/`_undo`/`_discard`, que já
têm ciclo de vida). Os órfãos ficam fora do Snapper: nem `KEEP_GROUPS` nem a
lixeira os alcançam. E não há como saber quanto espaço qualquer item prende.

## 2. Backups órfãos

### 2.1. Descoberta

1. Montar o top-level com `btrfs::mount_toplevel` (`/run/snapgroup`, `0700`,
   `nosuid,nodev`), como o restore.
2. Bases conhecidas: a base do mountpoint de cada config do Snapper (`@`,
   `@home`, `@root`), pelo resolvedor do restore.
3. Candidato: entrada direta do top-level `<base>_backup_<label>` com `<base>`
   conhecida. Prefixos desconhecidos ficam de fora.
4. Data: a do label. ISO UTC (`…T12:28:04.367Z`, restore do Limine) ou hora
   local `YYYY-MM-DD_HH:MM:SS` (snapg antigo, btrfs-assistant). Label que não
   é data cai na hora de criação gravada pelo btrfs. *Mudou do plano:* o
   restore do Limine grava cada base com segundos de diferença, e a data de
   criação do btrfs não juntava os membros de um mesmo restore.
5. Agrupamento: backups cujas datas ficam a até 60 s uma da outra
   (`SAME_RESTORE_SECS`) viram uma linha, com as bases em ordem alfabética.
   Na máquina, 60 subvolumes viraram 22 linhas.

### 2.2. Proteções

O candidato não aparece (motivo em `--verbose`) se:

- é o subvolume default (`btrfs subvolume get-default`);
- está montado (fonte `[/<nome>]` em `/proc/self/mountinfo`);
- é citado em `limine.conf` ou em algum `snapshots.json` sob `/boot`;
- contém subvolume aninhado com dados (`delete_subvolume_tree` já recusa;
  filtrar antes evita falha no meio do lote).

Com restore pendente, `delete` já recusa (gate existente). Tudo sob o lock
global.

## 3. Espaço no `delete`

### 3.1. Tela

```
 ▪ Espaço
   ██▉░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░
   libera 17.4 GiB agora · +4.8 GiB de metadados ficam reservados
   0 de 64 checkpoints → lixeira (15 dias) · 21 de 21 backups → agora (57 subvolumes)

 ▪ Checkpoints
         Nome                                   Kernel             Data
   › □       Checkpoint 2026-11-12 00:06        7.3.3-1-cachyos    2026-11-12 00:06
     ■ fixo  Antes do Estudo para enem          7.3.3-1-cachyos    2026-11-09 16:12

 ▪ Backups órfãos restos de restores fora do Snapper, sem lixeira
         Criado             Subvolumes      Origem
     ■       2026-05-09 08:28   @ @home @root   restore do Limine

   espaço marca · o órfãos · a automáticos · f fixos · t tudo · enter revisa · esc sai
```

- **Uma lista só** com checkpoints e backups, em seções. Atalhos marcam um
  tipo inteiro (`o`, `a`, `f`) ou tudo (`t`).
- **Barra:** escala = dados de todos os snapshots lidos. Três partes, cada uma
  sobre as anteriores (somam o total da seleção): amarelo = backups, sai agora;
  azul-claro = checkpoints automáticos, saem no purge da lixeira; rosa (cor
  211) = fixos, também no purge. Com `--purge`, tudo sai agora.
- **Linhas:** `□` desmarcado; marcado, `■` e o nome na cor da sua parte da
  barra, resto da linha em negrito. O fixo é dito pelo rótulo `fixo`, sempre
  apagado. *Mudou do plano:* sem barra por linha (poluía a lista), sem 📌
  (largura incerta, §8.3) e sem `[x]`. Os outros pickers (lixeira, restore,
  Regret) usam o mesmo `■`/`□`, sem cor.

### 3.2. Medição

*Mudou do plano:* `btrfs filesystem du` só dá o exclusivo de cada item e
sub-conta seleções (blocos compartilhados só entre itens marcados). A medição
lê os metadados do btrfs direto do dispositivo, só leitura (`src/space.rs`):

- superbloco, chunk tree, root tree e as fs trees, cada bloco uma vez,
  validando checksum, fsid, bytenr, geração e nível; leitura velha por escrita
  concorrente repete até 3 vezes.
- Para cada extent de dados e bloco de metadados, o conjunto de árvores que o
  referencia. `freed(ids)` soma o que só as árvores escolhidas referenciam:
  número exato para qualquer seleção, sem `≥`.
- Medido aqui: 6 s com 32 threads, 459 mil blocos, 3.93 M extents.
- A leitura roda numa thread ao abrir a tela; a barra mostra `calculando…`
  até ela terminar. Marcar só soma bitsets já prontos.
- **Só dados contam como liberados.** O statvfs (o `df`) só sobe com dados; o
  metadado liberado fica reservado nos chunks de metadados até um block group
  esvaziar ou um balance. Por isso a barra e o "libera" são de dados, e os
  metadados aparecem à parte.
- Depois de apagar: `btrfs subvolume sync`, `btrfs filesystem sync` (os
  extents soltos só voltam ao livre quando a transação é gravada) e statvfs
  antes/depois: `liberados 3.8 GiB · livre 107.4 → 111.2 GiB · previsto 3.8
  GiB + 353 MiB de metadados (ficam reservados)`.
- Verificação: teste `#[ignore]` em btrfs real confere a previsão contra o
  `Data, Used` e contra o statvfs, byte a byte; na máquina, dois backups
  apagados bateram com a previsão.

### 3.3. Revisão

A revisão irreversível atual ganha os backups marcados numa seção à parte, com
os nomes completos dos subvolumes e "apagados agora, sem lixeira". `delete -y`
continua operando só no checkpoint mais recente.

## 4. Retenção por intenção (checkpoints)

### 4.1. Marca no save

O `save` grava `snapgroup-auto=1` no userdata quando o nome é o padrão:

| Origem | Resultado |
|---|---|
| `snapg save` + Enter (aceita o placeholder) | automático |
| `snapg save` sem TTY e sem nome (script/cron) | automático |
| `snapg save` com nome digitado ou `snapg save "nome"` | fixo |
| `snapg rename` em automático | vira fixo (remove a marca) |
| Guardar o Regret como checkpoint | fixo |
| Futuro hook do pacman (`TODO.md` §4) | automático |

Texto digitado igual ao placeholder conta como automático. O prompt do `save`
avisa: `Enter = automático (entra na retenção) · digite um nome para fixar`.

**Grupos antigos, sem a marca:** automático só se a descrição bate exatamente
com o que `default_checkpoint_name` gera (`Checkpoint YYYY-MM-DD HH:MM` ou
`Checkpoint <epoch>`). Qualquer outra coisa é fixa. Hoje isso classifica os 44
`Checkpoint …` como automáticos.

### 4.2. Regra

- `KEEP_GROUPS` passa a contar **só automáticos**: mantém os N mais novos;
  o excedente vai para a lixeira, como hoje, e sai após `TRASH_PRUNE_DAYS`.
- Fixos nunca entram na conta nem saem sozinhos; saem só pelo `delete`.
- O Regret ativo continua fora da retenção, como hoje.
- **Mudança de comportamento:** quem tem `KEEP_GROUPS>0` hoje deixa de perder
  checkpoints nomeados. Vai no CHANGELOG como `Fix`/`Feature`.

O risco é o Enter apressado num checkpoint que importava. A proteção é a
soma do aviso no prompt, da lixeira de 15 dias e do `rename`, que fixa com um
gesto.

## 5. Purge de backups órfãos

- Opção `BACKUP_PRUNE_DAYS` em `/etc/snapgroup.conf`, padrão `0` (desligado):
  os backups são de outra ferramenta, apagar sozinho precisa ser escolha do
  usuário.
- Roda junto do purge da lixeira (início de `save` e `delete`), sob o lock
  global. Sem timer.
- Sai o backup com mais de N dias (data de criação do btrfs) que passa nas
  proteções de §2.2, **exceto o mais recente de cada base**, que pode ser o
  único jeito de desfazer o último restore feito pelo Limine.
- Sem lixeira: a idade já é a carência.
- Uma linha de saída: `purge: 55 backups órfãos com mais de 30 dias apagados`.
  Falha numa deleção é mostrada e o purge segue, como na lixeira.
- Com a opção ligada, o `delete` mostra `expira em Nd` nos backups.
- Questão em aberto (§8): confirmar quando um purge apagaria muitos de uma vez.

## 6. `doctor`

Linha informativa, sem montar o top-level para medir espaço:
`N backups órfãos · snapg delete`. Contagem por `btrfs subvolume list`, que já
lê o top-level sem montá-lo.

## 7. Implementação estimada

- `src/btrfs.rs`: `default_subvol_id`, `exclusive_bytes`, fontes montadas,
  listagem dos órfãos.
- `src/commands.rs`: `delete` com órfãos, purge de backups, `save`/`rename` com
  a marca, `try_cleanup` filtrando automáticos.
- `src/group.rs`: `is_auto(group)` (marca ou nome padrão exato).
- `src/config.rs`: `BACKUP_PRUNE_DAYS`, mesmo parser e clamp de
  `TRASH_PRUNE_DAYS`.
- `src/ui/`: barra do recuperável, barra por linha, seção de órfãos, prompt do
  `save`, revisão.
- Testes puros: parse/agrupamento de nomes, `is_auto` (marca, nome padrão,
  nome parecido), `groups_to_prune` só com automáticos, elegibilidade do purge
  (mais recente por base preservado), proteções sobre listas fixas, escala das
  barras.
- Teste `#[ignore]` em btrfs real (`SNAPG_TEST_BTRFS`): backup montado e
  default não aparecem; os demais são apagados; o mais recente sobrevive ao
  purge.

Ordem sugerida, cada uma uma branch: (1) retenção por intenção, menor e
independente; (2) órfãos no `delete` sem espaço; (3) medição e barras;
(4) purge de backups.

## 8. Questões em aberto

1. Purge de backups pede confirmação quando apagaria mais de N (ex.: 10) de
   uma vez, ou ligar a opção já é a confirmação?
2. Incluir os `_snapg_*` que sobram de falhas (hoje do `boot-clean` e do
   `doctor`) na seção de órfãos?
3. ~~Sinal de fixo: 📌 ou texto?~~ Resolvida: texto `fixo`.
