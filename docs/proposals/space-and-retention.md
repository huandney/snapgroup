# Proposta: espaço e retenção no `snapg delete`

- **Status:** proposta, não implementada.
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
4. Data: a de criação gravada pelo btrfs (`btrfs::subvol_creation_time`). O
   label só serve para exibição e agrupamento; não é preciso interpretar dois
   formatos de data, um deles em UTC.
5. Agrupamento: o mesmo `<label>` em várias bases (o mesmo restore) vira uma
   linha. Labels diferentes por milissegundos ficam separados, sem heurística.

### 2.2. Proteções

O candidato não aparece (motivo em `--verbose`) se:

- é o subvolume default (`btrfs subvolume get-default`);
- está montado (fonte `[/<nome>]` em `/proc/self/mountinfo`);
- é citado em `limine.conf` ou em algum `snapshots.json` sob `/boot`;
- contém subvolume aninhado com dados (`delete_subvolume_tree` já recusa;
  filtrar antes evita falha no meio do lote).

Com restore pendente, `delete` já recusa (gate existente). Tudo sob o lock
global.

## 3. Espaço no picker (tela A+B)

### 3.1. Tela

```
 SnapGroup 0.12.0-beta

 ▪ Recuperável  ≥ 31.4 GiB em checkpoints e backups
   ███████████▊█████▌░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░
   ■ agora ≥ 12.8 GiB   ■ no purge ≥ 5.6 GiB   disco 412/931 GiB

 ▪ Checkpoints  vão para a lixeira, liberam no purge (15 dias)
       Nome                            Kernel Data                Exclusivo
   [ ] 📌 Teste restore                 7.3.3  2026-11-11 17:47     ≥ 410 MiB  █▌
   [x]    Checkpoint 2026-11-06 07:30   7.3.1  2026-11-06 07:30     ≥ 3.2 GiB  ████▋
   [ ]    Checkpoint 2026-11-04 15:34   7.3.1  2026-11-04 15:34     ≥ 922 MiB  █▎   sai em 3d

 ▪ Backups órfãos  apagados na hora, sem lixeira
       Data              Membros                              Exclusivo
   [x] 2026-04-26 20:58  @ @home @root                       ≥ 8.2 GiB  ████████████
   [ ] 2026-11-08 03:03  @home                               ≥ 307 MiB  ▍

   espaço marca · a marca órfãos · enter revisa · q sai
```

- **Barra do topo:** escala = total recuperável (soma dos exclusivos de tudo
  listado), não o disco. Num disco de 931 GiB, 30 GiB de snapshots seriam
  invisíveis. O disco aparece só como números.
- **Duas cores:** laranja = sai agora (backups), azul = sai no purge da
  lixeira (checkpoints). Sem isso a barra promete espaço que não aparece já.
- **Barra por linha:** escala = maior item listado. Mostra qual item pesa.
  Abaixo de 80 colunas as barras das linhas somem; a do topo fica, encolhida.
- **Fixos e automáticos:** 📌 marca checkpoint fixo (§4); automático perto de
  sair mostra `sai em Nd`.

### 3.2. Medição

- Exclusivo por item via `btrfs filesystem du -s --raw` no caminho do
  top-level, numa thread de fundo ao abrir o picker. A lista aparece na hora;
  linha não medida mostra `medindo` e o total cresce até `medindo N/M` sumir.
- Marcar/desmarcar só soma números já medidos: atualização instantânea, sem
  I/O na thread da UI.
- **Todo número é mínimo (`≥`).** Blocos compartilhados só entre itens marcados
  (checkpoints vizinhos) também são liberados, mas o btrfs não os atribui sem
  qgroups, e ligar quota deixa deleção em lote e commits lentos. Checkpoints
  consecutivos podem liberar bem mais que a soma.
- Depois de apagar: `btrfs subvolume sync` e espaço livre antes/depois via
  `btrfs filesystem usage`, como confirmação real.

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
3. Sinal de fixo: 📌 ou texto (`fixo`)? Emoji tem largura incerta em alguns
   terminais e quebraria o alinhamento das colunas.
