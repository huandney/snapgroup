# Proposta: limpeza de backups órfãos (`snapg clean`)

- **Status:** proposta, não implementada.
- **Data:** 2026-11-12.
- **Relacionado:** `docs/architecture/restore-highlander.md` §5.2 (naming
  `_backup_`), backlog local de retenção de checkpoints (`TODO.md`), que esta
  proposta não substitui.

## 0. TL;DR

Subvolumes `<base>_backup_<timestamp>` deixados por restores de outras
ferramentas (e por versões antigas do snapg) acumulam no top-level sem que nada
os expire. `snapg clean` lista esses órfãos com data e espaço exclusivo, o
usuário escolhe quais apagar, revisa e confirma. Nada é apagado
automaticamente.

---

## 1. Problema

Estado medido em 2026-11-12 nesta máquina:

- 60 subvolumes `_backup_` no top-level: 23 `@`, 20 `@home`, 17 `@root`.
- Dois formatos de timestamp:
  - 3 em hora local `YYYY-MM-DD_HH:MM:SS` (`2026-04-26_20:58:44`), o formato
    de `now_local_label()` e do btrfs-assistant;
  - 57 em ISO UTC com milissegundos (`2026-11-08T03:03:12.448Z`), provavelmente
    do restore do limine-snapper-sync (não confirmado). O mais recente é de
    2026-11-08.
- Todos são filhos diretos do top-level (ID 5) e nenhum tem subvolume aninhado.
- Nenhum é referenciado por `/boot/limine.conf` nem por `snapshots.json` do
  limine-snapper-sync.
- O subvolume default é o top-level (ID 5), não um backup.

O snapgroup atual não cria esses nomes: o restore usa `_snapg_regret`,
`_snapg_undo` e `_snapg_discard`, que já têm ciclo de vida próprio
(`boot-clean`, Regret). Os `_backup_` ficam fora do Snapper, então nem
`KEEP_GROUPS` nem a lixeira os alcançam. Cada um prende os blocos que
divergiram do sistema atual desde a sua criação.

## 2. Escopo

Dentro:

- descobrir backups órfãos das bases das configs do Snapper;
- mostrar data e espaço exclusivo de cada um;
- apagar os escolhidos com revisão e confirmação.

Fora:

- apagar sem pedido (sem timer, sem hook, sem opção de config);
- impedir que outras ferramentas criem backups;
- retenção dos checkpoints do Snapper (fica no backlog do `TODO.md`).

## 3. Descoberta

1. Montar o top-level com `btrfs::mount_toplevel` (mesmo caminho do restore,
   em `/run/snapgroup`, `nosuid,nodev`).
2. Bases conhecidas: para cada config do Snapper, a base do mountpoint
   (`@`, `@home`, `@root`), pelo mesmo resolvedor usado no restore.
3. Candidato: entrada direta do top-level cujo nome é
   `<base>_backup_<label>`, com `<base>` em uma das bases conhecidas. Prefixos
   desconhecidos não entram: o comando só fala do que o snapgroup gerencia.
4. Data: parse de `<label>` nos dois formatos acima. Label que não parseia usa a
   data de criação do subvolume (`btrfs::subvol_creation_time`) e aparece
   marcado como tal.
5. Agrupamento: backups com o mesmo `<label>` (o mesmo restore, em `@`, `@home`
   e `@root`) viram uma linha só, como os grupos de checkpoint. Labels
   diferentes por milissegundos ficam em linhas separadas, sem heurística de
   proximidade.

## 4. O que nunca é apagado

O candidato some da lista, com o motivo exibido em `--verbose`, se:

- é o subvolume default (`btrfs subvolume get-default`);
- está montado em qualquer lugar (fonte `[/<nome>]` em `/proc/self/mountinfo`);
- é citado em `limine.conf` ou em algum `snapshots.json` sob `/boot`;
- contém subvolume aninhado com dados (`delete_subvolume_tree` já recusa;
  detectar antes evita falha no meio do lote).

O comando inteiro recusa enquanto houver restore pendente (o mesmo gate de
`pending_check`), porque o swap ainda não foi validado por um boot. Também roda
sob o lock global, como `delete` e `trash`.

## 5. Espaço

- Exclusivo por backup via `btrfs filesystem du -s --raw` no caminho do top-level,
  calculado sob demanda depois que a lista aparece, com o spinner de I/O
  existente. Sem qgroups: ligar quota custa caro em deleções em lote.
- Rótulo honesto: o exclusivo de cada backup é um **mínimo**. Blocos
  compartilhados só entre backups (dois backups do mesmo período) só são
  liberados quando todos saem; a soma não promete o total.
- Depois da deleção, mostrar o espaço livre antes/depois (`btrfs filesystem
  usage`) após `btrfs subvolume sync`, que espera o cleaner terminar.

## 6. Interface

```
$ sudo snapg clean
 ▪ Backups órfãos
   Data               Membros                 Exclusivo
   2026-04-26 20:58   [@] [@home] [@root]      3.1 GiB
   2026-05-16 19:55   [@root]                  12 MiB
   …
```

- Picker multi-seleção, mais antigo primeiro, reaproveitando o layout tabular
  dos pickers de checkpoint.
- Revisão irreversível igual à do `delete`/purge da lixeira: lista final com
  nomes completos dos subvolumes e o total mínimo.
- `snapg doctor` ganha uma linha informativa, sem ação:
  `N backups órfãos (≥ X GiB) · snapg clean`.

## 7. Implementação estimada

- `src/commands.rs`: `clean` (montar, descobrir, filtrar, picker, revisão,
  deletar, desmontar). Reusa `delete_subvolume_tree` e o gate de pendência.
- `src/btrfs.rs`: `default_subvol_id`, `exclusive_bytes`, leitura das fontes
  montadas.
- `src/ui/`: tabela e revisão, a partir das do `delete`.
- Funções puras testáveis: parse de nome/label, agrupamento, filtros de
  proteção sobre listas fixas.
- Teste `#[ignore]` em btrfs real (imagem de loopback, `SNAPG_TEST_BTRFS`):
  backup montado e backup default não aparecem; os demais são apagados.

## 8. Questões em aberto

1. Nome: `snapg clean` ou `snapg backups` (com `list`/`delete`)?
2. A linha no `doctor` vale o custo de montar o top-level a cada diagnóstico,
   ou só conta nomes, sem calcular espaço?
3. Incluir também `_snapg_*` que sobraram de falhas (hoje tratados pelo
   `boot-clean` e pelo `doctor`), ou manter separado?
